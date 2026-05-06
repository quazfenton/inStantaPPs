//! Authentication and Authorization Module
//!
//! Provides capability-based access control for ISA operations.
//!
//! # Security Model
//!
//! - API keys for authentication
//! - Capability-based authorization (snapshot, resume, fork, delete)
//! - Time-limited tokens
//! - Per-state permissions
//!
//! # Usage
//!
//! ```rust,no_run
//! use isa_workspace::auth::{AuthManager, AuthConfig, ApiKey, Capability};
//!
//! let config = AuthConfig::default();
//! let auth = AuthManager::new(config);
//!
//! // Create API key with specific capabilities
//! let key = auth.create_key("user-123", vec![Capability::Snapshot, Capability::Resume]);
//!
//! // Verify token on each request
//! let result = auth.verify_token(&token).await?;
//! if result.has_capability(Capability::Snapshot) {
//!     // Allow snapshot operation
//! }
//! ```

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Enable authentication (false = allow all)
    pub enabled: bool,
    /// Token expiration time (hours)
    pub token_expiry_hours: u64,
    /// Maximum tokens per user
    pub max_tokens_per_user: usize,
    /// Secret key for HMAC (should be loaded from secure storage)
    pub secret_key: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token_expiry_hours: 24,
            max_tokens_per_user: 10,
            // In production, this MUST be loaded from environment or secrets manager
            secret_key: "CHANGE_ME_IN_PRODUCTION".to_string(),
        }
    }
}

/// Capabilities for authorization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Create state snapshots
    Snapshot,
    /// Resume from snapshots
    Resume,
    /// Fork existing states
    Fork,
    /// Delete states
    Delete,
    /// Share states with others
    Share,
    /// List states
    List,
    /// Read state metadata
    Read,
    /// Full admin access
    Admin,
}

/// API key information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// Key ID (public identifier)
    pub key_id: String,
    /// Key secret (private, shown only on creation)
    pub key_secret: Option<String>,
    /// Hash of the key secret for verification
    pub key_hash: String,
    /// Owner user ID
    pub user_id: String,
    /// Capabilities granted to this key
    pub capabilities: Vec<Capability>,
    /// Created timestamp
    pub created_at: DateTime<Utc>,
    /// Expiration timestamp (None = never expires)
    pub expires_at: Option<DateTime<Utc>>,
    /// Whether the key is revoked
    pub revoked: bool,
    /// Optional state ID restriction (key only works for this state)
    pub state_restriction: Option<String>,
}

/// Access token for API requests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessToken {
    /// Token string (bearer token)
    pub token: String,
    /// Token type
    pub token_type: String,
    /// Expiration timestamp
    pub expires_at: DateTime<Utc>,
    /// Associated API key ID
    pub key_id: String,
    /// User ID
    pub user_id: String,
    /// Granted capabilities
    pub capabilities: Vec<Capability>,
}

/// Authentication result
#[derive(Debug, Clone)]
pub struct AuthResult {
    /// User ID
    pub user_id: String,
    /// API key ID
    pub key_id: String,
    /// Granted capabilities
    pub capabilities: Vec<Capability>,
    /// State restriction (if any)
    pub state_restriction: Option<String>,
}

impl AuthResult {
    /// Check if user has a specific capability
    pub fn has_capability(&self, cap: Capability) -> bool {
        self.capabilities.contains(&cap)
    }

    /// Check if user can access a specific state
    pub fn can_access_state(&self, state_id: &str) -> bool {
        match &self.state_restriction {
            Some(restricted_id) => restricted_id == state_id,
            None => true, // No restriction
        }
    }
}

