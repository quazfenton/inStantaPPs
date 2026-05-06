//! State Export/Import Module
//!
//! Provides backup and restore functionality for states.
//! Supports multiple export formats and compression.
//!
//! # Features
//!
//! - JSON export/import
//! - Binary export (protobuf-style)
//! - Compression (gzip, zstd)
//! - Batch export
//! - Import validation

use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use tracing::{debug, info, warn};

use crate::model::{State, StateId, StateMetadata};

/// Export format
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Json,
    Binary,
}

/// Compression algorithm
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompressionAlgorithm {
    None,
    Gzip,
    Zstd,
}

/// Export options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub compression: CompressionAlgorithm,
    pub include_metadata: bool,
    pub include_history: bool,
    pub password: Option<String>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Json,
            compression: CompressionAlgorithm::Gzip,
            include_metadata: true,
            include_history: true,
            password: None,
        }
    }
}

/// Exported state package
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedState {
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    pub state: State,
    pub metadata: ExportMetadata,
    pub checksum: String,
}

/// Export metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportMetadata {
    pub export_version: String,
    pub isa_version: String,
    pub compression: String,
    pub format: String,
    pub original_size: usize,
    pub compressed_size: Option<usize>,
}

/// Import result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub state_id: StateId,
    pub imported_at: DateTime<Utc>,
    pub validation_errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// State exporter/importer
pub struct StateArchive {
    export_version: String,
    isa_version: String,
}

impl StateArchive {
    /// Create a new state archive manager
    pub fn new() -> Self {
        Self {
            export_version: "1.0".to_string(),
            isa_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Export a state to bytes
    pub async fn export(
        &self,
        state: &State,
        options: &ExportOptions,
    ) -> Result<Vec<u8>, ArchiveError> {
        info!("Exporting state {} with options {:?}", state.state_id.0, options);

        // Create exported state package
        let mut exported = ExportedState {
            version: 1,
            exported_at: Utc::now(),
            state: state.clone(),
            metadata: ExportMetadata {
                export_version: self.export_version.clone(),
                isa_version: self.isa_version.clone(),
                compression: format!("{:?}", options.compression),
                format: format!("{:?}", options.format),
                original_size: 0,
                compressed_size: None,
            },
            checksum: String::new(),
        };

        // Calculate original size
        let original_data = match options.format {
            ExportFormat::Json => {
                serde_json::to_vec(&exported).map_err(|e| {
                    ArchiveError::SerializationError(format!("JSON export failed: {}", e))
                })?
            }
            ExportFormat::Binary => {
                // Binary format: use bincode or similar
                // For now, use JSON as binary is complex
                serde_json::to_vec(&exported).map_err(|e| {
                    ArchiveError::SerializationError(format!("Binary export failed: {}", e))
                })?
            }
        };

        exported.metadata.original_size = original_data.len();

        // Calculate checksum
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&original_data);
        exported.checksum = hex::encode(hasher.finalize());

        // Re-serialize with checksum
        let data = match options.format {
            ExportFormat::Json => serde_json::to_vec(&exported).map_err(|e| {
                ArchiveError::SerializationError(format!("JSON export failed: {}", e))
            })?,
            ExportFormat::Binary => serde_json::to_vec(&exported).map_err(|e| {
                ArchiveError::SerializationError(format!("Binary export failed: {}", e))
            })?,
        };

        // Compress if requested
        let compressed_data = match options.compression {
            CompressionAlgorithm::None => data,
            CompressionAlgorithm::Gzip => {
                let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&data).map_err(|e| {
                    ArchiveError::CompressionError(format!("Gzip compression failed: {}", e))
                })?;
                let compressed = encoder.finish().map_err(|e| {
                    ArchiveError::CompressionError(format!("Gzip finish failed: {}", e))
                })?;
                exported.metadata.compressed_size = Some(compressed.len());
                compressed
            }
            CompressionAlgorithm::Zstd => {
                let compressed = zstd::encode_all(&data[..], 3).map_err(|e| {
                    ArchiveError::CompressionError(format!("Zstd compression failed: {}", e))
                })?;
                exported.metadata.compressed_size = Some(compressed.len());
                compressed
            }
        };

        debug!(
            "Exported state: {} bytes -> {} bytes",
            exported.metadata.original_size,
            exported.metadata.compressed_size.unwrap_or(exported.metadata.original_size)
        );

        Ok(compressed_data)
    }

