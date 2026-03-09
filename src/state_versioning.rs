//! State Versioning Module
//!
//! Provides version control for states with branching and history.
//! Similar to Git but for VM states.
//!
//! # Features
//!
//! - State history tracking
//! - Branching and merging
//! - Tags and labels
//! - Rollback support

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::model::StateId;

/// Version control for states
pub struct StateVersionControl {
    /// State history by state_id
    history: Arc<RwLock<HashMap<StateId, StateHistory>>>,
    /// Branches by name
    branches: Arc<RwLock<HashMap<String, StateId>>>,
    /// Tags by name
    tags: Arc<RwLock<HashMap<String, StateId>>>,
}

impl StateVersionControl {
    pub fn new() -> Self {
        Self {
            history: Arc::new(RwLock::new(HashMap::new())),
            branches: Arc::new(RwLock::new(HashMap::new())),
            tags: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record a new state in history
    pub async fn record_state(
        &self,
        state_id: StateId,
        parent_id: Option<StateId>,
        message: &str,
        author: &str,
    ) -> Result<(), VersionError> {
        let mut history = self.history.write().await;

        let entry = HistoryEntry {
            state_id: state_id.clone(),
            parent_id: parent_id.clone(),
            timestamp: Utc::now(),
            message: message.to_string(),
            author: author.to_string(),
            tags: Vec::new(),
        };

        history
            .entry(state_id.clone())
            .or_insert_with(|| StateHistory::new(state_id.clone()))
            .entries
            .push(entry);

        // Update parent's children
        if let Some(parent) = parent_id {
            if let Some(parent_history) = history.get_mut(&parent) {
                parent_history.children.push(state_id.clone());
            }
        }

        info!("Recorded state {} with parent {:?}", state_id.0, parent_id);
        Ok(())
    }

    /// Create a new branch at a state
    pub async fn create_branch(&self, name: &str, at_state: StateId) -> Result<(), VersionError> {
        let mut branches = self.branches.write().await;

        if branches.contains_key(name) {
            return Err(VersionError::BranchExists(name.to_string()));
        }

        branches.insert(name.to_string(), at_state.clone());

        // Also record in history
        self.record_state(at_state.clone(), None, &format!("Branch: {}", name), "system")
            .await?;

        info!("Created branch '{}' at {}", name, at_state.0);
        Ok(())
    }

    /// Get branch head
    pub async fn get_branch(&self, name: &str) -> Option<StateId> {
        let branches = self.branches.read().await;
        branches.get(name).cloned()
    }

    /// List all branches
    pub async fn list_branches(&self) -> Vec<String> {
        let branches = self.branches.read().await;
        branches.keys().cloned().collect()
    }

    /// Delete a branch
    pub async fn delete_branch(&self, name: &str) -> Result<(), VersionError> {
        let mut branches = self.branches.write().await;

        if branches.remove(name).is_none() {
            return Err(VersionError::BranchNotFound(name.to_string()));
        }

        info!("Deleted branch '{}'", name);
        Ok(())
    }

    /// Create a tag at a state
    pub async fn create_tag(&self, name: &str, at_state: StateId) -> Result<(), VersionError> {
        let mut tags = self.tags.write().await;

        if tags.contains_key(name) {
            return Err(VersionError::TagExists(name.to_string()));
        }

        tags.insert(name.to_string(), at_state.clone());

        info!("Created tag '{}' at {}", name, at_state.0);
        Ok(())
    }

    /// Get tag
    pub async fn get_tag(&self, name: &str) -> Option<StateId> {
        let tags = self.tags.read().await;
        tags.get(name).cloned()
    }

    /// List all tags
    pub async fn list_tags(&self) -> Vec<String> {
        let tags = self.tags.read().await;
        tags.keys().cloned().collect()
    }

    /// Delete a tag
    pub async fn delete_tag(&self, name: &str) -> Result<(), VersionError> {
        let mut tags = self.tags.write().await;

        if tags.remove(name).is_none() {
            return Err(VersionError::TagNotFound(name.to_string()));
        }

        info!("Deleted tag '{}'", name);
        Ok(())
    }

    /// Get state history
    pub async fn get_history(&self, state_id: &StateId) -> Option<StateHistory> {
        let history = self.history.read().await;
        history.get(state_id).cloned()
    }

    /// Get full ancestry (all parents)
    pub async fn get_ancestry(&self, state_id: &StateId) -> Vec<StateId> {
        let mut ancestry = Vec::new();
        let history = self.history.read().await;

        let mut current = Some(state_id.clone());
        while let Some(id) = current {
            if let Some(hist) = history.get(&id) {
                ancestry.push(id.clone());
                current = hist.entries.first().and_then(|e| e.parent_id.clone());
            } else {
                break;
            }
        }

        ancestry
    }

    /// Get all descendants (children, grandchildren, etc.)
    pub async fn get_descendants(&self, state_id: &StateId) -> Vec<StateId> {
        let mut descendants = Vec::new();
        let history = self.history.read().await;

        let mut to_visit = vec![state_id.clone()];
        while let Some(id) = to_visit.pop() {
            if let Some(hist) = history.get(&id) {
                for child in &hist.children {
                    if !descendants.contains(child) {
                        descendants.push(child.clone());
                        to_visit.push(child.clone());
                    }
                }
            }
        }

        descendants
    }

    /// Find common ancestor of two states
    pub async fn find_common_ancestor(
        &self,
        state_a: &StateId,
        state_b: &StateId,
    ) -> Option<StateId> {
        let ancestry_a = self.get_ancestry(state_a).await;
        let ancestry_b = self.get_ancestry(state_b).await;

        // Find first common ancestor
        for id in &ancestry_a {
            if ancestry_b.contains(id) {
                return Some(id.clone());
            }
        }

        None
    }

    /// Rollback to a previous state
    pub async fn rollback(&self, to_state: &StateId) -> Result<StateId, VersionError> {
        // Verify state exists in history
        let history = self.history.read().await;
        if !history.contains_key(to_state) {
            return Err(VersionError::StateNotFound(to_state.0.clone()));
        }

        // Create new rollback state
        let rollback_id = StateId(Uuid::new_v4().to_string());

        drop(history);

        self.record_state(
            rollback_id.clone(),
            Some(to_state.clone()),
            "Rollback",
            "system",
        )
        .await?;

        info!("Rolled back to {} (new state: {})", to_state.0, rollback_id.0);
        Ok(rollback_id)
    }

    /// Get version statistics
    pub async fn get_stats(&self) -> VersionStats {
        let history = self.history.read().await;
        let branches = self.branches.read().await;
        let tags = self.tags.read().await;

        let total_states: usize = history.values().map(|h| h.entries.len()).sum();
        let total_branches = branches.len();
        let total_tags = tags.len();

        VersionStats {
            total_states,
            total_branches,
            total_tags,
            oldest_state: history.values().flat_map(|h| h.entries.iter()).map(|e| e.timestamp).min(),
            newest_state: history.values().flat_map(|h| h.entries.iter()).map(|e| e.timestamp).max(),
        }
    }
}

impl Default for StateVersionControl {
    fn default() -> Self {
        Self::new()
    }
}

/// History of a single state lineage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateHistory {
    pub state_id: StateId,
    pub entries: Vec<HistoryEntry>,
    pub children: Vec<StateId>,
}

impl StateHistory {
    pub fn new(state_id: StateId) -> Self {
        Self {
            state_id,
            entries: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// Single history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub state_id: StateId,
    pub parent_id: Option<StateId>,
    pub timestamp: DateTime<Utc>,
    pub message: String,
    pub author: String,
    pub tags: Vec<String>,
}

/// Version statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionStats {
    pub total_states: usize,
    pub total_branches: usize,
    pub total_tags: usize,
    pub oldest_state: Option<DateTime<Utc>>,
    pub newest_state: Option<DateTime<Utc>>,
}

/// Version errors
#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("Branch already exists: {0}")]
    BranchExists(String),

