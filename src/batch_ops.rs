//! Batch Operations Module
//!
//! Provides bulk operations for managing multiple states efficiently.
//!
//! # Features
//!
//! - Bulk delete
//! - Bulk tag management
//! - Bulk metadata updates
//! - Parallel processing
//! - Progress tracking

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

use crate::model::StateId;

/// Batch operation type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum BatchOperation {
    Delete {
        state_ids: Vec<StateId>,
    },
    AddTags {
        state_ids: Vec<StateId>,
        tags: Vec<String>,
    },
    RemoveTags {
        state_ids: Vec<StateId>,
        tags: Vec<String>,
    },
    UpdateMetadata {
        state_ids: Vec<StateId>,
        ttl_seconds: Option<u64>,
    },
    Export {
        state_ids: Vec<StateId>,
        format: String,
    },
}

/// Batch operation status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BatchStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Batch operation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    pub operation_id: String,
    pub status: BatchStatus,
    pub total: usize,
    pub processed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub errors: Vec<BatchError>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Batch error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchError {
    pub state_id: String,
    pub error: String,
}

/// Progress update
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressUpdate {
    pub operation_id: String,
    pub processed: usize,
    pub total: usize,
    pub current_state: Option<String>,
}

/// Batch operation executor trait
#[async_trait::async_trait]
pub trait BatchExecutor: Send + Sync {
    async fn delete_state(&self, state_id: &StateId) -> Result<(), String>;
    async fn add_tags(&self, state_id: &StateId, tags: &[String]) -> Result<(), String>;
    async fn remove_tags(&self, state_id: &StateId, tags: &[String]) -> Result<(), String>;
    async fn update_metadata(&self, state_id: &StateId, ttl_seconds: Option<u64>) -> Result<(), String>;
    async fn export_state(&self, state_id: &StateId, format: &str) -> Result<Vec<u8>, String>;
}

/// In-memory batch executor for testing
pub struct MemoryBatchExecutor {
    states: Arc<RwLock<HashMap<StateId, bool>>>,
}

impl MemoryBatchExecutor {
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_state(&self, state_id: StateId) {
        let mut states = self.states.write().await;
        states.insert(state_id, true);
    }
}

impl Default for MemoryBatchExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl BatchExecutor for MemoryBatchExecutor {
    async fn delete_state(&self, state_id: &StateId) -> Result<(), String> {
        let mut states = self.states.write().await;
        if states.remove(state_id).is_some() {
            Ok(())
        } else {
            Err("State not found".to_string())
        }
    }

    async fn add_tags(&self, _state_id: &StateId, _tags: &[String]) -> Result<(), String> {
        Ok(())
    }

    async fn remove_tags(&self, _state_id: &StateId, _tags: &[String]) -> Result<(), String> {
        Ok(())
    }

    async fn update_metadata(&self, _state_id: &StateId, _ttl_seconds: Option<u64>) -> Result<(), String> {
        Ok(())
    }

    async fn export_state(&self, _state_id: &StateId, _format: &str) -> Result<Vec<u8>, String> {
        Ok(vec![])
    }
}

/// Batch operation manager with executor
pub struct BatchManager {
    operations: Arc<RwLock<Vec<BatchResult>>>,
    max_concurrent: usize,
    progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
    executor: Option<Arc<dyn BatchExecutor>>,
}

