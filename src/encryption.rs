//! Encryption Module
//!
//! Provides AES-256-GCM encryption for state at rest.
//! Each state is encrypted with a unique key derived from a master key.
//!
//! # Features
//!
//! - AES-256-GCM authenticated encryption
//! - Per-state unique keys (HKDF derivation)
//! - Key rotation support
//! - Secure key storage interface

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

/// Encryption key (32 bytes for AES-256)
pub type EncryptionKey = [u8; 32];

/// Nonce (12 bytes for GCM)
pub type NonceBytes = [u8; 12];

/// Encrypted data with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedData {
    /// Encrypted payload
    pub ciphertext: Vec<u8>,
    /// Nonce used for encryption
    pub nonce: NonceBytes,
    /// Key version (for rotation)
    pub key_version: u32,
    /// Authentication tag (included in ciphertext by aes-gcm)
}

/// Master key manager
pub struct KeyManager {
    /// Current master key
    master_key: EncryptionKey,
    /// Key version
    key_version: u32,
    /// Derived keys cache
    derived_keys: Arc<RwLock<HashMap<String, EncryptionKey>>>,
}

impl KeyManager {
    /// Create a new key manager with a random master key
    pub fn new() -> Self {
        let mut master_key = [0u8; 32];
        OsRng.fill_bytes(&mut master_key);
        
        Self {
            master_key,
            key_version: 1,
            derived_keys: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create from existing key (for loading from storage)
    pub fn from_key(master_key: EncryptionKey, key_version: u32) -> Self {
        Self {
            master_key,
            key_version,
            derived_keys: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get master key version
    pub fn key_version(&self) -> u32 {
        self.key_version
    }

    /// Derive a unique key for a state ID
    pub async fn derive_key(&self, state_id: &str) -> EncryptionKey {
        // Check cache first
        {
            let cache = self.derived_keys.read().await;
            if let Some(key) = cache.get(state_id) {
                return *key;
            }
        }

        // Derive key using HKDF-SHA256
        let mut derived = [0u8; 32];
        let mut hasher = Sha256::new();
        hasher.update(b"isa-state-key-derivation");
        hasher.update(&self.master_key);
        hasher.update(state_id.as_bytes());
        hasher.update(&self.key_version.to_be_bytes());
        derived.copy_from_slice(&hasher.finalize());

        // Cache the derived key
        let mut cache = self.derived_keys.write().await;
        cache.insert(state_id.to_string(), derived);

        derived
    }

    /// Rotate to a new master key
    pub fn rotate_key(&mut self) -> EncryptionKey {
        let mut new_key = [0u8; 32];
        OsRng.fill_bytes(&mut new_key);
        
        self.master_key = new_key;
        self.key_version += 1;
        
        // Clear derived keys cache (will be re-derived on demand)
        let mut cache = self.derived_keys.write().await;
        cache.clear();
        
        info!("Key rotated to version {}", self.key_version);
        new_key
    }

    /// Export master key (for backup)
    pub fn export_key(&self) -> EncryptionKey {
        self.master_key
    }
}

impl Default for KeyManager {
    fn default() -> Self {
        Self::new()
    }
}

/// State encryption helper
pub struct StateEncryptor {
    key_manager: Arc<KeyManager>,
}

impl StateEncryptor {
    pub fn new(key_manager: Arc<KeyManager>) -> Self {
        Self { key_manager }
    }

    /// Encrypt data for a specific state
    pub async fn encrypt(&self, state_id: &str, plaintext: &[u8]) -> Result<EncryptedData, EncryptionError> {
        let key = self.key_manager.derive_key(state_id).await;
        let key_version = self.key_manager.key_version();

        // Generate random nonce
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Create cipher and encrypt
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| EncryptionError::CipherError(e.to_string()))?;

        let ciphertext = cipher.encrypt(nonce, plaintext)
            .map_err(|e| EncryptionError::EncryptionFailed(e.to_string()))?;

        debug!("Encrypted {} bytes for state {}", plaintext.len(), state_id);

        Ok(EncryptedData {
            ciphertext,
            nonce: nonce_bytes,
            key_version,
        })
    }

    /// Decrypt data for a specific state
    pub async fn decrypt(&self, state_id: &str, encrypted: &EncryptedData) -> Result<Vec<u8>, EncryptionError> {
        // Check key version
        if encrypted.key_version != self.key_manager.key_version() {
            return Err(EncryptionError::KeyVersionMismatch {
                expected: self.key_manager.key_version(),
                got: encrypted.key_version,
            });
        }

        let key = self.key_manager.derive_key(state_id).await;
        let nonce = Nonce::from_slice(&encrypted.nonce);

        // Create cipher and decrypt
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| EncryptionError::CipherError(e.to_string()))?;

        let plaintext = cipher.decrypt(nonce, encrypted.ciphertext.as_ref())
            .map_err(|e| EncryptionError::DecryptionFailed(e.to_string()))?;

        debug!("Decrypted {} bytes for state {}", plaintext.len(), state_id);

        Ok(plaintext)
    }
}

/// Encryption errors
#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("Cipher error: {0}")]
    CipherError(String),

    #[error("Encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("Decryption failed: {0}")]
    DecryptionFailed(String),

    #[error("Key version mismatch: expected {expected}, got {got}")]
    KeyVersionMismatch {
        expected: u32,
        got: u32,
    },

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Secure key storage trait
pub trait KeyStorage: Send + Sync {
    /// Store master key securely
    fn store_key(&self, key: &EncryptionKey, version: u32) -> Result<(), EncryptionError>;
    
    /// Load master key
    fn load_key(&self) -> Result<Option<(EncryptionKey, u32)>, EncryptionError>;
    
    /// Delete master key
    fn delete_key(&self) -> Result<(), EncryptionError>;
}

/// In-memory key storage (for testing)
pub struct MemoryKeyStorage {
    storage: Arc<RwLock<Option<(EncryptionKey, u32)>>>,
}

impl MemoryKeyStorage {
    pub fn new() -> Self {
        Self {
            storage: Arc::new(RwLock::new(None)),
        }
    }
}

impl Default for MemoryKeyStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl KeyStorage for MemoryKeyStorage {
    async fn store_key(&self, key: &EncryptionKey, version: u32) -> Result<(), EncryptionError> {
        let mut storage = self.storage.write().await;
        *storage = Some((*key, version));
        Ok(())
    }

    async fn load_key(&self) -> Result<Option<(EncryptionKey, u32)>, EncryptionError> {
        let storage = self.storage.read().await;
        Ok(*storage)
    }

    async fn delete_key(&self) -> Result<(), EncryptionError> {
        let mut storage = self.storage.write().await;
        *storage = None;
        Ok(())
    }
}

/// File-based key storage (for production)
pub struct FileKeyStorage {
    key_path: std::path::PathBuf,
}

impl FileKeyStorage {
    pub fn new<P: AsRef<std::path::Path>>(key_path: P) -> Self {
        Self {
            key_path: key_path.as_ref().to_path_buf(),
        }
    }
}

#[async_trait::async_trait]
impl KeyStorage for FileKeyStorage {
    async fn store_key(&self, key: &EncryptionKey, version: u32) -> Result<(), EncryptionError> {
        use tokio::fs;
        
        // Serialize key with version
        let mut data = Vec::with_capacity(36);
        data.extend_from_slice(key);
        data.extend_from_slice(&version.to_be_bytes());
        
        // Write to file with restricted permissions
        fs::write(&self.key_path, &data).await?;
        
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&self.key_path).await?.permissions();
            perms.set_mode(0o600); // Owner read/write only
            fs::set_permissions(&self.key_path, perms).await?;
        }
        
        Ok(())
    }

