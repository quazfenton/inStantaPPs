//! State Sharing Module
//!
//! Provides collaborative access control for states.
//! Multiple users can view, fork, and collaborate on shared states.
//!
//! # Features
//!
//! - Permission-based access control
//! - Time-limited sharing
//! - Collaborative sessions
//! - Access revocation

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::model::StateId;

/// Permission levels for state access
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Can only view the state
    Read,
    /// Can view and resume the state
    Execute,
    /// Can view, resume, and fork the state
    Fork,
    /// Full control including sharing with others
    Admin,
}

impl Permission {
    pub fn can_read(&self) -> bool {
        true // All permissions include read
    }

    pub fn can_execute(&self) -> bool {
        matches!(self, Permission::Execute | Permission::Fork | Permission::Admin)
    }

    pub fn can_fork(&self) -> bool {
        matches!(self, Permission::Fork | Permission::Admin)
    }

    pub fn can_share(&self) -> bool {
        matches!(self, Permission::Admin)
    }
}

/// Access grant for a user
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessGrant {
    /// User ID
    pub user_id: String,
    /// Permission level
    pub permission: Permission,
    /// Grant expiration (None = permanent)
    pub expires_at: Option<DateTime<Utc>>,
    /// Granted by
    pub granted_by: String,
    /// Grant timestamp
    pub granted_at: DateTime<Utc>,
}

impl AccessGrant {
    pub fn new(user_id: String, permission: Permission, granted_by: String) -> Self {
        Self {
            user_id,
            permission,
            expires_at: None,
            granted_by,
            granted_at: Utc::now(),
        }
    }

    pub fn with_expiry(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at.map(|exp| Utc::now() > exp).unwrap_or(false)
    }

    pub fn is_valid(&self) -> bool {
        !self.is_expired()
    }
}

/// State sharing manager
pub struct StateSharer {
    /// Access grants by state_id
    grants: Arc<RwLock<HashMap<StateId, Vec<AccessGrant>>>>,
    /// Active collaborative sessions
    sessions: Arc<RwLock<HashMap<String, CollaborativeSession>>>,
    /// Default permission for new states
    default_permission: Permission,
}

impl StateSharer {
    pub fn new() -> Self {
        Self {
            grants: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            default_permission: Permission::Read,
        }
    }

    pub fn with_default_permission(mut self, permission: Permission) -> Self {
        self.default_permission = permission;
        self
    }

    /// Grant access to a state
    pub async fn grant_access(
        &self,
        state_id: StateId,
        user_id: &str,
        permission: Permission,
        granted_by: &str,
        duration: Option<Duration>,
    ) -> Result<(), ShareError> {
        let mut grants = self.grants.write().await;

        let mut grant = AccessGrant::new(user_id.to_string(), permission, granted_by.to_string());

        if let Some(d) = duration {
            grant = grant.with_expiry(Utc::now() + d);
        }

        grants
            .entry(state_id.clone())
            .or_insert_with(Vec::new)
            .push(grant.clone());

        info!(
            "Granted {} access to {} for user {} (expires: {:?})",
            format!("{:?}", permission),
            state_id.0,
            user_id,
            grant.expires_at
        );

        Ok(())
    }

    /// Revoke access from a user
    pub async fn revoke_access(&self, state_id: &StateId, user_id: &str) -> Result<(), ShareError> {
        let mut grants = self.grants.write().await;

        if let Some(state_grants) = grants.get_mut(state_id) {
            let before = state_grants.len();
            state_grants.retain(|g| g.user_id != user_id);
            let removed = before - state_grants.len();

            if removed > 0 {
                info!("Revoked access to {} for user {}", state_id.0, user_id);
                Ok(())
            } else {
                Err(ShareError::AccessNotFound {
                    state_id: state_id.0.clone(),
                    user_id: user_id.to_string(),
                })
            }
        } else {
            Err(ShareError::AccessNotFound {
                state_id: state_id.0.clone(),
                user_id: user_id.to_string(),
            })
        }
    }

    /// Check if user has permission
    pub async fn check_permission(
        &self,
        state_id: &StateId,
        user_id: &str,
        required: Permission,
    ) -> bool {
        let grants = self.grants.read().await;

        if let Some(state_grants) = grants.get(state_id) {
            for grant in state_grants {
                if grant.user_id == user_id && grant.is_valid() {
                    return grant.permission as u8 >= required as u8;
                }
            }
        }

        false
    }