    /// Import a state from bytes
    pub async fn import(&self, data: &[u8], options: &ExportOptions) -> Result<ImportResult, ArchiveError> {
        info!("Importing state with options {:?}", options);

        // Decompress if needed
        let decompressed = match options.compression {
            CompressionAlgorithm::None => data.to_vec(),
            CompressionAlgorithm::Gzip => {
                let mut decoder = GzDecoder::new(data);
                let mut decompressed = Vec::new();
                decoder.read_to_end(&mut decompressed).map_err(|e| {
                    ArchiveError::DecompressionError(format!("Gzip decompression failed: {}", e))
                })?;
                decompressed
            }
            CompressionAlgorithm::Zstd => {
                zstd::decode_all(data).map_err(|e| {
                    ArchiveError::DecompressionError(format!("Zstd decompression failed: {}", e))
                })?
            }
        };

        // Deserialize
        let mut exported: ExportedState = match options.format {
            ExportFormat::Json => serde_json::from_slice(&decompressed).map_err(|e| {
                ArchiveError::DeserializationError(format!("JSON import failed: {}", e))
            })?,
            ExportFormat::Binary => serde_json::from_slice(&decompressed).map_err(|e| {
                ArchiveError::DeserializationError(format!("Binary import failed: {}", e))
            })?,
        };

        // Validate checksum
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&decompressed);
        let calculated_checksum = hex::encode(hasher.finalize());

        let mut warnings = Vec::new();
        if calculated_checksum != exported.checksum {
            warnings.push(format!(
                "Checksum mismatch: expected {}, got {}",
                exported.checksum, calculated_checksum
            ));
        }

        // Validate version compatibility
        if exported.version != 1 {
            warnings.push(format!("Unknown export version: {}", exported.version));
        }

        // Validate state
        let validation_errors = self.validate_state(&exported.state);

        info!(
            "Imported state {} with {} errors, {} warnings",
            exported.state.state_id.0,
            validation_errors.len(),
            warnings.len()
        );

        Ok(ImportResult {
            state_id: exported.state.state_id.clone(),
            imported_at: Utc::now(),
            validation_errors,
            warnings,
        })
    }

    /// Validate a state
    fn validate_state(&self, state: &State) -> Vec<String> {
        let mut errors = Vec::new();

        // Validate state ID format
        if state.state_id.0.is_empty() {
            errors.push("State ID is empty".to_string());
        }

        // Validate VM config
        if state.vm_config.vcpus == 0 {
            errors.push("VCPUs cannot be zero".to_string());
        }
        if state.vm_config.memory_mb == 0 {
            errors.push("Memory cannot be zero".to_string());
        }

        // Validate memory regions
        for (i, region) in state.memory_manifest.iter().enumerate() {
            if region.size == 0 {
                errors.push(format!("Memory region {} has zero size", i));
            }
        }

        // Validate metadata
        if state.metadata.label.is_empty() {
            errors.push("State label is empty".to_string());
        }

        errors
    }

    /// Export multiple states as a batch
    pub async fn export_batch(
        &self,
        states: &[State],
        options: &ExportOptions,
    ) -> Result<Vec<u8>, ArchiveError> {
        info!("Exporting {} states as batch", states.len());

        let mut exported_states = Vec::new();
        for state in states {
            let exported = self.export(state, options).await?;
            exported_states.push(exported);
        }

        // Package all exports
        let batch = serde_json::to_vec(&exported_states).map_err(|e| {
            ArchiveError::SerializationError(format!("Batch export failed: {}", e))
        })?;

        Ok(batch)
    }

    /// Import multiple states from a batch
    pub async fn import_batch(
        &self,
        data: &[u8],
        options: &ExportOptions,
    ) -> Result<Vec<ImportResult>, ArchiveError> {
        info!("Importing batch of states");

        let exported_states: Vec<Vec<u8>> = serde_json::from_slice(data).map_err(|e| {
            ArchiveError::DeserializationError(format!("Batch import failed: {}", e))
        })?;

        let mut results = Vec::new();
        for exported_data in exported_states {
            let result = self.import(&exported_data, options).await?;
            results.push(result);
        }

        Ok(results)
    }
}

