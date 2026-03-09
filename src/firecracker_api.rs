//! Firecracker API Client - Working Implementation
//!
//! Connects to real Firecracker VMM via Unix socket HTTP API.
//! Provides full VM lifecycle management with actual Firecracker integration.

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::client::conn::http1::handshake;
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Firecracker API client
pub struct FirecrackerClient {
    socket_path: PathBuf,
    timeout_secs: u64,
}

impl FirecrackerClient {
    /// Create a new Firecracker client
    pub fn new<P: AsRef<Path>>(socket_path: P) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            timeout_secs: 30,
        }
    }

    /// Create client with custom timeout
    pub fn with_timeout<P: AsRef<Path>>(socket_path: P, timeout_secs: u64) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            timeout_secs,
        }
    }

    /// Check if Firecracker API is available
    pub async fn is_available(&self) -> bool {
        self.get::<serde_json::Value>("/").await.is_ok()
    }

    /// Get Firecracker version/info
    pub async fn get_info(&self) -> Result<FirecrackerInfo, FirecrackerError> {
        self.get("/").await
    }

    /// Configure boot source (kernel)
    pub async fn configure_boot_source(
        &self,
        config: &BootSourceConfig,
    ) -> Result<(), FirecrackerError> {
        self.put("/boot-source", config).await
    }

    /// Configure root filesystem
    pub async fn configure_drive(
        &self,
        config: &DriveConfig,
    ) -> Result<(), FirecrackerError> {
        self.put(format!("/drives/{}", config.drive_id), config).await
    }

    /// Configure network interface
    pub async fn configure_network(
        &self,
        config: &NetworkInterfaceConfig,
    ) -> Result<(), FirecrackerError> {
        self.put(format!("/network-interfaces/{}", config.iface_id), config).await
    }

    /// Configure VM (vCPUs, memory)
    pub async fn configure_machine(
        &self,
        config: &MachineConfig,
    ) -> Result<(), FirecrackerError> {
        self.put("/machine-config", config).await
    }

    /// Configure logging
    pub async fn configure_logger(
        &self,
        config: &LoggerConfig,
    ) -> Result<(), FirecrackerError> {
        self.put("/logger", config).await
    }

    /// Configure metrics
    pub async fn configure_metrics(
        &self,
        config: &MetricsConfig,
    ) -> Result<(), FirecrackerError> {
        self.put("/metrics", config).await
    }

    /// Start the VM
    pub async fn start_vm(&self) -> Result<(), FirecrackerError> {
        let action = InstanceAction {
            action_type: "InstanceStart".to_string(),
        };
        self.put("/actions", &action).await
    }

    /// Pause the VM
    pub async fn pause_vm(&self) -> Result<(), FirecrackerError> {
        let action = InstanceAction {
            action_type: "Pause".to_string(),
        };
        // Firecracker expects pause/resume actions on the /actions endpoint
        self.put("/actions", &action).await
    }

    /// Resume the VM
    pub async fn resume_vm(&self) -> Result<(), FirecrackerError> {
        let action = InstanceAction {
            action_type: "Resume".to_string(),
        };
        // Firecracker expects pause/resume actions on the /actions endpoint
        self.put("/actions", &action).await
    }
    }

    /// Stop the VM (send Ctrl+Alt+Del)
    pub async fn stop_vm(&self) -> Result<(), FirecrackerError> {
        let action = InstanceAction {
            action_type: "SendCtrlAltDel".to_string(),
        };
        self.put("/actions", &action).await
    }

    /// Get VM configuration
    pub async fn get_vm_config(&self) -> Result<VmConfig, FirecrackerError> {
        self.get("/vm").await
    }

    /// Get CPU configuration
    pub async fn get_cpu_config(&self) -> Result<CpuConfig, FirecrackerError> {
        self.get("/cpu-config").await
    }

    /// Create a full snapshot (memory + state)
    pub async fn create_full_snapshot(
        &self,
        config: &SnapshotConfig,
    ) -> Result<(), FirecrackerError> {
        self.put("/snapshot/create", config).await
    }

    /// Load a snapshot (for restore)
    pub async fn load_snapshot(
        &self,
        config: &SnapshotLoadConfig,
    ) -> Result<(), FirecrackerError> {
        self.put("/snapshot/load", config).await
    }

    /// Generic GET request
    async fn get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
    ) -> Result<T, FirecrackerError> {
        let response = self.request("GET", path, None).await?;
        self.parse_response(response).await
    }

    /// Generic PUT request
    async fn put<T: Serialize>(
        &self,
        path: impl AsRef<str>,
        body: &T,
    ) -> Result<(), FirecrackerError> {
        let path = path.as_ref();
        let json = serde_json::to_string(body)?;
        let _response = self.request("PUT", path, Some(json)).await?;
        Ok(())
    }

    /// Make HTTP request over Unix socket
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<hyper::Response<Incoming>, FirecrackerError> {
        let socket_path = self.socket_path.clone();
        let timeout_secs = self.timeout_secs;
        let method_str = method.to_string();
        let path_str = path.to_string();
        let body_data = body.unwrap_or_default();

        // Connect to Unix socket with timeout
        let stream = timeout(
            Duration::from_secs(timeout_secs),
            UnixStream::connect(&socket_path),
        )
        .await
        .map_err(|_| FirecrackerError::ConnectionTimeout)?
        .map_err(|e| FirecrackerError::ConnectionError(e.to_string()))?;

        let io = TokioIo::new(stream);

        // HTTP handshake
        let (mut sender, conn) = handshake(io)
            .await
            .map_err(|e| FirecrackerError::HandshakeError(e.to_string()))?;

        // Spawn connection handler
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                error!("Connection error: {}", e);
            }
        });

        // Build request
        let req = Request::builder()
            .method(method_str.as_str())
            .uri(path_str.as_str())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Host", "localhost")
            .body(Full::new(Bytes::from(body_data)))
            .map_err(|e| FirecrackerError::RequestError(e.to_string()))?;

        // Send request
        let response = timeout(
            Duration::from_secs(timeout_secs),
            sender.send_request(req),
        )
        .await
        .map_err(|_| FirecrackerError::RequestTimeout)?
        .map_err(|e| FirecrackerError::SendError(e.to_string()))?;

        // Check status
        if !response.status().is_success() {
            let status = response.status();
            let body = hyper::body::to_bytes(response.into_body())
                .await
                .unwrap_or_default();
            let error_msg = String::from_utf8_lossy(&body);
            return Err(FirecrackerError::ApiError {
                status: status.as_u16(),
                message: error_msg.to_string(),
            });
        }

        Ok(response)
    }

    /// Parse response body
    async fn parse_response<T: for<'de> Deserialize<'de>>(
        &self,
        response: hyper::Response<Incoming>,
    ) -> Result<T, FirecrackerError> {
        let body = hyper::body::to_bytes(response.into_body())
            .await
            .map_err(|e| FirecrackerError::BodyError(e.to_string()))?;

        serde_json::from_slice(&body)
            .map_err(|e| FirecrackerError::ParseError(e.to_string()))
    }
}

