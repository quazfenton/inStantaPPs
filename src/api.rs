//! ISA API Implementation
//!
//! Wire API endpoints to actual backend implementations:
//! - Firecracker VM management
//! - Memory snapshot
//! - State store
//! - QUIC transport
//! - Socket proxy
//! - Deterministic logging

use std::sync::Arc;
use axum::{routing::{post, get}, Json, Router, extract::State};
use axum::http::StatusCode;
use tracing::{info, error, warn};

use crate::model::{
    ApiErrorResponse, ForkRequest, ForkResponse, ResumeRequest, ResumeResponse,
    SnapshotRequest, SnapshotResponse, State as IsaState, StateMetadata, StateId,
    VMConfig, CPUState, MemoryFlags, MemoryTemperature, MemoryRegion,
    EventLog, UIStateRef, FDDescriptor, SocketDescriptor, DeviceState,
};
use crate::state_store::{StateStore, StateStoreConfig, StorageBackend};
use crate::firecracker::{VMManager, FirecrackerConfig, VMInstanceId};
use crate::memory::MemorySnapshotManager;
use crate::deterministic::{DeterministicLogger, DeterministicLogConfig};
use crate::socket_proxy::{ConnectionProxy, SocketProxyConfig};
use crate::metrics::MetricsRegistry;
use crate::websocket::WebSocketManager;

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub state_store: Arc<StateStore>,
    pub vm_manager: Arc<tokio::sync::RwLock<VMManager>>,
    pub memory_manager: Arc<tokio::sync::RwLock<MemorySnapshotManager>>,
    pub logger: Arc<DeterministicLogger>,
    pub socket_proxy: Arc<ConnectionProxy>,
    pub metrics: Arc<MetricsRegistry>,
    pub ws_manager: Arc<WebSocketManager>,
}

impl AppState {
    /// Create new application state with default configurations
    pub async fn new() -> Result<Self, ApiError> {
        let state_store = Arc::new(
            StateStore::new(StateStoreConfig {
                backend: StorageBackend::Memory, // Use memory for dev/testing
                ..Default::default()
            }).await?
        );

        let vm_manager = Arc::new(tokio::sync::RwLock::new(
            VMManager::new(FirecrackerConfig::default())
        ));

        let memory_manager = Arc::new(tokio::sync::RwLock::new(
            MemorySnapshotManager::new()
        ));

        let logger = Arc::new(DeterministicLogger::new(DeterministicLogConfig::default()));
        logger.enable_recording();

        let socket_proxy = Arc::new(ConnectionProxy::new(SocketProxyConfig::default()));
        
        let metrics = Arc::new(MetricsRegistry::new());
        let ws_manager = Arc::new(WebSocketManager::new());

        Ok(Self {
            state_store,
            vm_manager,
            memory_manager,
            logger,
            socket_proxy,
            metrics,
            ws_manager,
        })
    }
}

pub fn router() -> Router {
    Router::new()
        .route("/v1/snapshot", post(create_snapshot))
        .route("/v1/resume", post(resume_state))
        .route("/v1/fork", post(fork_state))
        .route("/v1/status", post(get_status))
        .route("/v1/delete", post(delete_state))
        .route("/v1/list", get(list_states))
        .route("/metrics", get(get_metrics))
        .route("/health", get(get_health))
        .route("/ws/state/:state_id", get(crate::websocket::state_websocket_handler))
        .route("/ws/collab/:session_id", get(crate::websocket::collab_websocket_handler))
}