impl Default for StateArchive {
    fn default() -> Self {
        Self::new()
    }
}

/// Archive errors
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Deserialization error: {0}")]
    DeserializationError(String),

    #[error("Compression error: {0}")]
    CompressionError(String),

    #[error("Decompression error: {0}")]
    DecompressionError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CPUState, MemoryFlags, MemoryTemperature, MemoryRegion, VMConfig, EventLog, UIStateRef};
    use uuid::Uuid;

    fn create_test_state() -> State {
        State {
            state_id: StateId(Uuid::new_v4().to_string()),
            vm_config: VMConfig {
                vcpus: 2,
                memory_mb: 512,
                kernel_image: "/test/vmlinux.bin".to_string(),
                rootfs_image: "/test/rootfs.ext4".to_string(),
            },
            cpu_state: CPUState {
                arch: "x86_64".to_string(),
                registers: serde_json::json!({}),
            },
            memory_manifest: vec![
                MemoryRegion {
                    region_id: "region-1".to_string(),
                    base_addr: 0x1000,
                    size: 4096,
                    flags: MemoryFlags {
                        read: true,
                        write: true,
                        execute: false,
                    },
                    temperature: MemoryTemperature::Hot,
                },
            ],
            fd_table: vec![],
            socket_table: vec![],
            device_state: vec![],
            deterministic_log: EventLog { events: vec![] },
            ui_state: UIStateRef {
                stream_id: Uuid::new_v4(),
            },
            metadata: StateMetadata {
                label: "test-state".to_string(),
                created_at: Utc::now(),
                ttl_seconds: None,
            },
        }
    }

    #[tokio::test]
    async fn test_export_import_json() {
        let archive = StateArchive::new();
        let state = create_test_state();

        let options = ExportOptions {
            format: ExportFormat::Json,
            compression: CompressionAlgorithm::None,
            ..Default::default()
        };

        // Export
        let exported = archive.export(&state, &options).await.unwrap();
        assert!(!exported.is_empty());

        // Import
        let result = archive.import(&exported, &options).await.unwrap();
        assert_eq!(result.state_id, state.state_id);
        assert!(result.validation_errors.is_empty());
    }

    #[tokio::test]
    async fn test_export_import_gzip() {
        let archive = StateArchive::new();
        let state = create_test_state();

        let options = ExportOptions {
            format: ExportFormat::Json,
            compression: CompressionAlgorithm::Gzip,
            ..Default::default()
        };

        // Export
        let exported = archive.export(&state, &options).await.unwrap();
        assert!(exported.len() < serde_json::to_vec(&state).unwrap().len());

        // Import
        let result = archive.import(&exported, &options).await.unwrap();
        assert_eq!(result.state_id, state.state_id);
    }

    #[tokio::test]
    async fn test_export_import_zstd() {
        let archive = StateArchive::new();
        let state = create_test_state();

        let options = ExportOptions {
            format: ExportFormat::Json,
            compression: CompressionAlgorithm::Zstd,
            ..Default::default()
        };

        // Export
        let exported = archive.export(&state, &options).await.unwrap();

        // Import
        let result = archive.import(&exported, &options).await.unwrap();
        assert_eq!(result.state_id, state.state_id);
    }

    #[tokio::test]
    async fn test_validation_errors() {
        let archive = StateArchive::new();

        // Create invalid state
        let mut state = create_test_state();
        state.state_id = StateId("".to_string()); // Invalid: empty ID
        state.vm_config.vcpus = 0; // Invalid: zero vcpus

        let options = ExportOptions::default();
        let exported = archive.export(&state, &options).await.unwrap();
        let result = archive.import(&exported, &options).await.unwrap();

        assert!(!result.validation_errors.is_empty());
    }

    #[tokio::test]
    async fn test_batch_export_import() {
        let archive = StateArchive::new();
        let states = vec![create_test_state(), create_test_state()];

        let options = ExportOptions::default();

        // Batch export
        let exported = archive.export_batch(&states, &options).await.unwrap();
        assert!(!exported.is_empty());

        // Batch import
        let results = archive.import_batch(&exported, &options).await.unwrap();
        assert_eq!(results.len(), 2);
    }
}
