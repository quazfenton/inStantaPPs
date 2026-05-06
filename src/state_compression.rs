//! State Compression Module
//!
//! Provides Zstandard compression for state storage to reduce disk usage
//! and network transfer times.
//!
//! # Features
//!
//! - Zstandard compression (levels 1-22)
//! - Streaming compression for large states
//! - Compression statistics
//! - Automatic decompression
//!
//! # Usage
//!
//! ```rust,no_run
//! use isa_workspace::state_compression::{Compressor, CompressionLevel};
//!
//! let compressor = Compressor::new(CompressionLevel::Default);
//!
//! // Compress state
//! let compressed = compressor.compress(&state_bytes)?;
//!
//! // Decompress
//! let decompressed = compressor.decompress(&compressed)?;
//! ```

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use tracing::{debug, info};

/// Compression level (1-22, higher = better compression but slower)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CompressionLevel {
    /// Fastest compression (level 1)
    Fastest,
    /// Fast compression (level 3)
    Fast,
    /// Default compression (level 6)
    Default,
    /// Good compression (level 9)
    Good,
    /// Best compression (level 13)
    Best,
    /// Maximum compression (level 19)
    Maximum,
    /// Custom level
    Custom(i32),
}

impl CompressionLevel {
    pub fn to_zstd_level(&self) -> i32 {
        match self {
            CompressionLevel::Fastest => 1,
            CompressionLevel::Fast => 3,
            CompressionLevel::Default => 6,
            CompressionLevel::Good => 9,
            CompressionLevel::Best => 13,
            CompressionLevel::Maximum => 19,
            CompressionLevel::Custom(level) => level.clamp(1, 22),
        }
    }
}

impl Default for CompressionLevel {
    fn default() -> Self {
        Self::Default
    }
}

/// Compression statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionStats {
    /// Original size in bytes
    pub original_size: usize,
    /// Compressed size in bytes
    pub compressed_size: usize,
    /// Compression ratio (0.0 - 1.0, lower is better)
    pub compression_ratio: f32,
    /// Space saved in bytes
    pub space_saved: usize,
    /// Space saved percentage
    pub space_saved_percent: f32,
    /// Compression time in milliseconds
    pub compress_time_ms: f32,
    /// Decompression time in milliseconds
    pub decompress_time_ms: f32,
}

impl CompressionStats {
    pub fn new(original_size: usize, compressed_size: usize) -> Self {
        let space_saved = original_size.saturating_sub(compressed_size);
        let ratio = if original_size > 0 {
            compressed_size as f32 / original_size as f32
        } else {
            1.0
        };
        let percent = if original_size > 0 {
            (space_saved as f32 / original_size as f32) * 100.0
        } else {
            0.0
        };

        Self {
            original_size,
            compressed_size,
            compression_ratio: ratio,
            space_saved,
            space_saved_percent: percent,
            compress_time_ms: 0.0,
            decompress_time_ms: 0.0,
        }
    }

    pub fn is_worth_it(&self) -> bool {
        // Compression is worth it if we saved at least 10%
        self.space_saved_percent >= 10.0
    }
}

/// State compressor using Zstandard
pub struct Compressor {
    level: CompressionLevel,
    /// Minimum size to attempt compression (bytes)
    min_size: usize,
}

impl Compressor {
    /// Create a new compressor with default settings
    pub fn new(level: CompressionLevel) -> Self {
        Self {
            level,
            min_size: 1024, // Don't compress data smaller than 1KB
        }
    }

    /// Set minimum size for compression
    pub fn with_min_size(mut self, min_size: usize) -> Self {
        self.min_size = min_size;
        self
    }