    /// Get user's permission for a state
    pub async fn get_permission(&self, state_id: &StateId, user_id: &str) -> Option<Permission> {
        let grants = self.grants.read().await;

        if let Some(state_grants) = grants.get(state_id) {
            for grant in state_grants {
                if grant.user_id == user_id && grant.is_valid() {
                    return Some(grant.permission);
                }
            }
        }

        None
    }

    /// List all users with access to a state
    pub async fn list_grantees(&self, state_id: &StateId) -> Vec<AccessGrant> {
        let grants = self.grants.read().await;

        if let Some(state_grants) = grants.get(state_id) {
            state_grants
                .iter()
                .filter(|g| g.is_valid())
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Create a collaborative session
    pub async fn create_session(
        &self,
        state_id: StateId,
        owner_id: &str,
    ) -> Result<CollaborativeSession, ShareError> {
        let session_id = Uuid::new_v4().to_string();

        let session = CollaborativeSession {
            session_id: session_id.clone(),
            state_id: state_id.clone(),
            owner_id: owner_id.to_string(),
            participants: vec![owner_id.to_string()],
            created_at: Utc::now(),
            last_activity: Utc::now(),
            status: SessionStatus::Active,
        };

        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id.clone(), session.clone());

        info!("Created collaborative session {} for state {}", session_id, state_id.0);
        Ok(session)
    }

    /// Join a collaborative session
    pub async fn join_session(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Result<CollaborativeSession, ShareError> {
        let mut sessions = self.sessions.write().await;

        if let Some(session) = sessions.get_mut(session_id) {
            if session.status != SessionStatus::Active {
                return Err(ShareError::SessionInactive(session_id.to_string()));
            }

            // Check if user has read permission on the state
            if !self.check_permission(&session.state_id, user_id, Permission::Read).await {
                return Err(ShareError::PermissionDenied {
                    user_id: user_id.to_string(),
                    required: Permission::Read,
                });
            }

            if !session.participants.contains(&user_id.to_string()) {
                session.participants.push(user_id.to_string());
            }

            session.last_activity = Utc::now();

            info!("User {} joined session {}", user_id, session_id);
            Ok(session.clone())
        } else {
            Err(ShareError::SessionNotFound(session_id.to_string()))
        }
    }

    /// Leave a collaborative session
    pub async fn leave_session(&self, session_id: &str, user_id: &str) -> Result<(), ShareError> {
        let mut sessions = self.sessions.write().await;

        if let Some(session) = sessions.get_mut(session_id) {
            session.participants.retain(|p| p != user_id);
            session.last_activity = Utc::now();

            // If owner leaves, mark session as inactive
            if session.owner_id == user_id {
                session.status = SessionStatus::Ended;
            }

            info!("User {} left session {}", user_id, session_id);
            Ok(())
        } else {
            Err(ShareError::SessionNotFound(session_id.to_string()))
        }
    }

    /// Get session info
    pub async fn get_session(&self, session_id: &str) -> Option<CollaborativeSession> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// End a collaborative session
    pub async fn end_session(&self, session_id: &str, user_id: &str) -> Result<(), ShareError> {
        let mut sessions = self.sessions.write().await;

        if let Some(session) = sessions.get_mut(session_id) {
            if session.owner_id != user_id {
                return Err(ShareError::PermissionDenied {
                    user_id: user_id.to_string(),
                    required: Permission::Admin,
                });
            }

            session.status = SessionStatus::Ended;
            session.last_activity = Utc::now();

            info!("Session {} ended by owner {}", session_id, user_id);
            Ok(())
        } else {
            Err(ShareError::SessionNotFound(session_id.to_string()))
        }
    }

    /// Get active sessions for a state
    pub async fn get_active_sessions(&self, state_id: &StateId) -> Vec<CollaborativeSession> {
        let sessions = self.sessions.read().await;

        sessions
            .values()
            .filter(|s| {
                s.state_id == *state_id && s.status == SessionStatus::Active
            })
            .cloned()
            .collect()
    }

    /// Cleanup expired grants and inactive sessions
    pub async fn cleanup(&self) -> CleanupResult {
        let mut grants = self.grants.write().await;
        let mut sessions = self.sessions.write().await;

        let mut expired_grants = 0;
        let mut ended_sessions = 0;

        // Cleanup expired grants
        for state_grants in grants.values_mut() {
            let before = state_grants.len();
            state_grants.retain(|g| g.is_valid());
            expired_grants += before - state_grants.len();
        }

        // Cleanup ended sessions (older than 24 hours)
        let cutoff = Utc::now() - Duration::hours(24);
        sessions.retain(|_, s| {
            if s.status == SessionStatus::Ended && s.last_activity < cutoff {
                ended_sessions += 1;
                false
            } else {
                true
            }
        });

        info!(
            "Cleanup complete: {} expired grants, {} ended sessions",
            expired_grants, ended_sessions
        );

        CleanupResult {
            expired_grants,
            ended_sessions,
        }
    }
}

impl Default for StateSharer {
    fn default() -> Self {
        Self::new()
    }
}

/// Collaborative session for real-time collaboration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborativeSession {
    pub session_id: String,
    pub state_id: StateId,
    pub owner_id: String,
    pub participants: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub status: SessionStatus,
}

/// Session status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Active,
    Paused,
    Ended,
}

