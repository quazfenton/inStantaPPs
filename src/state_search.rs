//! State Search Module
//!
//! Provides full-text search and filtering for states.
//! Indexes state metadata for fast queries.
//!
//! # Features
//!
//! - Full-text search on labels
//! - Filter by date range
//! - Filter by tags
//! - Sort by various fields
//! - Pagination support

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

use crate::model::StateId;

/// Search query parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Text to search for (matches labels)
    pub query: Option<String>,
    /// Filter by tags (must have all specified tags)
    pub tags: Option<Vec<String>>,
    /// Filter by creation date (after)
    pub created_after: Option<DateTime<Utc>>,
    /// Filter by creation date (before)
    pub created_before: Option<DateTime<Utc>>,
    /// Sort field
    pub sort_by: SortField,
    /// Sort order
    pub sort_order: SortOrder,
    /// Page number (0-indexed)
    pub page: u32,
    /// Items per page
    pub page_size: u32,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            query: None,
            tags: None,
            created_after: None,
            created_before: None,
            sort_by: SortField::CreatedAt,
            sort_order: SortOrder::Desc,
            page: 0,
            page_size: 20,
        }
    }
}

/// Sort fields
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    CreatedAt,
    Label,
    Size,
}

/// Sort order
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Asc,
    Desc,
}

/// Search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Total matching results
    pub total: usize,
    /// Current page
    pub page: u32,
    /// Page size
    pub page_size: u32,
    /// Total pages
    pub total_pages: u32,
    /// Results on this page
    pub results: Vec<StateIndexEntry>,
}

/// Indexed state entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateIndexEntry {
    pub state_id: StateId,
    pub label: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub size_bytes: usize,
    pub owner: String,
}

/// Search index for fast state queries
pub struct StateSearchIndex {
    /// All indexed states
    entries: Arc<RwLock<HashMap<StateId, StateIndexEntry>>>,
    /// Inverted index for labels (word -> state_ids)
    label_index: Arc<RwLock<HashMap<String, HashSet<StateId>>>>,
    /// Inverted index for tags (tag -> state_ids)
    tag_index: Arc<RwLock<HashMap<String, HashSet<StateId>>>>,
}

impl StateSearchIndex {
    /// Create a new search index
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            label_index: Arc::new(RwLock::new(HashMap::new())),
            tag_index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add or update a state in the index
    pub async fn index_state(&self, entry: StateIndexEntry) {
        let state_id = entry.state_id.clone();

        // Remove old entry if exists
        self.remove_state(&state_id).await;

        // Add new entry
        {
            let mut entries = self.entries.write().await;
            entries.insert(state_id.clone(), entry.clone());
        }

        // Index label words
        {
            let mut label_index = self.label_index.write().await;
            for word in self.tokenize(&entry.label) {
                label_index
                    .entry(word)
                    .or_insert_with(HashSet::new)
                    .insert(state_id.clone());
            }
        }

        // Index tags
        {
            let mut tag_index = self.tag_index.write().await;
            for tag in &entry.tags {
                tag_index
                    .entry(tag.clone())
                    .or_insert_with(HashSet::new)
                    .insert(state_id.clone());
            }
        }

        debug!("Indexed state {} with label '{}'", state_id.0, entry.label);
    }

    /// Remove a state from the index
    pub async fn remove_state(&self, state_id: &StateId) {
        // Get old entry
        let old_entry = {
            let entries = self.entries.read().await;
            entries.get(state_id).cloned()
        };

        if let Some(entry) = old_entry {
            // Remove from label index
            {
                let mut label_index = self.label_index.write().await;
                for word in self.tokenize(&entry.label) {
                    if let Some(state_ids) = label_index.get_mut(&word) {
                        state_ids.remove(state_id);
                        if state_ids.is_empty() {
                            label_index.remove(&word);
                        }
                    }
                }
            }

            // Remove from tag index
            {
                let mut tag_index = self.tag_index.write().await;
                for tag in &entry.tags {
                    if let Some(state_ids) = tag_index.get_mut(tag) {
                        state_ids.remove(state_id);
                        if state_ids.is_empty() {
                            tag_index.remove(tag);
                        }
                    }
                }
            }

            // Remove entry
            {
                let mut entries = self.entries.write().await;
                entries.remove(state_id);
            }

            debug!("Removed state {} from index", state_id.0);
        }
    }