/// Authentication manager
pub struct AuthManager {
    config: AuthConfig,
    /// API keys by key_id
    keys: Arc<RwLock<HashMap<String, ApiKey>>>,
    /// Active tokens by token string
    tokens: Arc<RwLock<HashMap<String, AccessToken>>>,
    /// Keys by user_id (for listing)
    user_keys: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl AuthManager {
    /// Create a new authentication manager
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config,
            keys: Arc::new(RwLock::new(HashMap::new())),
            tokens: Arc::new(RwLock::new(HashMap::new())),
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new API key for a user
    pub async fn create_key(
        &self,
        user_id: &str,
        capabilities: Vec<Capability>,
    ) -> Result<ApiKey, AuthError> {
        // Generate key secret
        let key_secret = format!("isa_{}", Uuid::new_v4());
        let key_id = format!("key_{}", Uuid::new_v4().as_simple());

        // Hash the secret for storage
        let key_hash = self.hash_key(&key_secret);

        // Calculate expiration
        let expires_at = None; // API keys don't expire by default

        let api_key = ApiKey {
            key_id: key_id.clone(),
            key_secret: Some(key_secret.clone()),
            key_hash,
            user_id: user_id.to_string(),
            capabilities,
            created_at: Utc::now(),
            expires_at,
            revoked: false,
            state_restriction: None,
        };

        // Store the key
        {
            let mut keys = self.keys.write().await;
            keys.insert(key_id.clone(), api_key.clone());
        }

        // Update user's key list
        {
            let mut user_keys = self.user_keys.write().await;
            user_keys
                .entry(user_id.to_string())
                .or_insert_with(Vec::new)
                .push(key_id.clone());
        }

        info!(user_id = %user_id, key_id = %key_id, "API key created");
        Ok(api_key)
    }

    /// Revoke an API key
    pub async fn revoke_key(&self, key_id: &str) -> Result<(), AuthError> {
        let mut keys = self.keys.write().await;

        if let Some(key) = keys.get_mut(key_id) {
            key.revoked = true;
            info!(key_id = %key_id, "API key revoked");
            Ok(())
        } else {
            Err(AuthError::KeyNotFound(key_id.to_string()))
        }
    }

    /// Verify an API key and return auth result
    pub async fn verify_api_key(&self, key_id: &str, key_secret: &str) -> Result<AuthResult, AuthError> {
        let keys = self.keys.read().await;

        let key = keys.get(key_id)
            .ok_or_else(|| AuthError::KeyNotFound(key_id.to_string()))?;

        // Check if revoked
        if key.revoked {
            return Err(AuthError::KeyRevoked);
        }

        // Check expiration
        if let Some(expires_at) = &key.expires_at {
            if &Utc::now() > expires_at {
                return Err(AuthError::KeyExpired);
            }
        }

        // Verify key hash
        let provided_hash = self.hash_key(key_secret);
        if provided_hash != key.key_hash {
            return Err(AuthError::InvalidKey);
        }

        Ok(AuthResult {
            user_id: key.user_id.clone(),
            key_id: key_id.to_string(),
            capabilities: key.capabilities.clone(),
            state_restriction: key.state_restriction.clone(),
        })
    }

    /// Create a bearer token from an API key
    pub async fn create_token(&self, key_id: &str, key_secret: &str) -> Result<AccessToken, AuthError> {
        // First verify the API key
        let auth_result = self.verify_api_key(key_id, key_secret).await?;

        // Generate token
        let token = format!("bearer_{}", Uuid::new_v4());
        let expires_at = Utc::now() + Duration::hours(self.config.token_expiry_hours as i64);

        let access_token = AccessToken {
            token: token.clone(),
            token_type: "Bearer".to_string(),
            expires_at,
            key_id: key_id.to_string(),
            user_id: auth_result.user_id.clone(),
            capabilities: auth_result.capabilities.clone(),
        };

        // Store token
        {
            let mut tokens = self.tokens.write().await;

            // Check token limit
            let user_token_count = tokens.values()
                .filter(|t| t.user_id == auth_result.user_id)
                .count();

            if user_token_count >= self.config.max_tokens_per_user {
                return Err(AuthError::TokenLimitExceeded);
            }

            tokens.insert(token.clone(), access_token.clone());
        }

        debug!(user_id = %auth_result.user_id, "Access token created");
        Ok(access_token)
    }

    /// Verify a bearer token
    pub async fn verify_token(&self, token: &str) -> Result<AuthResult, AuthError> {
        let tokens = self.tokens.read().await;

        let access_token = tokens.get(token)
            .ok_or_else(|| AuthError::InvalidToken)?;

        // Check expiration
        if Utc::now() > access_token.expires_at {
            // Clean up expired token
            drop(tokens);
            self.revoke_token(token).await?;
            return Err(AuthError::TokenExpired);
        }

        Ok(AuthResult {
            user_id: access_token.user_id.clone(),
            key_id: access_token.key_id.clone(),
            capabilities: access_token.capabilities.clone(),
            state_restriction: None,
        })
    }

    /// Revoke a bearer token
    pub async fn revoke_token(&self, token: &str) -> Result<(), AuthError> {
        let mut tokens = self.tokens.write().await;
        tokens.remove(token);
        Ok(())
    }

    /// Clean up expired tokens
    pub async fn cleanup_expired_tokens(&self) -> usize {
        let mut tokens = self.tokens.write().await;
        let now = Utc::now();
        let initial_count = tokens.len();

        tokens.retain(|_, token| token.expires_at > now);

        let removed = initial_count - tokens.len();
        if removed > 0 {
            debug!("Cleaned up {} expired tokens", removed);
        }
        removed
    }

    /// List API keys for a user
    pub async fn list_keys(&self, user_id: &str) -> Vec<ApiKey> {
        let user_keys = self.user_keys.read().await;
        let keys = self.keys.read().await;

        user_keys.get(user_id)
            .map(|key_ids| {
                key_ids.iter()
                    .filter_map(|id| keys.get(id))
                    .map(|k| ApiKey {
                        key_id: k.key_id.clone(),
                        key_secret: None, // Never return secret
                        key_hash: k.key_hash.clone(),
                        user_id: k.user_id.clone(),
                        capabilities: k.capabilities.clone(),
                        created_at: k.created_at,
                        expires_at: k.expires_at,
                        revoked: k.revoked,
                        state_restriction: k.state_restriction.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Hash a key secret for verification
    fn hash_key(&self, key: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.config.secret_key.as_bytes());
        hasher.update(key.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Check if authentication is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

/// Authentication errors
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("API key not found: {0}")]
    KeyNotFound(String),

    #[error("API key has been revoked")]
    KeyRevoked,

    #[error("API key has expired")]
    KeyExpired,

    #[error("Invalid API key")]
    InvalidKey,

    #[error("Invalid token")]
    InvalidToken,

    #[error("Token has expired")]
    TokenExpired,

    #[error("Token limit exceeded")]
    TokenLimitExceeded,

    #[error("Insufficient capabilities: required {0}")]
    InsufficientCapabilities(String),

    #[error("Access denied to state: {0}")]
    AccessDenied(String),

    #[error("Authentication required")]
    AuthenticationRequired,
}

/// Axum extractor for authenticated requests
#[derive(Debug)]
pub struct AuthenticatedUser(pub AuthResult);

/// Axum extractor for capability-checked requests
#[derive(Debug)]
pub struct AuthorizedUser {
    pub user: AuthResult,
    pub capability: Capability,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_and_verify_key() {
        let auth = AuthManager::new(AuthConfig::default());

        let key = auth.create_key("user-123", vec![Capability::Snapshot, Capability::Resume])
            .await
            .unwrap();

        assert_eq!(key.user_id, "user-123");
        assert!(key.key_secret.is_some());

        let secret = key.key_secret.unwrap();
        let result = auth.verify_api_key(&key.key_id, &secret).await.unwrap();

        assert_eq!(result.user_id, "user-123");
        assert!(result.has_capability(Capability::Snapshot));
        assert!(result.has_capability(Capability::Resume));
    }

    #[tokio::test]
    async fn test_revoke_key() {
        let auth = AuthManager::new(AuthConfig::default());

        let key = auth.create_key("user-123", vec![Capability::Snapshot])
            .await
            .unwrap();

        auth.revoke_key(&key.key_id).await.unwrap();

        let result = auth.verify_api_key(&key.key_id, &key.key_secret.unwrap()).await;
        assert!(matches!(result, Err(AuthError::KeyRevoked)));
    }

    #[tokio::test]
    async fn test_create_and_verify_token() {
        let auth = AuthManager::new(AuthConfig::default());

        let key = auth.create_key("user-123", vec![Capability::Snapshot])
            .await
            .unwrap();

        let token = auth.create_token(&key.key_id, &key.key_secret.unwrap())
            .await
            .unwrap();

        let result = auth.verify_token(&token.token).await.unwrap();
        assert_eq!(result.user_id, "user-123");
        assert!(result.has_capability(Capability::Snapshot));
    }

    #[tokio::test]
    async fn test_invalid_key() {
        let auth = AuthManager::new(AuthConfig::default());

        let key = auth.create_key("user-123", vec![]).await.unwrap();

        let result = auth.verify_api_key(&key.key_id, "wrong_secret").await;
        assert!(matches!(result, Err(AuthError::InvalidKey)));
    }

    #[tokio::test]
    async fn test_capability_check() {
        let auth = AuthManager::new(AuthConfig::default());

        let key = auth.create_key("user-123", vec![Capability::Snapshot])
            .await
            .unwrap();

        let result = auth.verify_api_key(&key.key_id, &key.key_secret.unwrap())
            .await
            .unwrap();

        assert!(result.has_capability(Capability::Snapshot));
        assert!(!result.has_capability(Capability::Delete));
    }
}
