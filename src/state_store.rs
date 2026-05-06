//! State Store - Content-Addressed Storage
//!
//! Provides distributed, deduplicated storage for VM state snapshots.
//! Uses SHA256 content-addressing for automatic deduplication.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐
//! │ State Object    │ ────► SHA256 hash ────► Content Address
//! │ - CPU State     │
//! │ - Memory Pages  │
//! │ - Metadata      │
//! └─────────────────┘
//!        │
//!        ▼
//! ┌─────────────────┐
//! │ Object Store    │ (S3-compatible, local fs, or in-memory)
//! │ - Deduplication │
//! │ - TTL Management│
//! │ - GC            │
//! └─────────────────┘
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{debug, info, warn, error};
use sha2::{Digest, Sha256};
use hex::Encode;
use reqwest::Client;

use crate::model::{State, StateId, StateMetadata};
use crate::memory::MemoryPage;

/// Unique content address (SHA256 hash)
pub type ContentAddress = String;

/// State store configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateStoreConfig {
    /// Storage backend type
    pub backend: StorageBackend,
    /// Base directory for local storage
    pub storage_path: PathBuf,
    /// Cache size limit in bytes
    pub cache_size_limit: usize,
    /// Default TTL for state objects (hours)
    pub default_ttl_hours: u64,
    /// Enable garbage collection
    pub enable_gc: bool,
    /// GC interval (seconds)
    pub gc_interval_secs: u64,
}

impl Default for StateStoreConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::Local,
            storage_path: PathBuf::from("/tmp/isa-state-store"),
            cache_size_limit: 1024 * 1024 * 1024, // 1GB
            default_ttl_hours: 24,
            enable_gc: true,
            gc_interval_secs: 3600,
        }
    }
}

/// Storage backend type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend {
    /// Local filesystem storage
    Local,
    /// In-memory storage (for testing)
    Memory,
    /// S3-compatible object store
    S3,
}

/// A stored state object with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredState {
    /// Content-addressed state ID
    pub state_id: StateId,
    /// Content hash of the state
    pub content_hash: ContentAddress,
    /// Reference count for GC
    pub ref_count: usize,
    /// Creation time
    pub created_at: DateTime<Utc>,
    /// Expiration time (None = permanent)
    pub expires_at: Option<DateTime<Utc>>,
    /// State size in bytes
    pub size_bytes: usize,
    /// Metadata
    pub metadata: StateMetadata,
}

/// High-level state store interface
pub struct StateStore {
    config: StateStoreConfig,
    /// In-memory index of stored states
    index: Arc<RwLock<HashMap<StateId, StoredState>>>,
    /// Content-addressed page cache
    page_cache: Arc<RwLock<HashMap<ContentAddress, Vec<u8>>>>,
    /// Storage backend
    backend: Arc<dyn StorageBackendTrait + Send + Sync>,
    /// GC running flag
    gc_running: Arc<std::sync::atomic::AtomicBool>,
}

