//! ISA Workspace Library
//!
//! Instant-State Applications platform for sub-200ms state resume.
//!
//! # Modules
//!
//! ## Production System Integration
//!
//! - `firecracker_api`: Real Firecracker HTTP API client
//! - `firecracker`: Firecracker VM lifecycle management
//! - `kvm_capture`: KVM CPU state capture via kvm-ioctls
//! - `userfaultfd`: Linux userfaultfd for lazy page faulting
//! - `syscall_intercept`: ptrace/eBPF syscall interception
//!
//! ## Media & Streaming
//!
//! - `webrtc_transport`: WebRTC peer-to-peer media transport (SDP generation)
//! - `native_webrtc`: Native WebRTC with webrtc-rs (feature-gated)
//! - `ui_streaming_full`: Complete UI capture, encode, stream pipeline
//! - `drm_capture`: DRM/KMS framebuffer capture (Linux)
//! - `ffmpeg_encoder`: FFmpeg video encoding (H.264/VP8/VP9/AV1)
//!
//! ## Security & Operations
//!
//! - `auth`: Authentication and authorization (API keys, capabilities)
//! - `encryption`: AES-256-GCM state encryption
//! - `audit`: Tamper-evident audit logging
//! - `rate_limit`: Token bucket rate limiting
//!
//! ## State Management
//!
//! - `state_diff`: Incremental state diffing and patching
//! - `state_versioning`: Version control with branching
//! - `state_sharing`: Collaborative access control
//!
//! ## Core Modules
//!
//! - `model`: Core data structures (State, CPUState, MemoryRegion, etc.)
//! - `config`: Configuration loading and management
//! - `metrics`: Prometheus-compatible metrics and telemetry
//! - `websocket`: WebSocket handlers for real-time updates
//! - `orchestration`: Edge node management and VM placement
//! - `api`: HTTP API server (Axum)
//! - `memory`: Memory snapshot with dirty-page tracking and compression
//! - `state_store`: Content-addressed storage with deduplication
//! - `quic_transport`: QSSP (QUIC State Streaming Protocol)
//! - `socket_proxy`: TCP/QUIC connection virtualization
//! - `deterministic`: Deterministic execution logging and replay
//!
//! # Example
//!
//! ```rust,no_run
//! use isa_workspace::api::{serve, AppState};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     serve("0.0.0.0:3000").await?;
//!     Ok(())
//! }
//! ```

pub mod model;
pub mod config;
pub mod metrics;
pub mod websocket;
pub mod ui_streaming_full;
pub mod x11_capture;
pub mod webrtc_transport;
pub mod webrtc_media;
pub mod native_webrtc;
pub mod drm_capture;
pub mod ffmpeg_encoder;
pub mod encryption;
pub mod audit;
pub mod rate_limit;
pub mod auth;
pub mod validation;
pub mod state_diff;
pub mod state_versioning;
pub mod state_sharing;
pub mod state_compression;
pub mod state_cloning;
pub mod state_search;
pub mod state_archive;
pub mod batch_ops;
pub mod webhook;
pub mod page_fault_handler;
pub mod firecracker_process;
pub mod s3_backend;
pub mod syscall_interceptor_real;
pub mod orchestration;
pub mod syscall_intercept;
pub mod api;
pub mod firecracker;
pub mod firecracker_api;
pub mod kvm_capture;
pub mod userfaultfd;
pub mod memory;
pub mod state_store;
pub mod quic_transport;
pub mod socket_proxy;
pub mod socket_intercept;
pub mod deterministic;

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Create default application state
pub async fn create_app_state() -> Result<api::AppState, Box<dyn std::error::Error>> {
    let state = api::AppState::new().await?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
    }

    #[tokio::test]
    async fn test_create_state() {
        let state = create_app_state().await;
        assert!(state.is_ok());
    }
}