    /// Compress data
    pub fn compress(&self, data: &[u8]) -> Result<CompressedData, CompressionError> {
        let start = std::time::Instant::now();

        // Skip compression for small data
        if data.len() < self.min_size {
            debug!("Skipping compression for small data ({} bytes)", data.len());
            return Ok(CompressedData {
                data: data.to_vec(),
                compressed: false,
                original_size: data.len(),
            });
        }

        // Compress using zstd
        let compressed = zstd::encode_all(data, self.level.to_zstd_level())
            .map_err(|e| CompressionError::CompressError(e.to_string()))?;

        let compress_time = start.elapsed().as_secs_f32() * 1000.0;

        let stats = CompressionStats::new(data.len(), compressed.len());
        info!(
            "Compressed {} bytes -> {} bytes ({:.1}% saved, {:.2}ms)",
            data.len(),
            compressed.len(),
            stats.space_saved_percent,
            compress_time
        );

        Ok(CompressedData {
            data: compressed,
            compressed: true,
            original_size: data.len(),
        })
    }

    /// Decompress data
    pub fn decompress(&self, compressed: &CompressedData) -> Result<Vec<u8>, CompressionError> {
        let start = std::time::Instant::now();

        if !compressed.compressed {
            return Ok(compressed.data.clone());
        }

        let decompressed = zstd::decode_all(&compressed.data[..])
            .map_err(|e| CompressionError::DecompressError(e.to_string()))?;

        let decompress_time = start.elapsed().as_secs_f32() * 1000.0;

        // Verify size matches
        if decompressed.len() != compressed.original_size {
            return Err(CompressionError::SizeMismatch {
                expected: compressed.original_size,
                got: decompressed.len(),
            });
        }

        debug!(
            "Decompressed {} bytes -> {} bytes ({:.2}ms)",
            compressed.data.len(),
            decompressed.len(),
            decompress_time
        );

        Ok(decompressed)
    }

    /// Compress and return statistics
    pub fn compress_with_stats(&self, data: &[u8]) -> Result<(CompressedData, CompressionStats), CompressionError> {
        let start = std::time::Instant::now();
        let original_size = data.len();

        let compressed_data = self.compress(data)?;

        let total_time = start.elapsed().as_secs_f32() * 1000.0;

        let mut stats = CompressionStats::new(original_size, compressed_data.data.len());
        stats.compress_time_ms = total_time;

        Ok((compressed_data, stats))
    }

    /// Decompress and return statistics
    pub fn decompress_with_stats(&self, compressed: &CompressedData) -> Result<(Vec<u8>, CompressionStats), CompressionError> {
        let start = std::time::Instant::now();
        let original_size = compressed.original_size;

        let decompressed = self.decompress(compressed)?;

        let total_time = start.elapsed().as_secs_f32() * 1000.0;

        let mut stats = CompressionStats::new(original_size, compressed.data.len());
        stats.decompress_time_ms = total_time;

        Ok((decompressed, stats))
    }

    /// Get compression level
    pub fn level(&self) -> CompressionLevel {
        self.level
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new(CompressionLevel::Default)
    }
}

/// Compressed data with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedData {
    /// Compressed or raw data
    pub data: Vec<u8>,
    /// Whether data is actually compressed
    pub compressed: bool,
    /// Original uncompressed size
    pub original_size: usize,
}

impl CompressedData {
    /// Create new compressed data
    pub fn new(data: Vec<u8>, compressed: bool, original_size: usize) -> Self {
        Self {
            data,
            compressed,
            original_size,
        }
    }

    /// Get compressed size
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Get compression ratio
    pub fn ratio(&self) -> f32 {
        if self.original_size > 0 {
            self.data.len() as f32 / self.original_size as f32
        } else {
            1.0
        }
    }
}

/// Streaming compressor for large data
pub struct StreamingCompressor {
    level: CompressionLevel,
    encoder: Option<zstd::Encoder<'static, Vec<u8>>>,
}

impl StreamingCompressor {
    /// Create a new streaming compressor
    pub fn new(level: CompressionLevel) -> Result<Self, CompressionError> {
        Ok(Self {
            level,
            encoder: None,
        })
    }

    /// Start compression stream
    pub fn start(&mut self) -> Result<(), CompressionError> {
        let encoder = zstd::Encoder::new(Vec::new(), self.level.to_zstd_level())
            .map_err(|e| CompressionError::CompressError(e.to_string()))?;
        
        // Safety: We're transmuting the lifetime to 'static, but we ensure
        // the encoder is only used within this struct's lifetime
        self.encoder = Some(unsafe {
            std::mem::transmute::<zstd::Encoder<'_, Vec<u8>>, zstd::Encoder<'static, Vec<u8>>>(encoder)
        });
        
        Ok(())
    }