impl StateStore {
    /// Create a new state store
    pub async fn new(config: StateStoreConfig) -> Result<Self, StateStoreError> {
        let backend: Arc<dyn StorageBackendTrait + Send + Sync> = match config.backend {
            StorageBackend::Local => {
                // Create storage directory if needed
                fs::create_dir_all(&config.storage_path).await.map_err(|e| {
                    StateStoreError::IoError(format!("Failed to create storage dir: {}", e))
                })?;
                Arc::new(LocalBackend::new(config.storage_path.clone()))
            }
            StorageBackend::Memory => {
                Arc::new(MemoryBackend::new())
            }
            StorageBackend::S3 => {
                // S3-compatible backend using reqwest HTTP client
                Arc::new(S3Backend::new(config.storage_path.clone()))
            }
        };

        let store = Self {
            config,
            index: Arc::new(RwLock::new(HashMap::new())),
            page_cache: Arc::new(RwLock::new(HashMap::new())),
            backend,
            gc_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        // Start GC if enabled
        if store.config.enable_gc {
            store.start_gc_task();
        }

        Ok(store)
    }

    /// Store a complete state object
    pub async fn store_state(&self, state: State) -> Result<StoredState, StateStoreError> {
        info!(label = %state.metadata.label, "Storing state");

        // Serialize state to bytes
        let state_bytes = serde_json::to_vec(&state)
            .map_err(|e| StateStoreError::SerializationError(format!("Failed to serialize: {}", e)))?;

        // Calculate content hash
        let content_hash = compute_hash(&state_bytes);

        // Store in backend
        self.backend.put(&content_hash, &state_bytes).await?;

        // Calculate expiration
        let expires_at = state.metadata.ttl_seconds.map(|ttl| {
            Utc::now() + chrono::Duration::seconds(ttl as i64)
        });

        let stored = StoredState {
            state_id: state.state_id.clone(),
            content_hash: content_hash.clone(),
            ref_count: 1,
            created_at: Utc::now(),
            expires_at,
            size_bytes: state_bytes.len(),
            metadata: state.metadata.clone(),
        };

        // Update index
        {
            let mut index = self.index.write().await;
            index.insert(state.state_id.clone(), stored.clone());
        }

        // Cache the content
        {
            let mut cache = self.page_cache.write().await;
            if cache.len() < self.config.cache_size_limit {
                cache.insert(content_hash, state_bytes);
            }
        }

        info!(
            state_id = %state.state_id.0,
            content_hash = %content_hash,
            size = state_bytes.len(),
            "State stored successfully"
        );

        Ok(stored)
    }

    /// Retrieve a state object by ID
    pub async fn get_state(&self, state_id: &StateId) -> Result<State, StateStoreError> {
        let index = self.index.read().await;
        let stored = index.get(state_id)
            .ok_or_else(|| StateStoreError::NotFound(format!("State {} not found", state_id.0)))?;

        // Try cache first
        let content = {
            let cache = self.page_cache.read().await;
            cache.get(&stored.content_hash).cloned()
        };

        let content = match content {
            Some(c) => c,
            None => {
                // Fetch from backend
                let content = self.backend.get(&stored.content_hash).await?;
                
                // Cache it
                let mut cache = self.page_cache.write().await;
                if cache.len() < self.config.cache_size_limit {
                    cache.insert(stored.content_hash.clone(), content.clone());
                }
                content
            }
        };

        // Deserialize
        let state: State = serde_json::from_slice(&content)
            .map_err(|e| StateStoreError::SerializationError(format!("Failed to deserialize: {}", e)))?;

        Ok(state)
    }

    /// Delete a state object
    pub async fn delete_state(&self, state_id: &StateId) -> Result<(), StateStoreError> {
        let mut index = self.index.write().await;
        
        if let Some(stored) = index.remove(state_id) {
            // Decrement ref count or delete
            if stored.ref_count <= 1 {
                // Remove from backend
                self.backend.delete(&stored.content_hash).await?;
                
                // Remove from cache
                let mut cache = self.page_cache.write().await;
                cache.remove(&stored.content_hash);
                
                info!(state_id = %state_id.0, "State deleted");
            } else {
                // Just decrement ref count
                index.insert(state_id.clone(), StoredState {
                    ref_count: stored.ref_count - 1,
                    ..stored
                });
            }
        }

        Ok(())
    }

    /// Check if a state exists
    pub async fn has_state(&self, state_id: &StateId) -> bool {
        let index = self.index.read().await;
        index.contains_key(state_id)
    }

    /// List all stored states
    pub async fn list_states(&self) -> Vec<StoredState> {
        let index = self.index.read().await;
        index.values().cloned().collect()
    }

    /// Increment reference count for a state
    pub async fn increment_ref(&self, state_id: &StateId) -> Result<(), StateStoreError> {
        let mut index = self.index.write().await;
        
        if let Some(stored) = index.get_mut(state_id) {
            stored.ref_count += 1;
            Ok(())
        } else {
            Err(StateStoreError::NotFound(format!("State {} not found", state_id.0)))
        }
    }

    /// Decrement reference count for a state
    pub async fn decrement_ref(&self, state_id: &StateId) -> Result<(), StateStoreError> {
        let mut index = self.index.write().await;
        
        if let Some(stored) = index.get_mut(state_id) {
            if stored.ref_count > 0 {
                stored.ref_count -= 1;
            }
            Ok(())
        } else {
            Err(StateStoreError::NotFound(format!("State {} not found", state_id.0)))
        }
    }

    /// Store a memory page (content-addressed)
    pub async fn store_page(&self, page: &MemoryPage) -> Result<ContentAddress, StateStoreError> {
        // Use the page's existing hash as content address
        let content_hash = &page.page_hash;
        
        // Store in backend
        self.backend.put(content_hash, &page.data).await?;
        
        // Cache it
        {
            let mut cache = self.page_cache.write().await;
            if cache.len() < self.config.cache_size_limit {
                cache.insert(content_hash.clone(), page.data.clone());
            }
        }

        Ok(content_hash.clone())
    }

    /// Retrieve a memory page by content address
    pub async fn get_page(&self, content_hash: &str) -> Result<Vec<u8>, StateStoreError> {
        // Try cache first
        let content = {
            let cache = self.page_cache.read().await;
            cache.get(content_hash).cloned()
        };

        match content {
            Some(c) => Ok(c),
            None => {
                let content = self.backend.get(content_hash).await?;
                
                // Cache it
                let mut cache = self.page_cache.write().await;
                if cache.len() < self.config.cache_size_limit {
                    cache.insert(content_hash.to_string(), content.clone());
                }
                Ok(content)
            }
        }
    }

    /// Start background GC task
    fn start_gc_task(&self) {
        let index = self.index.clone();
        let backend = self.backend.clone();
        let running = self.gc_running.clone();
        let interval = self.config.gc_interval_secs;

        running.store(true, std::sync::atomic::Ordering::Relaxed);

        tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(tokio::time::Duration::from_secs(interval));
            
            while running.load(std::sync::atomic::Ordering::Relaxed) {
                interval_timer.tick().await;
                
                let now = Utc::now();
                let mut expired_ids = Vec::new();

                {
                    let index_read = index.read().await;
                    for (id, stored) in index_read.iter() {
                        if let Some(expires_at) = &stored.expires_at {
                            if &now > expires_at && stored.ref_count == 0 {
                                expired_ids.push(id.clone());
                            }
                        }
                    }
                }

                // Delete expired states
                for id in expired_ids {
                    let mut index_write = index.write().await;
                    if let Some(stored) = index_write.remove(&id) {
                        if let Err(e) = backend.delete(&stored.content_hash).await {
                            error!("GC failed to delete {}: {}", id.0, e);
                        } else {
                            info!("GC deleted expired state {}", id.0);
                        }
                    }
                }
            }
        });
    }

    /// Stop GC task
    pub fn stop_gc(&self) {
        self.gc_running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for StateStore {
    fn drop(&mut self) {
        self.stop_gc();
    }
}

/// Storage backend trait
#[async_trait::async_trait]
pub trait StorageBackendTrait {
    async fn put(&self, key: &str, data: &[u8]) -> Result<(), StateStoreError>;
    async fn get(&self, key: &str) -> Result<Vec<u8>, StateStoreError>;
    async fn delete(&self, key: &str) -> Result<(), StateStoreError>;
    async fn exists(&self, key: &str) -> Result<bool, StateStoreError>;
}

/// Local filesystem backend
pub struct LocalBackend {
    base_path: PathBuf,
}

impl LocalBackend {
    fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    fn key_to_path(&self, key: &str) -> PathBuf {
        // Use first 2 chars as directory for sharding
        if key.len() >= 2 {
            let dir = &key[..2];
            self.base_path.join(dir).join(key)
        } else {
            self.base_path.join(key)
        }
    }
}

#[async_trait::async_trait]
impl StorageBackendTrait for LocalBackend {
    async fn put(&self, key: &str, data: &[u8]) -> Result<(), StateStoreError> {
        let path = self.key_to_path(key);
        
        // Create directory if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                StateStoreError::IoError(format!("Failed to create dir: {}", e))
            })?;
        }

        fs::write(&path, data).await.map_err(|e| {
            StateStoreError::IoError(format!("Failed to write {}: {}", path.display(), e))
        })?;

        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StateStoreError> {
        let path = self.key_to_path(key);
        
        fs::read(&path).await.map_err(|e| {
            StateStoreError::IoError(format!("Failed to read {}: {}", path.display(), e))
        })
    }

    async fn delete(&self, key: &str) -> Result<(), StateStoreError> {
        let path = self.key_to_path(key);
        
        fs::remove_file(&path).await.map_err(|e| {
            StateStoreError::IoError(format!("Failed to delete {}: {}", path.display(), e))
        })
    }

    async fn exists(&self, key: &str) -> Result<bool, StateStoreError> {
        let path = self.key_to_path(key);
        Ok(fs::metadata(&path).await.is_ok())
    }
}