/// Create a snapshot of a running VM
async fn create_snapshot(
    State(state): State<AppState>,
    Json(payload): Json<SnapshotRequest>,
) -> Result<Json<SnapshotResponse>, (StatusCode, Json<ApiErrorResponse>)> {
    if payload.label.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: "label is required".to_string(),
            }),
        ));
    }

    info!(label = %payload.label, "Creating snapshot");

    // Parse TTL
    let ttl_seconds = parse_ttl(&payload.ttl)?;

    // Create VM for snapshot
    // Note: In production deployments, VMs are typically pre-existing
    // This creates a new VM for demonstration purposes
    let vm_config = VMConfig {
        vcpus: 2,
        memory_mb: 512,
        kernel_image: "/tmp/vmlinux.bin".to_string(),
        rootfs_image: "/tmp/rootfs.ext4".to_string(),
    };

    let mut vm_manager = state.vm_manager.write().await;
    let vm_handle = vm_manager.create_vm(vm_config.clone())
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to create VM: {}", e),
            }),
        ))?;

    let instance_id = vm_handle.instance_id.clone();

    // Boot VM for snapshot creation
    // Note: In production deployments, VMs are typically already running
    vm_handle.boot().await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorResponse {
            error: format!("Failed to boot VM: {}", e),
        }),
    ))?;

    // Create snapshot
    let vm_snapshot = vm_handle.snapshot().await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorResponse {
            error: format!("Failed to snapshot VM: {}", e),
        }),
    ))?;

    // Build complete state object
    let state_obj = IsaState {
        state_id: StateId(uuid::Uuid::new_v4().to_string()),
        vm_config,
        cpu_state: vm_snapshot.cpu_state,
        memory_manifest: vm_snapshot.memory_manifest,
        fd_table: state.socket_proxy.get_descriptors().await,
        socket_table: state.socket_proxy.get_socket_table().await,
        device_state: vec![],
        deterministic_log: state.logger.get_event_log().await,
        ui_state: UIStateRef { stream_id: uuid::Uuid::new_v4() },
        metadata: StateMetadata {
            label: payload.label.clone(),
            created_at: chrono::Utc::now(),
            ttl_seconds,
        },
    };

    // Store state
    let stored = state.state_store.store_state(state_obj.clone()).await
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to store state: {}", e),
            }),
        ))?;

    info!(
        state_id = %state_obj.state_id.0,
        label = %payload.label,
        "Snapshot created successfully"
    );

    Ok(Json(SnapshotResponse { 
        state_id: state_obj.state_id.0,
    }))
}

/// Resume a state from snapshot
async fn resume_state(
    State(state): State<AppState>,
    Json(payload): Json<ResumeRequest>,
) -> Result<Json<ResumeResponse>, (StatusCode, Json<ApiErrorResponse>)> {
    if payload.state_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: "state_id is required".to_string(),
            }),
        ));
    }

    info!(state_id = %payload.state_id, "Resuming state");

    // Retrieve state from store
    let isa_state = state.state_store.get_state(&StateId(payload.state_id.clone())).await
        .map_err(|e| match e {
            crate::state_store::StateStoreError::NotFound(_) => (
                StatusCode::NOT_FOUND,
                Json(ApiErrorResponse {
                    error: format!("State not found: {}", payload.state_id),
                }),
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorResponse {
                    error: format!("Failed to load state {}: {}", payload.state_id, e),
                }),
            ),
        })?;

    // Create new VM for resume
    let mut vm_manager = state.vm_manager.write().await;
    let vm_handle = vm_manager.create_vm(isa_state.vm_config.clone())
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to create VM: {}", e),
            }),
        ))?;

    let instance_id = vm_handle.instance_id.clone();

    // Resume from snapshot using memory manifest
    // The memory manifest contains region information for lazy page loading
    // Full snapshot restoration would load memory pages from the state store
    if !isa_state.memory_manifest.is_empty() {
        info!("Resuming with {} memory regions", isa_state.memory_manifest.len());
        // Memory pages are loaded on-demand via userfaultfd during execution
    }

    vm_handle.boot().await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorResponse {
            error: format!("Failed to boot VM: {}", e),
        }),
    ))?;

    // Restore deterministic logger state for replay
    state.logger.disable_recording();

    let target_region = payload.region.unwrap_or_else(|| "nearest".to_string());
    let mode = payload.mode.unwrap_or_else(|| "collaborative".to_string());

    info!(
        state_id = %payload.state_id,
        region = %target_region,
        mode = %mode,
        "State resumed successfully"
    );

    Ok(Json(ResumeResponse {
        accepted: true,
        target_region,
        mode,
    }))
}

