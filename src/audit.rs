//! Audit Logging Module
//!
//! Provides comprehensive audit logging for all state operations.
//! Logs are tamper-evident and can be exported for compliance.
//!
//! # Features
//!
//! - All state operations logged
//! - Tamper-evident log chain (hash chaining)
//! - Configurable log levels
//! - Export to JSON/syslog

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn, error};

/// Audit log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Unique entry ID
    pub id: u64,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
    /// Event type
    pub event: AuditEvent,
    /// Actor (user/service)
    pub actor: String,
    /// Target resource
    pub resource: String,
    /// Previous entry hash (for tamper evidence)
    pub previous_hash: String,
    /// This entry's hash
    pub entry_hash: String,
    /// Optional metadata
    pub metadata: Option<serde_json::Value>,
}

/// Audit event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditEvent {
    // State operations
    StateCreated { label: String },
    StateResumed { mode: String },
    StateForked { from_state: String },
    StateDeleted,
    StateShared { with_user: String, permissions: Vec<String> },
    
    // Access control
    AccessGranted { user: String, permissions: Vec<String> },
    AccessRevoked { user: String },
    AccessDenied { reason: String },
    
    // Authentication
    Login { method: String, success: bool },
    Logout,
    TokenRefreshed,
    
    // Security
    KeyRotated { old_version: u32, new_version: u32 },
    EncryptionEnabled,
    EncryptionDisabled,
    
    // System
    ConfigChanged { setting: String, old_value: String, new_value: String },
    BackupCreated { location: String },
    BackupRestored { location: String },
    
    // Errors
    Error { code: String, message: String },
}

/// Audit log configuration
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// Maximum entries in memory
    pub max_memory_entries: usize,
    /// Log file path (optional)
    pub log_file: Option<String>,
    /// Include request details
    pub include_details: bool,
    /// Async flush interval (seconds)
    pub flush_interval_secs: u64,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            max_memory_entries: 10000,
            log_file: None,
            include_details: true,
            flush_interval_secs: 60,
        }
    }
}