/// In-memory backend (for testing)
pub struct MemoryBackend {
    store: Arc<RwLock<HashMap<String, Vec<u8>>>>,
}

impl MemoryBackend {
    fn new() -> Self {
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl StorageBackendTrait for MemoryBackend {
    async fn put(&self, key: &str, data: &[u8]) -> Result<(), StateStoreError> {
        let mut store = self.store.write().await;
        store.insert(key.to_string(), data.to_vec());
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StateStoreError> {
        let store = self.store.read().await;
        store.get(key)
            .cloned()
            .ok_or_else(|| StateStoreError::NotFound(format!("Key {} not found", key)))
    }

    async fn delete(&self, key: &str) -> Result<(), StateStoreError> {
        let mut store = self.store.write().await;
        store.remove(key);
        Ok(())
    }

    async fn exists(&self, key: &str) -> Result<bool, StateStoreError> {
        let store = self.store.read().await;
        Ok(store.contains_key(key))
    }
}

/// S3-compatible backend using HTTP PUT/GET
/// Compatible with AWS S3, MinIO, and other S3-compatible stores
pub struct S3Backend {
    /// Base URL (e.g., "http://localhost:9000/bucket-name")
    base_url: String,
    /// Bucket name
    bucket: String,
    /// Access key ID (for AWS)
    access_key: Option<String>,
    /// Secret access key (for AWS)
    secret_key: Option<String>,
    /// Region (for AWS)
    region: String,
    /// HTTP client with optional auth headers
    client: Client,
}

/// S3 configuration
#[derive(Debug, Clone)]
pub struct S3Config {
    /// Endpoint URL (e.g., "https://s3.us-west-2.amazonaws.com" or "http://localhost:9000")
    pub endpoint: String,
    /// Bucket name
    pub bucket: String,
    /// Access key ID (optional for anonymous access)
    pub access_key: Option<String>,
    /// Secret access key
    pub secret_key: Option<String>,
    /// AWS region
    pub region: String,
    /// Use path-style URLs (true for MinIO, false for AWS)
    pub path_style: bool,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: std::env::var("ISA_S3_ENDPOINT").unwrap_or_else(|_| "".to_string()),
            bucket: std::env::var("ISA_S3_BUCKET").unwrap_or_else(|_| "isa-states".to_string()),
            access_key: std::env::var("ISA_S3_ACCESS_KEY").ok(),
            secret_key: std::env::var("ISA_S3_SECRET_KEY").ok(),
            region: std::env::var("ISA_S3_REGION").unwrap_or_else(|_| "us-west-2".to_string()),
            path_style: std::env::var("ISA_S3_PATH_STYLE").map(|v| v == "true").unwrap_or(false),
        }
    }
}

impl S3Backend {
    fn new(_base_path: PathBuf) -> Self {
        let config = S3Config::default();
        
        // Build base URL
        let base_url = if config.endpoint.is_empty() {
            // No endpoint configured, use a placeholder
            warn!("S3 endpoint not configured, using default");
            "http://localhost:9000".to_string()
        } else {
            config.endpoint.clone()
        };

        // Build client with optional authentication
        let client_builder = Client::builder();
        
        // Note: For production, add proper AWS SigV4 signing
        // This is a simplified implementation
        
        Self {
            base_url,
            bucket: config.bucket,
            access_key: config.access_key,
            secret_key: config.secret_key,
            region: config.region,
            client: client_builder.build().unwrap_or_else(|_| Client::new()),
        }
    }