    /// Search for states
    pub async fn search(&self, query: &SearchQuery) -> SearchResult {
        let entries = self.entries.read().await;
        let mut matching: Vec<&StateIndexEntry> = Vec::new();

        // Filter by query text
        if let Some(query_text) = &query.query {
            let query_words: HashSet<_> = self.tokenize(query_text);
            for entry in entries.values() {
                let entry_words: HashSet<_> = self.tokenize(&entry.label);
                if query_words.iter().any(|w| entry_words.contains(w)) {
                    matching.push(entry);
                }
            }
        } else {
            matching = entries.values().collect();
        }

        // Filter by tags
        if let Some(tags) = &query.tags {
            let tag_index = self.tag_index.read().await;
            matching.retain(|entry| {
                tags.iter().all(|tag| {
                    entry.tags.contains(tag)
                        || tag_index.get(tag).map(|s| s.contains(&entry.state_id)).unwrap_or(false)
                })
            });
        }

        // Filter by date range
        if let Some(after) = query.created_after {
            matching.retain(|e| e.created_at >= after);
        }
        if let Some(before) = query.created_before {
            matching.retain(|e| e.created_at <= before);
        }

        // Sort
        matching.sort_by(|a, b| match query.sort_by {
            SortField::CreatedAt => {
                let cmp = a.created_at.cmp(&b.created_at);
                match query.sort_order {
                    SortOrder::Asc => cmp,
                    SortOrder::Desc => cmp.reverse(),
                }
            }
            SortField::Label => {
                let cmp = a.label.cmp(&b.label);
                match query.sort_order {
                    SortOrder::Asc => cmp,
                    SortOrder::Desc => cmp.reverse(),
                }
            }
            SortField::Size => {
                let cmp = a.size_bytes.cmp(&b.size_bytes);
                match query.sort_order {
                    SortOrder::Asc => cmp,
                    SortOrder::Desc => cmp.reverse(),
                }
            }
        });

        // Paginate
        let total = matching.len();
        let total_pages = ((total as u32) + query.page_size - 1) / query.page_size;
        let start = (query.page * query.page_size) as usize;
        let end = (start + query.page_size as usize).min(total);

        let results: Vec<StateIndexEntry> = matching[start..end]
            .iter()
            .map(|e| (*e).clone())
            .collect();

        SearchResult {
            total,
            page: query.page,
            page_size: query.page_size,
            total_pages,
            results,
        }
    }

    /// Get index statistics
    pub async fn get_stats(&self) -> IndexStats {
        let entries = self.entries.read().await;
        let label_index = self.label_index.read().await;
        let tag_index = self.tag_index.read().await;

        IndexStats {
            total_states: entries.len(),
            unique_words: label_index.len(),
            unique_tags: tag_index.len(),
        }
    }

    /// Tokenize text into searchable words
    fn tokenize(&self, text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty() && s.len() >= 2)
            .map(|s| s.to_string())
            .collect()
    }
}

impl Default for StateSearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Index statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    pub total_states: usize,
    pub unique_words: usize,
    pub unique_tags: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn create_test_entry(label: &str, tags: Vec<&str>) -> StateIndexEntry {
        StateIndexEntry {
            state_id: StateId(Uuid::new_v4().to_string()),
            label: label.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            created_at: Utc::now(),
            size_bytes: 1024,
            owner: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn test_index_and_search() {
        let index = StateSearchIndex::new();

        // Index some states
        index.index_state(create_test_entry("production database", vec!["prod", "db"])).await;
        index.index_state(create_test_entry("development server", vec!["dev", "server"])).await;
        index.index_state(create_test_entry("staging database", vec!["staging", "db"])).await;

        // Search by text
        let query = SearchQuery {
            query: Some("database".to_string()),
            ..Default::default()
        };
        let result = index.search(&query).await;
        assert_eq!(result.total, 2);

        // Search by tag
        let query = SearchQuery {
            tags: Some(vec!["db".to_string()]),
            ..Default::default()
        };
        let result = index.search(&query).await;
        assert_eq!(result.total, 2);

        // Combined search
        let query = SearchQuery {
            query: Some("database".to_string()),
            tags: Some(vec!["prod".to_string()]),
            ..Default::default()
        };
        let result = index.search(&query).await;
        assert_eq!(result.total, 1);
    }

    #[tokio::test]
    async fn test_pagination() {
        let index = StateSearchIndex::new();

        // Index 50 states
        for i in 0..50 {
            index.index_state(create_test_entry(&format!("state {}", i), vec!["test"])).await;
        }

        // Get first page
        let query = SearchQuery {
            page: 0,
            page_size: 20,
            ..Default::default()
        };
        let result = index.search(&query).await;
        assert_eq!(result.total, 50);
        assert_eq!(result.results.len(), 20);
        assert_eq!(result.total_pages, 3);

        // Get second page
        let query = SearchQuery {
            page: 1,
            page_size: 20,
            ..Default::default()
        };
        let result = index.search(&query).await;
        assert_eq!(result.results.len(), 20);

        // Get third page
        let query = SearchQuery {
            page: 2,
            page_size: 20,
            ..Default::default()
        };
        let result = index.search(&query).await;
        assert_eq!(result.results.len(), 10);
    }

    #[tokio::test]
    async fn test_remove_state() {
        let index = StateSearchIndex::new();

        let entry = create_test_entry("test state", vec!["test"]);
        let state_id = entry.state_id.clone();

        index.index_state(entry).await;

        // Verify indexed
        let query = SearchQuery::default();
        assert_eq!(index.search(&query).await.total, 1);

        // Remove
        index.remove_state(&state_id).await;

        // Verify removed
        assert_eq!(index.search(&query).await.total, 0);
    }

    #[tokio::test]
    async fn test_sorting() {
        let index = StateSearchIndex::new();

        index.index_state(create_test_entry("aaa", vec![])).await;
        index.index_state(create_test_entry("bbb", vec![])).await;
        index.index_state(create_test_entry("ccc", vec![])).await;

        // Sort by label ascending
        let query = SearchQuery {
            sort_by: SortField::Label,
            sort_order: SortOrder::Asc,
            ..Default::default()
        };
        let result = index.search(&query).await;
        assert_eq!(result.results[0].label, "aaa");
        assert_eq!(result.results[2].label, "ccc");

        // Sort by label descending
        let query = SearchQuery {
            sort_by: SortField::Label,
            sort_order: SortOrder::Desc,
            ..Default::default()
        };
        let result = index.search(&query).await;
        assert_eq!(result.results[0].label, "ccc");
        assert_eq!(result.results[2].label, "aaa");
    }
}