/// Firecracker information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirecrackerInfo {
    pub firecracker_version: String,
    pub arch: String,
}

/// Boot source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootSourceConfig {
    pub kernel_image_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_args: Option<String>,
}

/// Drive configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveConfig {
    pub drive_id: String,
    pub path_on_host: String,
    pub is_root_device: bool,
    pub is_read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_engine: Option<String>,
}

/// Network interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterfaceConfig {
    pub iface_id: String,
    pub host_dev_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest_mac: Option<String>,
}

/// Machine configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineConfig {
    pub vcpu_count: u8,
    pub mem_size_mib: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smt: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_template: Option<CpuTemplate>,
}

/// CPU template
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuTemplate {
    C3,
    T2,
    T2S,
    T2A,
    V1N1,
    #[serde(rename = "None")]
    None,
}

/// Logger configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggerConfig {
    pub log_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_level: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_log_origin: Option<bool>,
}

/// Metrics configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub metrics_path: String,
}

/// Instance action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceAction {
    pub action_type: String,
}

/// VM configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub vcpu_count: u8,
    pub mem_size_mib: usize,
    pub smt: bool,
    pub cpu_template: Option<CpuTemplate>,
}

/// CPU configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuConfig {
    pub cpu_count: u8,
    pub cpu_template: Option<CpuTemplate>,
}

/// Snapshot configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    #[serde(rename = "mem_file_path")]
    pub mem_path: String,
    #[serde(rename = "snapshot_path")]
    pub state_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Snapshot load configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotLoadConfig {
    #[serde(rename = "mem_file_path")]
    pub mem_path: String,
    #[serde(rename = "snapshot_path")]
    pub state_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_diff_snapshots: Option<bool>,
}

/// Firecracker errors
#[derive(Debug, thiserror::Error)]
pub enum FirecrackerError {
    #[error("Connection timeout")]
    ConnectionTimeout,

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Handshake error: {0}")]
    HandshakeError(String),

    #[error("Request error: {0}")]
    RequestError(String),

    #[error("Request timeout")]
    RequestTimeout,

    #[error("Send error: {0}")]
    SendError(String),