/// Cleanup result
#[derive(Debug, Clone)]
pub struct CleanupResult {
    pub expired_grants: usize,
    pub ended_sessions: usize,
}

/// Share errors
#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("Access not found for state {state_id}, user {user_id}")]
    AccessNotFound {
        state_id: String,
        user_id: String,
    },

    #[error("Permission denied for user {user_id}, required {required:?}")]
    PermissionDenied {
        user_id: String,
        required: Permission,
    },

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Session inactive: {0}")]
    SessionInactive(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Shareable link for easy state sharing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareableLink {
    /// Unique link token
    pub token: String,
    /// Target state
    pub state_id: StateId,
    /// Permission granted by link
    pub permission: Permission,
    /// Link expiration
    pub expires_at: DateTime<Utc>,
    /// Max uses (None = unlimited)
    pub max_uses: Option<u32>,
    /// Current use count
    pub use_count: u32,
    /// Created by
    pub created_by: String,
}

impl ShareableLink {
    pub fn new(state_id: StateId, permission: Permission, created_by: &str, expires_in: Duration) -> Self {
        Self {
            token: Uuid::new_v4().to_string(),
            state_id,
            permission,
            expires_at: Utc::now() + expires_in,
            max_uses: None,
            use_count: 0,
            created_by: created_by.to_string(),
        }
    }

    pub fn with_max_uses(mut self, max_uses: u32) -> Self {
        self.max_uses = Some(max_uses);
        self
    }

    pub fn is_valid(&self) -> bool {
        Utc::now() < self.expires_at
            && self.max_uses.map(|max| self.use_count < max).unwrap_or(true)
    }

    pub fn record_use(&mut self) {
        self.use_count += 1;
    }
}

/// Link manager for shareable links
pub struct LinkManager {
    links: Arc<RwLock<HashMap<String, ShareableLink>>>,
    sharer: Arc<StateSharer>,
}

impl LinkManager {
    pub fn new(sharer: Arc<StateSharer>) -> Self {
        Self {
            links: Arc::new(RwLock::new(HashMap::new())),
            sharer,
        }
    }

    /// Create a shareable link
    pub async fn create_link(
        &self,
        state_id: StateId,
        permission: Permission,
        created_by: &str,
        expires_in: Duration,
    ) -> ShareableLink {
        let mut link = ShareableLink::new(state_id, permission, created_by, expires_in);

        let mut links = self.links.write().await;
        links.insert(link.token.clone(), link.clone());

        link
    }

    /// Use a shareable link
    pub async fn use_link(&self, token: &str, user_id: &str) -> Result<(), ShareError> {
        let mut links = self.links.write().await;

        if let Some(link) = links.get_mut(token) {
            if !link.is_valid() {
                return Err(ShareError::PermissionDenied {
                    user_id: user_id.to_string(),
                    required: Permission::Read,
                });
            }

            // Grant access
            self.sharer
                .grant_access(
                    link.state_id.clone(),
                    user_id,
                    link.permission,
                    "link",
                    Some(link.expires_at - Utc::now()),
                )
                .await?;

            link.record_use();

            info!("Link {} used by user {}", token, user_id);
            Ok(())
        } else {
            Err(ShareError::AccessNotFound {
                state_id: String::new(),
                user_id: user_id.to_string(),
            })
        }
    }

    /// Revoke a shareable link
    pub async fn revoke_link(&self, token: &str) -> Result<(), ShareError> {
        let mut links = self.links.write().await;

        if links.remove(token).is_some() {
            info!("Link {} revoked", token);
            Ok(())
        } else {
            Err(ShareError::AccessNotFound {
                state_id: String::new(),
                user_id: String::new(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grant_access() {
        let sharer = StateSharer::new();
        let state_id = StateId(Uuid::new_v4().to_string());

        // Grant access
        sharer
            .grant_access(
                state_id.clone(),
                "user-123",
                Permission::Execute,
                "admin",
                Some(Duration::hours(24)),
            )
            .await
            .unwrap();

        // Check permission
        assert!(sharer.check_permission(&state_id, "user-123", Permission::Read).await);
        assert!(sharer.check_permission(&state_id, "user-123", Permission::Execute).await);
        assert!(!sharer.check_permission(&state_id, "user-123", Permission::Fork).await);

        // Unknown user should not have access
        assert!(!sharer.check_permission(&state_id, "unknown", Permission::Read).await);
    }

    #[tokio::test]
    async fn test_revoke_access() {
        let sharer = StateSharer::new();
        let state_id = StateId(Uuid::new_v4().to_string());

        sharer
            .grant_access(state_id.clone(), "user-123", Permission::Read, "admin", None)
            .await
            .unwrap();

        // Verify access
        assert!(sharer.check_permission(&state_id, "user-123", Permission::Read).await);

        // Revoke
        sharer.revoke_access(&state_id, "user-123").await.unwrap();

        // Verify revoked
        assert!(!sharer.check_permission(&state_id, "user-123", Permission::Read).await);
    }

    #[tokio::test]
    async fn test_collaborative_session() {
        let sharer = StateSharer::new();
        let state_id = StateId(Uuid::new_v4().to_string());

        // Create session
        let session = sharer.create_session(state_id.clone(), "owner").await.unwrap();
        assert_eq!(session.participants.len(), 1);
        assert_eq!(session.status, SessionStatus::Active);

        // Join session
        sharer
            .grant_access(state_id.clone(), "user-1", Permission::Read, "owner", None)
            .await
            .unwrap();

        let joined = sharer.join_session(&session.session_id, "user-1").await.unwrap();
        assert_eq!(joined.participants.len(), 2);

        // Leave session
        sharer.leave_session(&session.session_id, "user-1").await.unwrap();

        let session = sharer.get_session(&session.session_id).await.unwrap();
        assert_eq!(session.participants.len(), 1);
    }

    #[tokio::test]
    async fn test_shareable_link() {
        let sharer = Arc::new(StateSharer::new());
        let link_manager = LinkManager::new(sharer.clone());

        let state_id = StateId(Uuid::new_v4().to_string());

        // Create link
        let link = link_manager
            .create_link(
                state_id.clone(),
                Permission::Read,
                "creator",
                Duration::hours(24),
            )
            .await;

        // Use link
        link_manager.use_link(&link.token, "new-user").await.unwrap();

        // Verify access granted
        assert!(sharer.check_permission(&state_id, "new-user", Permission::Read).await);
    }

    #[tokio::test]
    async fn test_expired_grant() {
        let sharer = StateSharer::new();
        let state_id = StateId(Uuid::new_v4().to_string());

        // Grant with very short expiry
        sharer
            .grant_access(
                state_id.clone(),
                "user-123",
                Permission::Read,
                "admin",
                Some(Duration::milliseconds(100)),
            )
            .await
            .unwrap();

        // Should have access initially
        assert!(sharer.check_permission(&state_id, "user-123", Permission::Read).await);

        // Wait for expiry
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        // Should not have access after expiry
        assert!(!sharer.check_permission(&state_id, "user-123", Permission::Read).await);
    }
}