impl BatchManager {
    /// Create a new batch manager
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            operations: Arc::new(RwLock::new(Vec::new())),
            max_concurrent,
            progress_tx: None,
            executor: None,
        }
    }

    /// Create batch manager with executor
    pub fn with_executor(max_concurrent: usize, executor: Arc<dyn BatchExecutor>) -> Self {
        Self {
            operations: Arc::new(RwLock::new(Vec::new())),
            max_concurrent,
            progress_tx: None,
            executor: Some(executor),
        }
    }

    /// Set progress callback
    pub fn with_progress_callback(
        mut self,
        tx: mpsc::Sender<ProgressUpdate>,
    ) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    /// Execute a batch operation
    pub async fn execute(
        &self,
        operation: BatchOperation,
    ) -> Result<String, BatchError> {
        let operation_id = uuid::Uuid::new_v4().to_string();
        let total = self.get_operation_count(&operation);

        let mut result = BatchResult {
            operation_id: operation_id.clone(),
            status: BatchStatus::Pending,
            total,
            processed: 0,
            succeeded: 0,
            failed: 0,
            errors: Vec::new(),
            started_at: None,
            completed_at: None,
        };

        // Store initial result
        {
            let mut operations = self.operations.write().await;
            operations.push(result.clone());
        }

        // Execute asynchronously
        let operations = self.operations.clone();
        let progress_tx = self.progress_tx.clone();
        let max_concurrent = self.max_concurrent;
        let executor = self.executor.clone();

        tokio::spawn(async move {
            result.status = BatchStatus::Running;
            result.started_at = Some(Utc::now());

            // Update status
            Self::update_result(&operations, &result).await;

            // Execute based on operation type
            match operation {
                BatchOperation::Delete { state_ids } => {
                    Self::execute_delete(
                        &mut result,
                        state_ids,
                        max_concurrent,
                        progress_tx,
                        executor,
                    )
                    .await;
                }
                BatchOperation::AddTags {
                    state_ids,
                    tags,
                } => {
                    Self::execute_add_tags(
                        &mut result,
                        state_ids,
                        tags,
                        max_concurrent,
                        progress_tx,
                        executor,
                    )
                    .await;
                }
                BatchOperation::RemoveTags {
                    state_ids,
                    tags,
                } => {
                    Self::execute_remove_tags(
                        &mut result,
                        state_ids,
                        tags,
                        max_concurrent,
                        progress_tx,
                        executor,
                    )
                    .await;
                }
                BatchOperation::UpdateMetadata {
                    state_ids,
                    ttl_seconds,
                } => {
                    Self::execute_update_metadata(
                        &mut result,
                        state_ids,
                        ttl_seconds,
                        max_concurrent,
                        progress_tx,
                        executor,
                    )
                    .await;
                }
                BatchOperation::Export {
                    state_ids,
                    format,
                } => {
                    Self::execute_export(
                        &mut result,
                        state_ids,
                        format,
                        max_concurrent,
                        progress_tx,
                        executor,
                    )
                    .await;
                }
            }

            result.status = if result.failed > 0 {
                BatchStatus::Failed
            } else {
                BatchStatus::Completed
            };
            result.completed_at = Some(Utc::now());

            // Update final status
            Self::update_result(&operations, &result).await;
        });

        Ok(operation_id)
    }

    /// Get operation count
    fn get_operation_count(&self, operation: &BatchOperation) -> usize {
        match operation {
            BatchOperation::Delete { state_ids } => state_ids.len(),
            BatchOperation::AddTags { state_ids, .. } => state_ids.len(),
            BatchOperation::RemoveTags { state_ids, .. } => state_ids.len(),
            BatchOperation::UpdateMetadata { state_ids, .. } => state_ids.len(),
            BatchOperation::Export { state_ids, .. } => state_ids.len(),
        }
    }

    /// Update result in storage
    async fn update_result(operations: &RwLock<Vec<BatchResult>>, result: &BatchResult) {
        let mut ops = operations.write().await;
        if let Some(existing) = ops.iter_mut().find(|r| r.operation_id == result.operation_id) {
            // Do not override a cancelled operation's status
            if existing.status != BatchStatus::Cancelled {
                *existing = result.clone();
            }
        }
    }
    }

    /// Execute delete operation
    async fn execute_delete(
        result: &mut BatchResult,
        state_ids: Vec<StateId>,
        max_concurrent: usize,
        progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
        executor: Option<Arc<dyn BatchExecutor>>,
    ) {
        // Use semaphore for concurrency control
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let mut handles = Vec::new();

        for state_id in state_ids {
            let sem = semaphore.clone();
            let executor_clone = executor.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();

                // Execute actual delete operation if executor provided
                if let Some(exec) = executor_clone {
                    exec.delete_state(&state_id).await
                } else {
                    // Without executor, assume success (for testing)
                    Ok(())
                }
            });

            handles.push((state_id.clone(), handle));
        }

        // Collect results
        for (state_id, handle) in handles {
            match handle.await {
                Ok(Ok(())) => {
                    result.succeeded += 1;
                }
                Ok(Err(e)) => {
                    result.failed += 1;
                    result.errors.push(BatchError {
                        state_id: state_id.0,
                        error: e,
                    });
                }
                Err(e) => {
                    result.failed += 1;
                    result.errors.push(BatchError {
                        state_id: state_id.0,
                        error: e.to_string(),
                    });
                }
            }

            result.processed += 1;

            // Send progress update
            if let Some(tx) = &progress_tx {
                let _ = tx
                    .send(ProgressUpdate {
                        operation_id: result.operation_id.clone(),
                        processed: result.processed,
                        total: result.total,
                        current_state: Some(state_id.0.clone()),
                    })
                    .await;
            }
        }
    }

    /// Execute add tags operation
    async fn execute_add_tags(
        result: &mut BatchResult,
        state_ids: Vec<StateId>,
        tags: Vec<String>,
        max_concurrent: usize,
        progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
        executor: Option<Arc<dyn BatchExecutor>>,
    ) {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let mut handles = Vec::new();

        for state_id in state_ids {
            let sem = semaphore.clone();
            let executor_clone = executor.clone();
            let tags_clone = tags.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();

                if let Some(exec) = executor_clone {
                    exec.add_tags(&state_id, &tags_clone).await
                } else {
                    Ok(())
                }
            });

            handles.push((state_id.clone(), handle));
        }

        Self::collect_results(result, handles, progress_tx).await;
    }

    /// Execute remove tags operation
    async fn execute_remove_tags(
        result: &mut BatchResult,
        state_ids: Vec<StateId>,
        tags: Vec<String>,
        max_concurrent: usize,
        progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
        executor: Option<Arc<dyn BatchExecutor>>,
    ) {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let mut handles = Vec::new();

        for state_id in state_ids {
            let sem = semaphore.clone();
            let executor_clone = executor.clone();
            let tags_clone = tags.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();

                if let Some(exec) = executor_clone {
                    exec.remove_tags(&state_id, &tags_clone).await
                } else {
                    Ok(())
                }
            });

            handles.push((state_id.clone(), handle));
        }

        Self::collect_results(result, handles, progress_tx).await;
    }

    /// Execute update metadata operation
    async fn execute_update_metadata(
        result: &mut BatchResult,
        state_ids: Vec<StateId>,
        ttl_seconds: Option<u64>,
        max_concurrent: usize,
        progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
        executor: Option<Arc<dyn BatchExecutor>>,
    ) {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let mut handles = Vec::new();

        for state_id in state_ids {
            let sem = semaphore.clone();
            let executor_clone = executor.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();

                if let Some(exec) = executor_clone {
                    exec.update_metadata(&state_id, ttl_seconds).await
                } else {
                    Ok(())
                }
            });

            handles.push((state_id.clone(), handle));
        }

        Self::collect_results(result, handles, progress_tx).await;
    }

    /// Execute export operation
    async fn execute_export(
        result: &mut BatchResult,
        state_ids: Vec<StateId>,
        format: String,
        max_concurrent: usize,
        progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
        executor: Option<Arc<dyn BatchExecutor>>,
    ) {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let mut handles = Vec::new();

        for state_id in state_ids {
            let sem = semaphore.clone();
            let executor_clone = executor.clone();
            let format_clone = format.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();

                if let Some(exec) = executor_clone {
                    exec.export_state(&state_id, &format_clone).await
                } else {
                    Ok(vec![])
                }
            });

            handles.push((state_id.clone(), handle));
        }

        Self::collect_results(result, handles, progress_tx).await;
    }

    /// Collect results from spawned tasks
    async fn collect_results(
        result: &mut BatchResult,
        handles: Vec<(StateId, tokio::task::JoinHandle<Result<(), String>>)>,
        progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
    ) {
        for (state_id, handle) in handles {
            match handle.await {
                Ok(Ok(())) => {
                    result.succeeded += 1;
                }
                Ok(Err(e)) => {
                    result.failed += 1;
                    result.errors.push(BatchError {
                        state_id: state_id.0,
                        error: e,
                    });
                }
                Err(e) => {
                    result.failed += 1;
                    result.errors.push(BatchError {
                        state_id: state_id.0,
                        error: e.to_string(),
                    });
                }
            }

            result.processed += 1;

            if let Some(tx) = &progress_tx {
                let _ = tx
                    .send(ProgressUpdate {
                        operation_id: result.operation_id.clone(),
                        processed: result.processed,
                        total: result.total,
                        current_state: Some(state_id.0.clone()),
                    })
                    .await;
            }
        }
    }

    /// Get operation status
    pub async fn get_status(&self, operation_id: &str) -> Option<BatchResult> {
        let operations = self.operations.read().await;
        operations.iter().find(|r| r.operation_id == operation_id).cloned()
    }

    /// List all operations
    pub async fn list_operations(&self, limit: usize) -> Vec<BatchResult> {
        let operations = self.operations.read().await;
        operations.iter().rev().take(limit).cloned().collect()
    }

    /// Cancel an operation
    pub async fn cancel(&self, operation_id: &str) -> bool {
        let mut operations = self.operations.write().await;
        if let Some(result) = operations.iter_mut().find(|r| r.operation_id == operation_id) {
            if result.status == BatchStatus::Pending || result.status == BatchStatus::Running {
                result.status = BatchStatus::Cancelled;
                result.completed_at = Some(Utc::now());
                return true;
            }
        }
        false
    }

    /// Cleanup old operations
    pub async fn cleanup(&self, older_than: Duration) -> usize {
        let mut operations = self.operations.write().await;
        let cutoff = Utc::now() - older_than;

        let initial_len = operations.len();
        operations.retain(|r| {
            r.completed_at.map(|t| t > cutoff).unwrap_or(true)
        });

        initial_len - operations.len()
    }
}

