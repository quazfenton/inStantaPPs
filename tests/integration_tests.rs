//! ISA Integration Tests
//!
//! Comprehensive tests for the full snapshot/resume flow,
//! including all subsystems working together.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use isa_workspace::api::{router, AppState};
use isa_workspace::model::*;
use isa_workspace::state_store::{StateStore, StateStoreConfig, StorageBackend};
use isa_workspace::firecracker::{VMManager, FirecrackerConfig, VMHandle, VMInstanceId};
use isa_workspace::memory::{MemorySnapshotManager, MemoryPage, CompressionAlgorithm, PAGE_SIZE};
use isa_workspace::deterministic::{DeterministicLogger, DeterministicLogConfig, SyscallEvent, SignalEvent};
use isa_workspace::socket_proxy::{ConnectionProxy, SocketProxyConfig};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

/// Create test application state
async fn create_test_state() -> AppState {
    AppState::new().await.unwrap()
}

/// Create test router with state
async fn test_app() -> Router {
    let state = create_test_state().await;
    router().with_state(state)
}

// ============================================================================
// API Integration Tests
// ============================================================================

#[tokio::test]
async fn test_full_snapshot_flow() {
    let app = test_app().await;

    // Create snapshot
    let body = Body::from(
        json!({
            "label": "integration-test-snapshot",
            "ttl": "1h"
        }).to_string()
    );
    
    let request = Request::builder()
        .method("POST")
        .uri("/v1/snapshot")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify response contains state_id
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(response_json.get("state_id").is_some());
    let state_id = response_json["state_id"].as_str().unwrap().to_string();

    println!("Created snapshot with state_id: {}", state_id);
}

#[tokio::test]
async fn test_snapshot_resume_flow() {
    let app = test_app().await;

    // Step 1: Create snapshot
    let body = Body::from(
        json!({
            "label": "snapshot-resume-test",
            "ttl": "24h"
        }).to_string()
    );
    
    let request = Request::builder()
        .method("POST")
        .uri("/v1/snapshot")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let state_id = response_json["state_id"].as_str().unwrap().to_string();

    // Step 2: Resume the snapshot
    // Note: In a real integration test, we'd use a fresh app instance
    // but share the state store. For now, we test with same app.
    
    let resume_body = Body::from(
        json!({
            "state_id": state_id,
            "mode": "collaborative",
            "region": "us-west-2"
        }).to_string()
    );
    
    let resume_request = Request::builder()
        .method("POST")
        .uri("/v1/resume")
        .header("content-type", "application/json")
        .body(resume_body)
        .unwrap();

    let resume_response = app.oneshot(resume_request).await.unwrap();
    assert_eq!(resume_response.status(), StatusCode::OK);

    let resume_body = axum::body::to_bytes(resume_response.into_body(), usize::MAX).await.unwrap();
    let resume_json: serde_json::Value = serde_json::from_slice(&resume_body).unwrap();
    assert_eq!(resume_json["accepted"], true);
    assert_eq!(resume_json["target_region"], "us-west-2");
    assert_eq!(resume_json["mode"], "collaborative");
}

#[tokio::test]
async fn test_snapshot_fork_flow() {
    let app = test_app().await;

    // Step 1: Create original snapshot
    let body = Body::from(
        json!({
            "label": "original-snapshot",
            "ttl": "24h"
        }).to_string()
    );
    
    let request = Request::builder()
        .method("POST")
        .uri("/v1/snapshot")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let state_id = response_json["state_id"].as_str().unwrap().to_string();

    // Step 2: Fork the snapshot
    let fork_body = Body::from(
        json!({
            "state_id": state_id,
            "label": "forked-snapshot"
        }).to_string()
    );
    
    let fork_request = Request::builder()
        .method("POST")
        .uri("/v1/fork")
        .header("content-type", "application/json")
        .body(fork_body)
        .unwrap();

    let fork_response = app.oneshot(fork_request).await.unwrap();
    assert_eq!(fork_response.status(), StatusCode::OK);

    let fork_body = axum::body::to_bytes(fork_response.into_body(), usize::MAX).await.unwrap();
    let fork_json: serde_json::Value = serde_json::from_slice(&fork_body).unwrap();
    
    let forked_state_id = fork_json["state_id"].as_str().unwrap();
    assert_ne!(forked_state_id, state_id);
    assert!(forked_state_id.starts_with("forked-") || Uuid::parse_str(forked_state_id).is_ok());
}