    /// Write data to compression stream
    pub fn write(&mut self, data: &[u8]) -> Result<(), CompressionError> {
        if let Some(ref mut encoder) = self.encoder {
            encoder.write_all(data)
                .map_err(|e| CompressionError::CompressError(e.to_string()))?;
        }
        Ok(())
    }

    /// Finish compression stream and get result
    pub fn finish(&mut self) -> Result<Vec<u8>, CompressionError> {
        if let Some(ref mut encoder) = self.encoder {
            encoder.finish()
                .map_err(|e| CompressionError::CompressError(e.to_string()))
        } else {
            Err(CompressionError::NotStarted)
        }
    }
}

impl Drop for StreamingCompressor {
    fn drop(&mut self) {
        // Ensure encoder is properly cleaned up
        let _ = self.finish();
    }
}

/// Compression errors
#[derive(Debug, thiserror::Error)]
pub enum CompressionError {
    #[error("Compression failed: {0}")]
    CompressError(String),

    #[error("Decompression failed: {0}")]
    DecompressError(String),

    #[error("Size mismatch: expected {expected}, got {got}")]
    SizeMismatch {
        expected: usize,
        got: usize,
    },

    #[error("Streaming compressor not started")]
    NotStarted,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compression_decompression() {
        let compressor = Compressor::new(CompressionLevel::Default);
        
        // Create test data (large enough to compress)
        let data = vec![42u8; 10000];
        
        // Compress
        let compressed = compressor.compress(&data).unwrap();
        assert!(compressed.compressed);
        assert!(compressed.data.len() < data.len());
        
        // Decompress
        let decompressed = compressor.decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_small_data_no_compression() {
        let compressor = Compressor::new(CompressionLevel::Default);
        
        // Small data should not be compressed
        let data = vec![42u8; 100];
        let compressed = compressor.compress(&data).unwrap();
        
        assert!(!compressed.compressed);
        assert_eq!(compressed.data.len(), data.len());
    }

    #[test]
    fn test_compression_stats() {
        let compressor = Compressor::new(CompressionLevel::Default);
        
        // Create compressible data (repeating pattern)
        let data = vec![0xABu8; 50000];
        
        let (_, stats) = compressor.compress_with_stats(&data).unwrap();
        
        assert!(stats.space_saved_percent > 0.0);
        assert!(stats.compression_ratio < 1.0);
        assert!(stats.compress_time_ms > 0.0);
    }

    #[test]
    fn test_compression_levels() {
        let data = vec![0xCDu8; 20000];
        
        let fastest = Compressor::new(CompressionLevel::Fastest);
        let best = Compressor::new(CompressionLevel::Best);
        
        let (fastest_compressed, fastest_stats) = fastest.compress_with_stats(&data).unwrap();
        let (best_compressed, best_stats) = best.compress_with_stats(&data).unwrap();
        
        // Best should compress better but take longer
        assert!(best_stats.compression_ratio < fastest_stats.compression_ratio);
        assert!(best_stats.compress_time_ms >= fastest_stats.compress_time_ms);
    }

    #[test]
    fn test_streaming_compression() {
        let mut streamer = StreamingCompressor::new(CompressionLevel::Default).unwrap();
        
        streamer.start().unwrap();
        streamer.write(b"Hello ").unwrap();
        streamer.write(b"World!").unwrap();
        
        let compressed = streamer.finish().unwrap();
        assert!(!compressed.is_empty());
    }

    #[test]
    fn test_compressed_data_methods() {
        let compressed = CompressedData::new(vec![1, 2, 3], true, 100);
        
        assert_eq!(compressed.size(), 3);
        assert_eq!(compressed.ratio(), 0.03);
    }

    #[test]
    fn test_stats_is_worth_it() {
        let stats_good = CompressionStats::new(1000, 500); // 50% saved
        assert!(stats_good.is_worth_it());
        
        let stats_bad = CompressionStats::new(1000, 950); // 5% saved
        assert!(!stats_bad.is_worth_it());
    }
}