    /// Create S3 backend with explicit configuration
    pub fn with_config(config: S3Config) -> Self {
        let client_builder = Client::builder();
        
        Self {
            base_url: config.endpoint,
            bucket: config.bucket,
            access_key: config.access_key,
            secret_key: config.secret_key,
            region: config.region,
            client: client_builder.build().unwrap_or_else(|_| Client::new()),
        }
    }

    fn key_to_url(&self, key: &str) -> String {
        // Use first 2 chars as directory for sharding
        let shard = if key.len() >= 2 { &key[..2] } else { "xx" };
        
        if self.base_url.ends_with('/') {
            format!("{}{}/{}/{}", self.base_url, self.bucket, shard, key)
        } else {
            format!("{}/{}/{}/{}", self.base_url, self.bucket, shard, key)
        }
    }
}

#[async_trait::async_trait]
impl StorageBackendTrait for S3Backend {
    async fn put(&self, key: &str, data: &[u8]) -> Result<(), StateStoreError> {
        let url = self.key_to_url(key);
        
        self.client.put(&url)
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| StateStoreError::IoError(format!("S3 PUT failed: {}", e)))?
            .error_for_status()
            .map_err(|e| StateStoreError::IoError(format!("S3 PUT error: {}", e)))?;
        
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StateStoreError> {
        let url = self.key_to_url(key);
        
        let response = self.client.get(&url)
            .send()
            .await
            .map_err(|e| StateStoreError::IoError(format!("S3 GET failed: {}", e)))?
            .error_for_status()
            .map_err(|e| StateStoreError::NotFound(format!("S3 object not found: {}", e)))?;
        
        let data = response.bytes()
            .await
            .map_err(|e| StateStoreError::IoError(format!("S3 GET read failed: {}", e)))?
            .to_vec();
        
        Ok(data)
    }

