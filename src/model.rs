use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Core State object as described in specs.md
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub state_id: StateId,
    pub vm_config: VMConfig,
    pub cpu_state: CPUState,
    pub memory_manifest: Vec<MemoryRegion>,
    pub fd_table: Vec<FDDescriptor>,
    pub socket_table: Vec<SocketDescriptor>,
    pub device_state: Vec<DeviceState>,
    pub deterministic_log: EventLog,
    pub ui_state: UIStateRef,
    pub metadata: StateMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VMConfig {
    pub vcpus: u8,
    pub memory_mb: u32,
    pub kernel_image: String,
    pub rootfs_image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CPUState {
    pub arch: String,
    pub registers: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRegion {
    pub region_id: String,
    pub base_addr: u64,
    pub size: u64,
    pub flags: MemoryFlags,
    pub temperature: MemoryTemperature,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFlags {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum MemoryTemperature {
    Hot,
    Warm,
    Cold,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FDDescriptor {
    pub fd: i32,
    pub kind: FDKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum FDKind {
    File { path: String },
    Socket { socket_id: String },
    Other { description: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketDescriptor {
    pub socket_id: String,
    pub protocol: TransportProtocol,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TransportProtocol {
    Tcp,
    Quic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceState {
    pub device_type: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLog {
    pub events: Vec<DeterministicEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeterministicEvent {
    Syscall { name: String },
    Signal { signal: String },
    ThreadSchedule { thread_id: u64 },
    NetworkIoBoundary { connection_id: String },
    RandomnessSeed { seed: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UIStateRef {
    pub stream_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMetadata {
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub ttl_seconds: Option<u64>,
}

/// API-level DTOs derived from the spec
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRequest {
    pub label: String,
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResponse {
    pub state_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeRequest {
    pub state_id: String,
    pub mode: Option<String>,
    pub region: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkRequest {
    pub state_id: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotCreateMetadata {
    pub state_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeResponse {
    pub accepted: bool,
    pub target_region: String,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkResponse {
    pub state_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: String,
}

impl State {
    /// Create a new empty State with provided metadata and config.
    pub fn new(vm_config: VMConfig, metadata: StateMetadata) -> Self {
        let state_id = StateId(Uuid::new_v4().to_string());
        Self {
            state_id,
            vm_config,
            cpu_state: CPUState { arch: "x86_64".into(), registers: serde_json::json!({}) },
            memory_manifest: Vec::new(),
            fd_table: Vec::new(),
            socket_table: Vec::new(),
            device_state: Vec::new(),
            deterministic_log: EventLog { events: Vec::new() },
            ui_state: UIStateRef { stream_id: Uuid::new_v4() },
            metadata,
        }
    }
}