#[tokio::test]
async fn test_resume_nonexistent_state() {
    let app = test_app().await;

    let body = Body::from(
        json!({
            "state_id": "nonexistent-state-id"
        }).to_string()
    );
    
    let request = Request::builder()
        .method("POST")
        .uri("/v1/resume")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_fork_nonexistent_state() {
    let app = test_app().await;

    let body = Body::from(
        json!({
            "state_id": "nonexistent-state-id"
        }).to_string()
    );
    
    let request = Request::builder()
        .method("POST")
        .uri("/v1/fork")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================================================
// Subsystem Integration Tests
// ============================================================================

#[tokio::test]
async fn test_state_store_integration() {
    let config = StateStoreConfig {
        backend: StorageBackend::Memory,
        ..Default::default()
    };
    
    let store = StateStore::new(config).await.unwrap();

    // Create test state
    let state = State {
        state_id: StateId(Uuid::new_v4().to_string()),
        vm_config: VMConfig {
            vcpus: 2,
            memory_mb: 1024,
            kernel_image: "/test/vmlinux.bin".to_string(),
            rootfs_image: "/test/rootfs.ext4".to_string(),
        },
        cpu_state: CPUState {
            arch: "x86_64".to_string(),
            registers: json!({"rax": 0x1000, "rbx": 0x2000}),
        },
        memory_manifest: vec![
            MemoryRegion {
                region_id: "stack-0".to_string(),
                base_addr: 0x7fff_0000_0000,
                size: 8 * 1024 * 1024,
                flags: MemoryFlags { read: true, write: true, execute: false },
                temperature: MemoryTemperature::Hot,
            },
        ],
        fd_table: vec![],
        socket_table: vec![],
        device_state: vec![],
        deterministic_log: EventLog { events: vec![] },
        ui_state: UIStateRef { stream_id: Uuid::new_v4() },
        metadata: StateMetadata {
            label: "test-state".to_string(),
            created_at: chrono::Utc::now(),
            ttl_seconds: Some(3600),
        },
    };

    let original_id = state.state_id.clone();

    // Store state
    let stored = store.store_state(state).await.unwrap();
    assert_eq!(stored.state_id, original_id);

    // Retrieve state
    let retrieved = store.get_state(&original_id).await.unwrap();
    assert_eq!(retrieved.state_id, original_id);
    assert_eq!(retrieved.vm_config.vcpus, 2);

    // Check existence
    assert!(store.has_state(&original_id).await);

    // List states
    let states = store.list_states().await;
    assert_eq!(states.len(), 1);
}

#[tokio::test]
async fn test_memory_snapshot_integration() {
    let mut manager = MemorySnapshotManager::new();

    // Register memory region
    manager.register_region("test-heap".to_string(), 0x4000_0000, PAGE_SIZE * 100);

    // Create test memory data (100 pages)
    let memory_data = vec![0xABu8; PAGE_SIZE * 100];

    // Mark some pages as dirty
    for i in 0..10 {
        manager.mark_page_dirty("test-heap", i);
    }

    // Record some accesses for temperature classification
    for _ in 0..10 {
        manager.record_page_access("test-heap", 0);
        manager.record_page_access("test-heap", 1);
    }
    for _ in 0..3 {
        manager.record_page_access("test-heap", 2);
    }

    // Snapshot dirty pages
    let pages = manager.snapshot_dirty("test-heap", &memory_data).await.unwrap();
    assert_eq!(pages.len(), 10);

    // Verify compression worked
    for page in &pages {
        assert!(!page.data.is_empty());
        assert!(page.data.len() <= PAGE_SIZE);
    }

    // Generate manifest
    let manifest = manager.generate_manifest("test-heap");
    assert!(manifest.is_some());
    let manifest = manifest.unwrap();
    assert!(!manifest.is_empty());
}

#[tokio::test]
async fn test_deterministic_logger_integration() {
    let config = DeterministicLogConfig::default();
    let logger = DeterministicLogger::new(config);
    logger.enable_recording();

    // Log various events
    logger.log_syscall(SyscallEvent {
        syscall_number: 1,
        syscall_name: "read".to_string(),
        arguments: [0, 0x1000, 100, 0, 0, 0],
        return_value: 50,
        error_code: None,
    }).await.unwrap();

    logger.log_syscall(SyscallEvent {
        syscall_number: 64,
        syscall_name: "gettimeofday".to_string(),
        arguments: [0x2000, 0, 0, 0, 0, 0],
        return_value: 0,
        error_code: None,
    }).await.unwrap();

    logger.log_signal(SignalEvent {
        signal_number: 2,
        signal_name: "SIGINT".to_string(),
        source_pid: Some(12345),
        siginfo: None,
    }).await.unwrap();

    // Verify log contents
    assert_eq!(logger.entry_count().await, 3);

    let log = logger.get_event_log().await;
    assert_eq!(log.events.len(), 3);

    // Serialize and deserialize
    let serialized = logger.serialize().await.unwrap();
    let logger2 = DeterministicLogger::new(DeterministicLogConfig::default());
    logger2.deserialize(&serialized).await.unwrap();

    assert_eq!(logger2.entry_count().await, 3);
}

#[tokio::test]
async fn test_socket_proxy_integration() {
    use std::net::{Ipv4Addr, SocketAddr};

    let config = SocketProxyConfig::default();
    let proxy = ConnectionProxy::new(config);

    // Register some connections
    let guest1 = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 50000);
    let remote1 = SocketAddr::new(Ipv4Addr::new(8, 8, 8, 8).into(), 443);
    
    let guest2 = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 50001);
    let remote2 = SocketAddr::new(Ipv4Addr::new(1, 1, 1, 1).into(), 80);

    let conn1 = proxy.register_connection(guest1, remote1, TransportProtocol::Tcp).await.unwrap();
    let conn2 = proxy.register_connection(guest2, remote2, TransportProtocol::Quic).await.unwrap();

    // Verify lookups
    assert!(proxy.get_connection(&conn1).await.is_some());
    assert!(proxy.get_by_guest_addr(&guest1).await.is_some());

    // Get descriptors
    let descriptors = proxy.get_descriptors().await;
    assert_eq!(descriptors.len(), 2);

    let socket_table = proxy.get_socket_table().await;
    assert_eq!(socket_table.len(), 2);

    // Prepare for snapshot
    let states = proxy.prepare_snapshot().await.unwrap();
    assert_eq!(states.len(), 2);

    // Verify state contains connection info
    let state_ids: Vec<&String> = states.iter().map(|s| &s.connection_id).collect();
    assert!(state_ids.contains(&&conn1));
    assert!(state_ids.contains(&&conn2));
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[tokio::test]
async fn test_empty_label_rejected() {
    let app = test_app().await;

    let body = Body::from(json!({ "label": "", "ttl": "24h" }).to_string());
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
async fn test_invalid_ttl_format() {
    let app = test_app().await;

    let body = Body::from(json!({ "label": "test", "ttl": "invalid" }).to_string());
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
async fn test_various_ttl_formats() {
    let app = test_app().await;

    // Test various valid TTL formats
    let valid_ttls = vec!["1h", "24h", "7d", "60s", "30m", "1w"];
    
    for ttl in valid_ttls {
        let body = Body::from(json!({ "label": format!("test-{}", ttl), "ttl": ttl }).to_string());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/snapshot")
            .header("content-type", "application/json")
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "TTL {} should be valid", ttl);
    }
}

#[tokio::test]
async fn test_concurrent_snapshots() {
    let app = test_app().await;

    // Create multiple snapshots concurrently
    let mut handles = Vec::new();
    
    for i in 0..5 {
        let app_clone = app.clone();
        let handle = tokio::spawn(async move {
            let body = Body::from(
                json!({
                    "label": format!("concurrent-{}", i),
                    "ttl": "1h"
                }).to_string()
            );
            
            let request = Request::builder()
                .method("POST")
                .uri("/v1/snapshot")
                .header("content-type", "application/json")
                .body(body)
                .unwrap();

            let response = app_clone.oneshot(request).await.unwrap();
            (i, response.status())
        });
        
        handles.push(handle);
    }

    // Wait for all to complete
    for handle in handles {
        let (i, status) = handle.await.unwrap();
        assert_eq!(status, StatusCode::OK, "Concurrent snapshot {} failed", i);
    }
}

// ============================================================================
// Performance Tests (not run by default)
// ============================================================================

#[tokio::test]
#[ignore] // Skip by default - run with --ignored flag
async fn test_large_memory_snapshot() {
    let mut manager = MemorySnapshotManager::new();

    // Register a larger memory region (1MB)
    let num_pages = 256; // 256 * 4KB = 1MB
    manager.register_region("large-heap".to_string(), 0x4000_0000, PAGE_SIZE * num_pages);

    // Create test memory data
    let memory_data = vec![0xCDu8; PAGE_SIZE * num_pages];

    // Mark 50% of pages as dirty
    for i in 0..(num_pages / 2) {
        manager.mark_page_dirty("large-heap", i);
    }

    // Time the snapshot
    let start = std::time::Instant::now();
    let pages = manager.snapshot_dirty("large-heap", &memory_data).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(pages.len(), num_pages / 2);
    println!("Snapshot of {} pages took {:?}", pages.len(), elapsed);
    
    // Should complete in reasonable time (< 1 second for 128 pages)
    assert!(elapsed.as_millis() < 1000);
}

#[tokio::test]
#[ignore]
async fn test_deterministic_replay_performance() {
    use isa_workspace::deterministic::ReplayEngine;

    let config = DeterministicLogConfig::default();
    let logger = DeterministicLogger::new(config.clone());
    logger.enable_recording();

    // Log many events
    let num_events = 10000;
    for i in 0..num_events {
        logger.log_syscall(SyscallEvent {
            syscall_number: i % 100,
            syscall_name: format!("syscall_{}", i % 100),
            arguments: [0; 6],
            return_value: 0,
            error_code: None,
        }).await.unwrap();
    }

    let log = logger.get_event_log().await;
    let replay = ReplayEngine::new(log, config);
    replay.start().await;

    // Time replay
    let start = std::time::Instant::now();
    while !replay.is_complete().await {
        replay.advance().await;
    }
    let elapsed = start.elapsed();

    println!("Replayed {} events in {:?}", num_events, elapsed);
    assert!(elapsed.as_millis() < 5000); // Should complete in < 5 seconds
}