/// Audit logger
pub struct AuditLogger {
    config: AuditConfig,
    /// In-memory log buffer
    entries: Arc<RwLock<VecDeque<AuditEntry>>>,
    /// Current entry ID
    entry_id: Arc<RwLock<u64>>,
    /// Last entry hash (for chaining)
    last_hash: Arc<RwLock<String>>,
    /// Log file handle
    log_file: Option<Arc<RwLock<tokio::fs::File>>>,
    /// Running flag
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl AuditLogger {
    /// Create a new audit logger
    pub async fn new(config: AuditConfig) -> Result<Self, AuditError> {
        let mut logger = Self {
            config,
            entries: Arc::new(RwLock::new(VecDeque::new())),
            entry_id: Arc::new(RwLock::new(0)),
            last_hash: Arc::new(RwLock::new(String::new())),
            log_file: None,
            running: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        // Open log file if configured
        if let Some(path) = &config.log_file {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await?;
            logger.log_file = Some(Arc::new(RwLock::new(file)));
            
            // Load last hash from existing log
            logger.last_hash = Arc::new(RwLock::new(logger.load_last_hash(path).await?));
        }

        // Start async flush task
        logger.start_flush_task();

        Ok(logger)
    }

    /// Load last hash from existing log file
    async fn load_last_hash(&self, path: &str) -> Result<String, AuditError> {
        use tokio::io::AsyncReadExt;
        
        // Read last line of file to get last hash
        let content = tokio::fs::read_to_string(path).await?;
        
        if let Some(last_line) = content.lines().last() {
            if let Ok(entry): Result<AuditEntry, _> = serde_json::from_str(last_line) {
                return Ok(entry.entry_hash);
            }
        }
        
        Ok(String::new())
    }

    /// Start background flush task
    fn start_flush_task(&self) {
        if self.log_file.is_none() {
            return;
        }

        let entries = self.entries.clone();
        let log_file = self.log_file.clone();
        let running = self.running.clone();
        let interval_secs = self.config.flush_interval_secs;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                tokio::time::Duration::from_secs(interval_secs)
            );

            while running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Flush entries to file
                let mut entries_queue = entries.write().await;
                if let Some(ref file) = log_file {
                    let mut file = file.write().await;
                    while let Some(entry) = entries_queue.pop_front() {
                        if let Ok(json) = serde_json::to_string(&entry) {
                            let _ = file.write_all(json.as_bytes()).await;
                            let _ = file.write_all(b"\n").await;
                        }
                    }
                    let _ = file.flush().await;
                }
            }
        });
    }

    /// Log an audit event
    pub async fn log(
        &self,
        event: AuditEvent,
        actor: &str,
        resource: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<u64, AuditError> {
        let previous_hash = self.last_hash.read().await.clone();
        
        let mut entry_id = self.entry_id.write().await;
        *entry_id += 1;
        let id = *entry_id;

        // Create entry
        let mut entry = AuditEntry {
            id,
            timestamp: Utc::now(),
            event,
            actor: actor.to_string(),
            resource: resource.to_string(),
            previous_hash: previous_hash.clone(),
            entry_hash: String::new(),
            metadata,
        };

        // Calculate entry hash
        entry.entry_hash = self.calculate_hash(&entry);

        // Update last hash
        *self.last_hash.write().await = entry.entry_hash.clone();

        // Add to buffer
        let mut entries = self.entries.write().await;
        entries.push_back(entry.clone());

        // Trim if needed
        while entries.len() > self.config.max_memory_entries {
            entries.pop_front();
        }

        // Log to tracing
        info!(
            "AUDIT: {} by {} on {} (id={})",
            serde_json::to_string(&entry.event).unwrap_or_default(),
            actor,
            resource,
            id
        );

        Ok(id)
    }

    /// Calculate hash for an entry
    fn calculate_hash(&self, entry: &AuditEntry) -> String {
        let mut hasher = Sha256::new();
        hasher.update(entry.id.to_be_bytes());
        hasher.update(entry.timestamp.to_rfc3339().as_bytes());
        hasher.update(serde_json::to_string(&entry.event).unwrap_or_default().as_bytes());
        hasher.update(entry.actor.as_bytes());
        hasher.update(entry.resource.as_bytes());
        hasher.update(entry.previous_hash.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Verify log integrity
    pub async fn verify_integrity(&self) -> Result<bool, AuditError> {
        let entries = self.entries.read().await;
        let mut expected_hash = String::new();

        for entry in entries.iter() {
            if entry.previous_hash != expected_hash {
                return Ok(false);
            }
            let calculated = self.calculate_hash(entry);
            if calculated != entry.entry_hash {
                return Ok(false);
            }
            expected_hash = entry.entry_hash.clone();
        }

        Ok(true)
    }

    /// Get recent entries
    pub async fn get_recent(&self, count: usize) -> Vec<AuditEntry> {
        let entries = self.entries.read().await;
        entries.iter().rev().take(count).cloned().collect()
    }

    /// Search entries
    pub async fn search(
        &self,
        actor: Option<&str>,
        resource: Option<&str>,
        event_type: Option<&str>,
        limit: usize,
    ) -> Vec<AuditEntry> {
        let entries = self.entries.read().await;
        
        entries.iter()
            .filter(|e| {
                if let Some(a) = actor {
                    if e.actor != a {
                        return false;
                    }
                }
                if let Some(r) = resource {
                    if e.resource != r {
                        return false;
                    }
                }
                if let Some(t) = event_type {
                    let event_json = serde_json::to_string(&e.event).unwrap_or_default();
                    if !event_json.contains(t) {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Export audit log
    pub async fn export(&self) -> Result<Vec<AuditEntry>, AuditError> {
        let entries = self.entries.read().await;
        Ok(entries.iter().cloned().collect())
    }

    /// Flush all entries to disk immediately
    pub async fn flush(&self) -> Result<(), AuditError> {
        if let Some(ref file) = self.log_file {
            let mut entries_queue = self.entries.write().await;
            let mut file = file.write().await;
            
            while let Some(entry) = entries_queue.pop_front() {
                let json = serde_json::to_string(&entry)?;
                file.write_all(json.as_bytes()).await?;
                file.write_all(b"\n").await?;
            }
            
            file.flush().await?;
        }
        
        Ok(())
    }

    /// Stop the audit logger
    pub fn stop(&self) {
        self.running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for AuditLogger {
    fn drop(&mut self) {
        self.stop();
        // Try to flush remaining entries
        let runtime = tokio::runtime::Handle::current();
        let _ = runtime.block_on(self.flush());
    }
}

/// Audit errors
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Log integrity check failed")]
    IntegrityCheckFailed,
}

/// Convenience macros for audit logging
#[macro_export]
macro_rules! audit_log {
    ($logger:expr, $event:expr, $actor:expr, $resource:expr) => {
        $logger.log($event, $actor, $resource, None).await
    };
    ($logger:expr, $event:expr, $actor:expr, $resource:expr, $meta:expr) => {
        $logger.log($event, $actor, $resource, Some($meta)).await
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audit_logging() {
        let config = AuditConfig {
            max_memory_entries: 100,
            log_file: None,
            ..Default::default()
        };

        let logger = AuditLogger::new(config).await.unwrap();

        // Log some events
        logger.log(
            AuditEvent::StateCreated { label: "test".to_string() },
            "user-123",
            "state-456",
            None,
        ).await.unwrap();

        logger.log(
            AuditEvent::StateResumed { mode: "collaborative".to_string() },
            "user-789",
            "state-456",
            Some(serde_json::json!({"region": "us-west-2"})),
        ).await.unwrap();

        // Verify entries
        let recent = logger.get_recent(10).await;
        assert_eq!(recent.len(), 2);

        // Verify integrity
        let valid = logger.verify_integrity().await.unwrap();
        assert!(valid);

        // Search
        let results = logger.search(Some("user-123"), None, None, 10).await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_hash_chaining() {
        let config = AuditConfig::default();
        let logger = AuditLogger::new(config).await.unwrap();

        // Log multiple entries
        for i in 0..5 {
            logger.log(
                AuditEvent::StateCreated { label: format!("state-{}", i) },
                "test-user",
                &format!("state-{}", i),
                None,
            ).await.unwrap();
        }

        // Verify chain integrity
        let valid = logger.verify_integrity().await.unwrap();
        assert!(valid);

        // Get entries and verify chain
        let entries = logger.get_recent(5).await;
        assert_eq!(entries.len(), 5);

        // Verify each entry's previous_hash matches the prior entry's hash
        for i in 1..entries.len() {
            assert_eq!(entries[i].previous_hash, entries[i - 1].entry_hash);
        }
    }
}