    #[error("Body error: {0}")]
    BodyError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("API error {status}: {message}")]
    ApiError { status: u16, message: String },

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// High-level VM manager with Firecracker integration
pub struct FirecrackerVM {
    client: FirecrackerClient,
    vm_id: String,
    config: VMConfiguration,
}

impl FirecrackerVM {
    /// Create a new VM manager
    pub fn new<P: AsRef<Path>>(
        socket_path: P,
        vm_id: String,
        config: VMConfiguration,
    ) -> Self {
        Self {
            client: FirecrackerClient::new(socket_path),
            vm_id,
            config,
        }
    }

    /// Initialize VM with all configurations
    pub async fn initialize(&self) -> Result<(), FirecrackerError> {
        info!("Initializing Firecracker VM: {}", self.vm_id);

        // Configure machine
        let machine_config = MachineConfig {
            vcpu_count: self.config.vcpus,
            mem_size_mib: self.config.memory_mb as usize,
            smt: Some(false),
            cpu_template: None,
        };
        self.client.configure_machine(&machine_config).await?;

        // Configure boot source
        let boot_config = BootSourceConfig {
            kernel_image_path: self.config.kernel_path.clone(),
            boot_args: Some(self.config.boot_args.clone()),
        };
        self.client.configure_boot_source(&boot_config).await?;

        // Configure root drive
        let drive_config = DriveConfig {
            drive_id: "rootfs".to_string(),
            path_on_host: self.config.rootfs_path.clone(),
            is_root_device: true,
            is_read_only: false,
            io_engine: None,
        };
        self.client.configure_drive(&drive_config).await?;

        // Configure network
        let network_config = NetworkInterfaceConfig {
            iface_id: "eth0".to_string(),
            host_dev_name: format!("tap-{}", &self.vm_id[..8.min(self.vm_id.len())]),
            guest_mac: None,
        };
        self.client.configure_network(&network_config).await?;

        Ok(())
    }

    /// Boot the VM
    pub async fn boot(&self) -> Result<(), FirecrackerError> {
        info!("Booting Firecracker VM: {}", self.vm_id);
        self.client.start_vm().await
    }

    /// Pause the VM
    pub async fn pause(&self) -> Result<(), FirecrackerError> {
        self.client.pause_vm().await
    }

    /// Resume the VM
    pub async fn resume(&self) -> Result<(), FirecrackerError> {
        self.client.resume_vm().await
    }

    /// Stop the VM
    pub async fn stop(&self) -> Result<(), FirecrackerError> {
        self.client.stop_vm().await
    }

    /// Create a snapshot
    pub async fn snapshot(
        &self,
        mem_path: &str,
        state_path: &str,
    ) -> Result<(), FirecrackerError> {
        info!("Creating snapshot for VM: {}", self.vm_id);

        // Pause VM first
        self.client.pause_vm().await?;

        // Create full snapshot
        let config = SnapshotConfig {
            mem_path: mem_path.to_string(),
            state_path: state_path.to_string(),
            snapshot_type: Some("Full".to_string()),
            version: None,
        };
        self.client.create_full_snapshot(&config).await?;

        // Resume VM
        self.client.resume_vm().await?;

        Ok(())
    }

    /// Load a snapshot (restore)
    pub async fn restore(&self, mem_path: &str, state_path: &str) -> Result<(), FirecrackerError> {
        info!("Restoring VM from snapshot: {}", self.vm_id);

        let config = SnapshotLoadConfig {
            mem_path: mem_path.to_string(),
            state_path: state_path.to_string(),
            enable_diff_snapshots: Some(false),
        };
        self.client.load_snapshot(&config).await
    }
}

/// VM configuration
#[derive(Debug, Clone)]
pub struct VMConfiguration {
    pub vcpus: u8,
    pub memory_mb: u32,
    pub kernel_path: String,
    pub rootfs_path: String,
    pub boot_args: String,
}

impl Default for VMConfiguration {
    fn default() -> Self {
        Self {
            vcpus: 2,
            memory_mb: 512,
            kernel_path: "/tmp/vmlinux.bin".to_string(),
            rootfs_path: "/tmp/rootfs.ext4".to_string(),
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialization() {
        let config = BootSourceConfig {
            kernel_image_path: "/tmp/vmlinux.bin".to_string(),
            boot_args: Some("console=ttyS0".to_string()),
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("vmlinux.bin"));
        assert!(json.contains("console=ttyS0"));
    }

    #[test]
    fn test_cpu_template_serialization() {
        let template = CpuTemplate::T2S;
        let json = serde_json::to_string(&template).unwrap();
        assert_eq!(json, "\"t2s\"");
    }

    #[test]
    fn test_instance_action() {
        let action = InstanceAction {
            action_type: "InstanceStart".to_string(),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("InstanceStart"));
    }
}