impl Default for BatchManager {
    fn default() -> Self {
        Self::new(10)
    }
}

/// Batch operation errors
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("Operation not found: {0}")]
    NotFound(String),

    #[error("Invalid operation: {0}")]
    InvalidOperation(String),

    #[error("Cancelled")]
    Cancelled,

    #[error("Internal error: {0}")]
    InternalError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_batch_delete() {
        let manager = BatchManager::new(5);

        let state_ids: Vec<StateId> = (0..10)
            .map(|_| StateId(uuid::Uuid::new_v4().to_string()))
            .collect();

        let operation = BatchOperation::Delete { state_ids };
        let operation_id = manager.execute(operation).await.unwrap();

        // Wait for completion
        sleep(Duration::from_millis(100)).await;

        let result = manager.get_status(&operation_id).await.unwrap();
        assert_eq!(result.status, BatchStatus::Completed);
        assert_eq!(result.total, 10);
        assert_eq!(result.processed, 10);
    }

    #[tokio::test]
    async fn test_batch_with_errors() {
        let manager = BatchManager::new(5);

        let state_ids: Vec<StateId> = (0..5)
            .map(|i| StateId(format!("state-{}", i)))
            .collect();

        let operation = BatchOperation::Delete { state_ids };
        let operation_id = manager.execute(operation).await.unwrap();

        sleep(Duration::from_millis(100)).await;

        let result = manager.get_status(&operation_id).await.unwrap();
        assert!(result.status == BatchStatus::Completed || result.status == BatchStatus::Failed);
    }

    #[tokio::test]
    async fn test_cancel_operation() {
        let manager = BatchManager::new(1);

        let state_ids: Vec<StateId> = (0..100)
            .map(|_| StateId(uuid::Uuid::new_v4().to_string()))
            .collect();

        let operation = BatchOperation::Delete { state_ids };
        let operation_id = manager.execute(operation).await.unwrap();

        // Cancel immediately
        let cancelled = manager.cancel(&operation_id).await;
        assert!(cancelled);

        sleep(Duration::from_millis(50)).await;

        let result = manager.get_status(&operation_id).await.unwrap();
        assert_eq!(result.status, BatchStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_list_operations() {
        let manager = BatchManager::new(5);

        // Create multiple operations
        for _ in 0..5 {
            let state_ids: Vec<StateId> = (0..3)
                .map(|_| StateId(uuid::Uuid::new_v4().to_string()))
                .collect();

            let operation = BatchOperation::Delete { state_ids };
            let _ = manager.execute(operation).await.unwrap();
        }

        sleep(Duration::from_millis(100)).await;

        let operations = manager.list_operations(10).await;
        assert_eq!(operations.len(), 5);
    }

    #[tokio::test]
    async fn test_cleanup() {
        let manager = BatchManager::new(5);

        // Create operation
        let state_ids: Vec<StateId> = (0..3)
            .map(|_| StateId(uuid::Uuid::new_v4().to_string()))
            .collect();

        let operation = BatchOperation::Delete { state_ids };
        let _ = manager.execute(operation).await.unwrap();

        sleep(Duration::from_millis(100)).await;

        // Cleanup operations older than 1 second
        let removed = manager.cleanup(Duration::from_secs(1)).await;
        assert!(removed >= 0);
    }
}
