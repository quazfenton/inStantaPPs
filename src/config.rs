//! Configuration Module
//!
//! Provides configuration loading from files and environment variables.
//!
//! # Configuration File Format (TOML)
//!
//! ```toml
//! [server]
//! host = "0.0.0.0"
//! port = 3000
//!
//! [firecracker]
//! binary_path = "/usr/bin/firecracker"
//! workspace_root = "/tmp/isa-workspace"
//! use_jailer = false
//!
//! [state_store]
//! backend = "local"
//! storage_path = "/var/lib/isa/store"
//! cache_size_mb = 1024
//! default_ttl_hours = 24
//!
//! [quic]
//! port = 4433
//! use_tls = false
//!
//! [logging]
//! level = "info"
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub firecracker: FirecrackerConfig,
    #[serde(default)]
    pub state_store: StateStoreConfig,
    #[serde(default)]
    pub quic: QuicConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            firecracker: FirecrackerConfig::default(),
            state_store: StateStoreConfig::default(),
            quic: QuicConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from file
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::IoError(format!("Failed to read config file: {}", e)))?;

        let config: Config = toml::from_str(&content)
            .map_err(|e| ConfigError::ParseError(format!("Failed to parse config: {}", e)))?;

        info!("Loaded configuration from {:?}", path);
        Ok(config)
    }

    /// Load configuration from file or use defaults
    pub fn load(path: Option<&Path>) -> Self {
        if let Some(p) = path {
            if p.exists() {
                match Self::from_file(p) {
                    Ok(config) => return config,
                    Err(e) => {
                        tracing::warn!("Failed to load config from {:?}: {}", p, e);
                    }
                }
            }
        }

        // Fall back to environment variables
        Self::from_env()
    }

    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        let mut config = Config::default();

        if let Ok(port) = std::env::var("ISA_API_PORT") {
            if let Ok(p) = port.parse() {
                config.server.port = p;
            }
        }

        if let Ok(host) = std::env::var("ISA_API_HOST") {
            config.server.host = host;
        }

        if let Ok(workspace) = std::env::var("ISA_WORKSPACE") {
            config.firecracker.workspace_root = PathBuf::from(workspace);
        }

        if let Ok(storage) = std::env::var("ISA_STATE_STORE") {
            config.state_store.storage_path = PathBuf::from(storage);
        }

        if let Ok(log_level) = std::env::var("RUST_LOG") {
            config.logging.level = log_level;
        }

        config
    }

    /// Get the API bind address
    pub fn api_address(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }
}

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_workers")]
    pub workers: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            workers: default_workers(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    3000
}

fn default_workers() -> usize {
    num_cpus::get().max(2)
}

/// Firecracker configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirecrackerConfig {
    #[serde(default = "default_firecracker_path")]
    pub binary_path: PathBuf,
    #[serde(default = "default_jailer_path")]
    pub jailer_path: PathBuf,
    #[serde(default = "default_workspace_root")]
    pub workspace_root: PathBuf,
    #[serde(default)]
    pub use_jailer: bool,
    #[serde(default = "default_network_iface")]
    pub network_iface: String,
}

impl Default for FirecrackerConfig {
    fn default() -> Self {
        Self {
            binary_path: default_firecracker_path(),
            jailer_path: default_jailer_path(),
            workspace_root: default_workspace_root(),
            use_jailer: false,
            network_iface: default_network_iface(),
        }
    }
}

fn default_firecracker_path() -> PathBuf {
    PathBuf::from("/usr/bin/firecracker")
}

fn default_jailer_path() -> PathBuf {
    PathBuf::from("/usr/bin/jailer")
}

fn default_workspace_root() -> PathBuf {
    PathBuf::from("/tmp/isa-workspace")
}

fn default_network_iface() -> String {
    "eth0".to_string()
}

/// State store configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateStoreConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default = "default_storage_path")]
    pub storage_path: PathBuf,
    #[serde(default = "default_cache_size_mb")]
    pub cache_size_mb: usize,
    #[serde(default = "default_ttl_hours")]
    pub default_ttl_hours: u64,
    #[serde(default = "default_true")]
    pub enable_gc: bool,
    #[serde(default = "default_gc_interval_secs")]
    pub gc_interval_secs: u64,
}

impl Default for StateStoreConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            storage_path: default_storage_path(),
            cache_size_mb: default_cache_size_mb(),
            default_ttl_hours: default_ttl_hours(),
            enable_gc: default_true(),
            gc_interval_secs: default_gc_interval_secs(),
        }
    }
}

fn default_backend() -> String {
    "local".to_string()
}

fn default_storage_path() -> PathBuf {
    PathBuf::from("/tmp/isa-state-store")
}

fn default_cache_size_mb() -> usize {
    1024
}

fn default_ttl_hours() -> u64 {
    24
}

fn default_true() -> bool {
    true
}

fn default_gc_interval_secs() -> u64 {
    3600
}

/// QUIC configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuicConfig {
    #[serde(default = "default_quic_port")]
    pub port: u16,
    #[serde(default)]
    pub use_tls: bool,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for QuicConfig {
    fn default() -> Self {
        Self {
            port: default_quic_port(),
            use_tls: false,
            timeout_secs: default_timeout_secs(),
        }
    }
}

fn default_quic_port() -> u16 {
    4433
}

fn default_timeout_secs() -> u64 {
    30
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_format")]
    pub format: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "full".to_string()
}

/// Configuration errors
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    IoError(String),

    #[error("Parse error: {0}")]
    ParseError(String),
}

// Required for converting to crate's state_store::StateStoreConfig
impl Config {
    pub fn to_firecracker_config(&self) -> crate::firecracker::FirecrackerConfig {
        crate::firecracker::FirecrackerConfig {
            binary_path: self.firecracker.binary_path.clone(),
            jailer_path: self.firecracker.jailer_path.clone(),
            workspace_root: self.firecracker.workspace_root.clone(),
            use_jailer: self.firecracker.use_jailer,
            network_iface: self.firecracker.network_iface.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.firecracker.use_jailer, false);
        assert_eq!(config.state_store.backend, "local");
    }

    #[test]
    fn test_api_address() {
        let config = Config::default();
        assert_eq!(config.api_address(), "0.0.0.0:3000");
    }
}