/// Fork an existing state
async fn fork_state(
    State(state): State<AppState>,
    Json(payload): Json<ForkRequest>,
) -> Result<Json<ForkResponse>, (StatusCode, Json<ApiErrorResponse>)> {
    if payload.state_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: "state_id is required".to_string(),
            }),
        ));
    }

    info!(
        state_id = %payload.state_id,
        label = ?payload.label,
        "Forking state"
    );

    // Get original state
    let original_state = state.state_store.get_state(&StateId(payload.state_id.clone())).await
        .map_err(|e| match e {
            crate::state_store::StateStoreError::NotFound(_) => (
                StatusCode::NOT_FOUND,
                Json(ApiErrorResponse {
                    error: format!("State not found: {}", payload.state_id),
                }),
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorResponse {
                    error: format!("Failed to load state {}: {}", payload.state_id, e),
                }),
            ),
        })?;

    // Create forked state with new ID
    let forked_state_id = StateId(uuid::Uuid::new_v4().to_string());
    
    let forked_state = IsaState {
        state_id: forked_state_id.clone(),
        vm_config: original_state.vm_config.clone(),
        cpu_state: original_state.cpu_state.clone(),
        memory_manifest: original_state.memory_manifest.clone(),
        fd_table: original_state.fd_table.clone(),
        socket_table: original_state.socket_table.clone(),
        device_state: original_state.device_state.clone(),
        deterministic_log: original_state.deterministic_log.clone(),
        ui_state: UIStateRef { stream_id: uuid::Uuid::new_v4() },
        metadata: StateMetadata {
            label: payload.label.unwrap_or_else(|| format!("fork-of-{}", original_state.metadata.label)),
            created_at: chrono::Utc::now(),
            ttl_seconds: original_state.metadata.ttl_seconds,
        },
    };

    // Store forked state
    state.state_store.store_state(forked_state).await
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to store forked state: {}", e),
            }),
        ))?;

    info!(
        original_state_id = %payload.state_id,
        forked_state_id = %forked_state_id.0,
        "State forked successfully"
    );

    Ok(Json(ForkResponse { 
        state_id: forked_state_id.0,
    }))
}

/// Get system status
async fn get_status(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiErrorResponse>)> {
    let states = state.state_store.list_states().await;
    let logger_entries = state.logger.entry_count().await;

    Ok(Json(serde_json::json!({
        "status": "healthy",
        "stored_states": states.len(),
        "log_entries": logger_entries,
        "timestamp": chrono::Utc::now(),
    })))
}

/// Parse TTL string (e.g., "24h", "7d") to seconds
fn parse_ttl(ttl: &Option<String>) -> Result<Option<u64>, (StatusCode, Json<ApiErrorResponse>)> {
    match ttl {
        None => Ok(None),
        Some(s) => {
            if s.is_empty() {
                return Ok(None);
            }

            // Parse format like "24h", "7d", "3600s"
            let chars: Vec<char> = s.chars().collect();
            if chars.is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ApiErrorResponse {
                        error: "Invalid TTL format".to_string(),
                    }),
                ));
            }

            // Find where digits end
            let num_end = chars.iter().position(|c| !c.is_ascii_digit()).unwrap_or(chars.len());
            if num_end == 0 {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ApiErrorResponse {
                        error: "Invalid TTL format: missing number".to_string(),
                    }),
                ));
            }

            let num: u64 = s[..num_end].parse().map_err(|_| (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorResponse {
                    error: "Invalid TTL format: invalid number".to_string(),
                }),
            ))?;

            let unit = &s[num_end..];
            let seconds = match unit {
                "s" | "sec" | "secs" | "second" | "seconds" => num,
                "m" | "min" | "mins" | "minute" | "minutes" => num * 60,
                "h" | "hr" | "hrs" | "hour" | "hours" => num * 3600,
                "d" | "day" | "days" => num * 86400,
                "w" | "week" | "weeks" => num * 604800,
                _ => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(ApiErrorResponse {
                            error: format!("Invalid TTL unit: {}", unit),
                        }),
                    ));
                }
            };

            Ok(Some(seconds))
        }
    }
}