    #[error("Branch not found: {0}")]
    BranchNotFound(String),

    #[error("Tag already exists: {0}")]
    TagExists(String),

    #[error("Tag not found: {0}")]
    TagNotFound(String),

    #[error("State not found: {0}")]
    StateNotFound(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Branch for collaborative work
pub struct Branch {
    pub name: String,
    pub head: StateId,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

impl Branch {
    pub fn new(name: &str, head: StateId, created_by: &str) -> Self {
        Self {
            name: name.to_string(),
            head,
            created_at: Utc::now(),
            created_by: created_by.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_branch_operations() {
        let vcs = StateVersionControl::new();

        let state_id = StateId(Uuid::new_v4().to_string());

        // Create branch
        vcs.create_branch("main", state_id.clone()).await.unwrap();

        // Get branch
        let head = vcs.get_branch("main").await;
        assert_eq!(head, Some(state_id.clone()));

        // List branches
        let branches = vcs.list_branches().await;
        assert_eq!(branches, vec!["main"]);

        // Try to create duplicate branch
        let result = vcs.create_branch("main", state_id.clone()).await;
        assert!(matches!(result, Err(VersionError::BranchExists(_))));

        // Delete branch
        vcs.delete_branch("main").await.unwrap();

        // Verify deletion
        let head = vcs.get_branch("main").await;
        assert_eq!(head, None);
    }

    #[tokio::test]
    async fn test_tag_operations() {
        let vcs = StateVersionControl::new();
        let state_id = StateId(Uuid::new_v4().to_string());

        // Create tag
        vcs.create_tag("v1.0.0", state_id.clone()).await.unwrap();

        // Get tag
        let tag = vcs.get_tag("v1.0.0").await;
        assert_eq!(tag, Some(state_id.clone()));

        // List tags
        let tags = vcs.list_tags().await;
        assert_eq!(tags, vec!["v1.0.0"]);

        // Delete tag
        vcs.delete_tag("v1.0.0").await.unwrap();
    }

    #[tokio::test]
    async fn test_ancestry() {
        let vcs = StateVersionControl::new();

        let state1 = StateId(Uuid::new_v4().to_string());
        let state2 = StateId(Uuid::new_v4().to_string());
        let state3 = StateId(Uuid::new_v4().to_string());

        // Build chain: state1 -> state2 -> state3
        vcs.record_state(state1.clone(), None, "Initial", "user").await.unwrap();
        vcs.record_state(state2.clone(), Some(state1.clone()), "Second", "user").await.unwrap();
        vcs.record_state(state3.clone(), Some(state2.clone()), "Third", "user").await.unwrap();

        // Get ancestry
        let ancestry = vcs.get_ancestry(&state3).await;
        assert_eq!(ancestry.len(), 3);
        assert_eq!(ancestry[0], state3);
        assert_eq!(ancestry[1], state2);
        assert_eq!(ancestry[2], state1);
    }

    #[tokio::test]
    async fn test_common_ancestor() {
        let vcs = StateVersionControl::new();

        let root = StateId(Uuid::new_v4().to_string());
        let branch_a = StateId(Uuid::new_v4().to_string());
        let branch_b = StateId(Uuid::new_v4().to_string());

        // Create branching structure
        //     root
        //    /    \
        //   a      b
        vcs.record_state(root.clone(), None, "Root", "user").await.unwrap();
        vcs.record_state(branch_a.clone(), Some(root.clone()), "Branch A", "user").await.unwrap();
        vcs.record_state(branch_b.clone(), Some(root.clone()), "Branch B", "user").await.unwrap();

        // Find common ancestor
        let ancestor = vcs.find_common_ancestor(&branch_a, &branch_b).await;
        assert_eq!(ancestor, Some(root.clone()));
    }

    #[tokio::test]
    async fn test_version_stats() {
        let vcs = StateVersionControl::new();

        let state1 = StateId(Uuid::new_v4().to_string());
        let state2 = StateId(Uuid::new_v4().to_string());

        vcs.record_state(state1, None, "First", "user").await.unwrap();
        vcs.record_state(state2, None, "Second", "user").await.unwrap();
        vcs.create_branch("main", state2.clone()).await.unwrap();
        vcs.create_tag("v1.0", state2.clone()).await.unwrap();

        let stats = vcs.get_stats().await;

        assert_eq!(stats.total_states, 2);
        assert_eq!(stats.total_branches, 1);
        assert_eq!(stats.total_tags, 1);
        assert!(stats.oldest_state.is_some());
        assert!(stats.newest_state.is_some());
    }
}