    async fn delete(&self, key: &str) -> Result<(), StateStoreError> {
        let url = self.key_to_url(key);
        
        self.client.delete(&url)
            .send()
            .await
            .map_err(|e| StateStoreError::IoError(format!("S3 DELETE failed: {}", e)))?
            .error_for_status()
            .map_err(|e| StateStoreError::IoError(format!("S3 DELETE error: {}", e)))?;
        
        Ok(())
    }

    async fn exists(&self, key: &str) -> Result<bool, StateStoreError> {
        let url = self.key_to_url(key);
        
        let response = self.client.head(&url)
            .send()
            .await
            .map_err(|e| StateStoreError::IoError(format!("S3 HEAD failed: {}", e)))?;
        
        Ok(response.status().is_success())
    }
}

/// Compute SHA256 hash of data
pub fn compute_hash(data: &[u8]) -> ContentAddress {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// State store errors
#[derive(Debug, thiserror::Error)]
pub enum StateStoreError {
    #[error("IO error: {0}")]
    IoError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Already exists: {0}")]
    AlreadyExists(String),

    #[error("Corrupted data: {0}")]
    Corrupted(String),
}

// Required for async_trait
use async_trait::async_trait;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{VMConfig, CPUState, MemoryFlags, MemoryTemperature, EventLog, UIStateRef};
    use uuid::Uuid;

    #[tokio::test]
    async fn test_state_store_memory_backend() {
        let config = StateStoreConfig {
            backend: StorageBackend::Memory,
            ..Default::default()
        };

        let store = StateStore::new(config).await.unwrap();

        // Create a test state
        let state = State {
            state_id: StateId(Uuid::new_v4().to_string()),
            vm_config: VMConfig {
                vcpus: 2,
                memory_mb: 512,
                kernel_image: "/tmp/vmlinux.bin".to_string(),
                rootfs_image: "/tmp/rootfs.ext4".to_string(),
            },
            cpu_state: CPUState {
                arch: "x86_64".to_string(),
                registers: serde_json::json!({}),
            },
            memory_manifest: vec![],
            fd_table: vec![],
            socket_table: vec![],
            device_state: vec![],
            deterministic_log: EventLog { events: vec![] },
            ui_state: UIStateRef { stream_id: Uuid::new_v4() },
            metadata: StateMetadata {
                label: "test-state".to_string(),
                created_at: Utc::now(),
                ttl_seconds: Some(3600),
            },
        };

        // Store state
        let stored = store.store_state(state.clone()).await.unwrap();
        assert_eq!(stored.state_id, state.state_id);

        // Retrieve state
        let retrieved = store.get_state(&state.state_id).await.unwrap();
        assert_eq!(retrieved.state_id, state.state_id);

        // Check existence
        assert!(store.has_state(&state.state_id).await);

        // List states
        let states = store.list_states().await;
        assert_eq!(states.len(), 1);
    }

    #[tokio::test]
    async fn test_content_deduplication() {
        let config = StateStoreConfig {
            backend: StorageBackend::Memory,
            ..Default::default()
        };

        let store = StateStore::new(config).await.unwrap();

        // Store same data twice
        let data1 = b"test data";
        let hash1 = compute_hash(data1);
        
        store.backend.put(&hash1, data1).await.unwrap();
        store.backend.put(&hash1, data1).await.unwrap();

        // Should only exist once in backend
        let retrieved = store.backend.get(&hash1).await.unwrap();
        assert_eq!(retrieved, data1);
    }

    #[test]
    fn test_hash_computation() {
        let data = b"hello world";
        let hash = compute_hash(data);
        
        // SHA256 produces 64 hex characters
        assert_eq!(hash.len(), 64);
        
        // Same data should produce same hash
        assert_eq!(hash, compute_hash(data));
        
        // Different data should produce different hash
        assert_ne!(hash, compute_hash(b"hello world!"));
    }
}