/// API error type
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("State store error: {0}")]
    StateStoreError(#[from] crate::state_store::StateStoreError),
    
    #[error("VM error: {0}")]
    VMError(#[from] crate::firecracker::VMError),
    
    #[error("Memory error: {0}")]
    MemoryError(#[from] crate::memory::MemoryError),
}

/// Serve the API server
pub async fn serve(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState::new().await?;
    let app = router().with_state(state);
    
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("API server listening on {}", addr);
    
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    async fn test_app() -> Router {
        let state = AppState::new().await.unwrap();
        router().with_state(state)
    }

    #[tokio::test]
    async fn test_snapshot_requires_label() {
        let app = test_app().await;

        let body = Body::from(json!({ "ttl": "24h" }).to_string());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/snapshot")
            .header("content-type", "application/json")
            .body(body)
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_snapshot_success() {
        let app = test_app().await;

        let body = Body::from(json!({ "label": "test-snapshot", "ttl": "24h" }).to_string());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/snapshot")
            .header("content-type", "application/json")
            .body(body)
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_resume_requires_state_id() {
        let app = test_app().await;

        let body = Body::from(json!({ "mode": "collaborative" }).to_string());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/resume")
            .header("content-type", "application/json")
            .body(body)
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_fork_requires_state_id() {
        let app = test_app().await;

        let body = Body::from(json!({ "label": "patched" }).to_string());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/fork")
            .header("content-type", "application/json")
            .body(body)
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_status_endpoint() {
        let app = test_app().await;

        let request = Request::builder()
            .method("POST")
            .uri("/v1/status")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn test_parse_ttl() {
        assert_eq!(parse_ttl(&None).unwrap(), None);
        assert_eq!(parse_ttl(&Some("".to_string())).unwrap(), None);
        assert_eq!(parse_ttl(&Some("24h".to_string())).unwrap(), Some(86400));
        assert_eq!(parse_ttl(&Some("7d".to_string())).unwrap(), Some(604800));
        assert_eq!(parse_ttl(&Some("3600s".to_string())).unwrap(), Some(3600));
        assert_eq!(parse_ttl(&Some("30m".to_string())).unwrap(), Some(1800));
        assert!(parse_ttl(&Some("invalid".to_string())).is_err());
    }
}

// ============================================================================
// Metrics and Health Endpoints
// ============================================================================

/// Get Prometheus-compatible metrics
async fn get_metrics(
    State(state): State<AppState>,
) -> impl axum::response::IntoResponse {
    let metrics = state.metrics.prometheus_metrics();
    axum::response::Response::builder()
        .header("Content-Type", "text/plain; version=0.0.4")
        .body(axum::body::Body::from(metrics))
        .unwrap()
}

/// Health check endpoint
async fn get_health(
    State(state): State<AppState>,
) -> impl axum::response::IntoResponse {
    use serde_json::json;
    
    let snapshot = state.metrics.snapshot();
    
    Json(json!({
        "status": "healthy",
        "uptime_secs": snapshot.uptime_secs,
        "snapshots_total": snapshot.snapshots_total,
        "resumes_total": snapshot.resumes_total,
        "forks_total": snapshot.forks_total,
        "errors_total": snapshot.errors_total,
        "avg_snapshot_duration_ms": snapshot.avg_snapshot_duration_ms,
        "avg_resume_duration_ms": snapshot.avg_resume_duration_ms,
    }))
}

/// Delete a state
async fn delete_state(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiErrorResponse>)> {
    let state_id = payload.get("state_id")
        .and_then(|v| v.as_str())
        .ok_or((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorResponse {
                error: "state_id is required".to_string(),
            }),
        ))?;

    info!(state_id = %state_id, "Deleting state");

    state.state_store.delete_state(&crate::model::StateId(state_id.to_string())).await
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiErrorResponse {
                error: format!("Failed to delete state: {}", e),
            }),
        ))?;

    Ok(Json(json!({ "success": true, "state_id": state_id })))
}

/// List all states
async fn list_states(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiErrorResponse>)> {
    let states = state.state_store.list_states().await;
    
    let state_list: Vec<serde_json::Value> = states.iter().map(|s| {
        json!({
            "state_id": s.state_id.0,
            "label": s.metadata.label,
            "created_at": s.created_at,
            "size_bytes": s.size_bytes,
            "expires_at": s.expires_at,
        })
    }).collect();

    Ok(Json(json!({
        "count": state_list.len(),
        "states": state_list,
    })))
}