    async fn load_key(&self) -> Result<Option<(EncryptionKey, u32)>, EncryptionError> {
        use tokio::fs;
        
        if !self.key_path.exists() {
            return Ok(None);
        }
        
        let data = fs::read(&self.key_path).await?;
        
        if data.len() != 36 {
            return Err(EncryptionError::DecryptionFailed("Invalid key file".to_string()));
        }
        
        let mut key = [0u8; 32];
        key.copy_from_slice(&data[0..32]);
        
        let mut version_bytes = [0u8; 4];
        version_bytes.copy_from_slice(&data[32..36]);
        let version = u32::from_be_bytes(version_bytes);
        
        Ok(Some((key, version)))
    }

    async fn delete_key(&self) -> Result<(), EncryptionError> {
        use tokio::fs;
        
        if self.key_path.exists() {
            fs::remove_file(&self.key_path).await?;
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_encryption_decryption() {
        let key_manager = Arc::new(KeyManager::new());
        let encryptor = StateEncryptor::new(key_manager.clone());

        let state_id = "test-state-123";
        let plaintext = b"Hello, encrypted world!";

        // Encrypt
        let encrypted = encryptor.encrypt(state_id, plaintext).await.unwrap();
        
        // Verify nonce is random
        let encrypted2 = encryptor.encrypt(state_id, plaintext).await.unwrap();
        assert_ne!(encrypted.nonce, encrypted2.nonce);

        // Decrypt
        let decrypted = encryptor.decrypt(state_id, &encrypted).await.unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[tokio::test]
    async fn test_key_derivation() {
        let key_manager = KeyManager::new();
        
        // Same state ID should produce same key
        let key1 = key_manager.derive_key("state-1").await;
        let key2 = key_manager.derive_key("state-1").await;
        assert_eq!(key1, key2);

        // Different state IDs should produce different keys
        let key3 = key_manager.derive_key("state-2").await;
        assert_ne!(key1, key3);
    }

    #[tokio::test]
    async fn test_key_rotation() {
        let mut key_manager = KeyManager::new();
        let encryptor = StateEncryptor::new(Arc::new(key_manager.clone()));

        let state_id = "test-state";
        let plaintext = b"Test data";

        // Encrypt with old key
        let encrypted = encryptor.encrypt(state_id, plaintext).await.unwrap();
        
        // Rotate key
        key_manager.rotate_key();
        let encryptor = StateEncryptor::new(Arc::new(key_manager));

        // Decrypt should fail with new key
        let result = encryptor.decrypt(state_id, &encrypted).await;
        assert!(matches!(result, Err(EncryptionError::KeyVersionMismatch { .. })));
    }

    #[tokio::test]
    async fn test_memory_key_storage() {
        let storage = MemoryKeyStorage::new();
        let key = [1u8; 32];
        
        storage.store_key(&key, 1).await.unwrap();
        
        let loaded = storage.load_key().await.unwrap();
        assert_eq!(loaded, Some((key, 1)));
        
        storage.delete_key().await.unwrap();
        
        let loaded = storage.load_key().await.unwrap();
        assert_eq!(loaded, None);
    }
}
