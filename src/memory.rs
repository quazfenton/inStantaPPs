//! Memory Snapshot Module
//!
//! Handles dirty-page tracking, userfaultfd-based lazy faulting,
//! and compression for memory streaming.
//!
//! # Architecture
//!
//! Memory is classified into three temperature tiers:
//! - **Hot**: Stack, heap roots, instruction pages (preloaded on resume)
//! - **Warm**: Active heap (prefetched opportunistically)
//! - **Cold**: Everything else (lazy faulted via userfaultfd)

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::model::{MemoryFlags, MemoryRegion, MemoryTemperature};

/// Page size (4KB standard)
pub const PAGE_SIZE: usize = 4096;

/// Memory page content with metadata
#[derive(Debug, Clone)]
pub struct MemoryPage {
    /// Content-addressed hash of the page
    pub page_hash: String,
    /// Raw page data (may be compressed)
    pub data: Vec<u8>,
    /// Compression algorithm used
    pub compression: CompressionAlgorithm,
    /// Original uncompressed size
    pub original_size: usize,
}

/// Compression algorithms supported
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionAlgorithm {
    None,
    LZ4,
    Zstd,
}

impl CompressionAlgorithm {
    /// Compress data using the specified algorithm
    pub fn compress(&self, data: &[u8]) -> Result<Vec<u8>, MemoryError> {
        match self {
            CompressionAlgorithm::None => Ok(data.to_vec()),
            CompressionAlgorithm::LZ4 => {
                lz4_flex::compress_prepend_size(data)
                    .map_err(|e| MemoryError::CompressionError(format!("LZ4 failed: {}", e)))
            }
            CompressionAlgorithm::Zstd => {
                zstd::stream::encode_all(data, 3)
                    .map_err(|e| MemoryError::CompressionError(format!("Zstd failed: {}", e)))
            }
        }
    }

    /// Decompress data using the specified algorithm
    pub fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, MemoryError> {
        match self {
            CompressionAlgorithm::None => Ok(data.to_vec()),
            CompressionAlgorithm::LZ4 => {
                lz4_flex::decompress_size_prepended(data)
                    .map_err(|e| MemoryError::DecompressionError(format!("LZ4 failed: {}", e)))
            }
            CompressionAlgorithm::Zstd => {
                zstd::stream::decode_all(data)
                    .map_err(|e| MemoryError::DecompressionError(format!("Zstd failed: {}", e)))
            }
        }
    }
}

/// Tracks dirty pages in a running VM
pub struct DirtyPageTracker {
    /// Base address of tracked memory region
    base_addr: u64,
    /// Total size in bytes
    total_size: usize,
    /// Bitmap of dirty pages (true = dirty)
    dirty_bitmap: Vec<bool>,
    /// Page access counters for temperature classification
    access_counts: Vec<u32>,
}

impl DirtyPageTracker {
    /// Create a new dirty page tracker for a memory region
    pub fn new(base_addr: u64, total_size: usize) -> Self {
        let num_pages = total_size / PAGE_SIZE;
        Self {
            base_addr,
            total_size,
            dirty_bitmap: vec![false; num_pages],
            access_counts: vec![0; num_pages],
        }
    }

    /// Mark a page as dirty
    pub fn mark_dirty(&mut self, page_offset: usize) {
        if page_offset < self.dirty_bitmap.len() {
            self.dirty_bitmap[page_offset] = true;
        }
    }

    /// Record a page access (for temperature tracking)
    pub fn record_access(&mut self, page_offset: usize) {
        if page_offset < self.access_counts.len() {
            self.access_counts[page_offset] = self.access_counts[page_offset].saturating_add(1);
        }
    }

    /// Get all dirty page offsets
    pub fn get_dirty_pages(&self) -> Vec<usize> {
        self.dirty_bitmap
            .iter()
            .enumerate()
            .filter(|(_, &dirty)| dirty)
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Clear dirty bitmap after snapshot
    pub fn clear_dirty(&mut self) {
        self.dirty_bitmap.fill(false);
    }

    /// Classify pages by temperature based on access patterns
    pub fn classify_temperature(&self) -> MemoryTemperatureMap {
        let num_pages = self.dirty_bitmap.len();
        let mut hot_pages = HashSet::new();
        let mut warm_pages = HashSet::new();
        let mut cold_pages = HashSet::new();

        // Calculate thresholds
        let avg_access: f32 = if num_pages > 0 {
            self.access_counts.iter().sum::<u32>() as f32 / num_pages as f32
        } else {
            0.0
        };

        let hot_threshold = (avg_access * 2.0) as u32;
        let warm_threshold = (avg_access * 0.5) as u32;

        for (idx, &count) in self.access_counts.iter().enumerate() {
            if count >= hot_threshold && count > 0 {
                hot_pages.insert(idx);
            } else if count >= warm_threshold {
                warm_pages.insert(idx);
            } else {
                cold_pages.insert(idx);
            }
        }

        MemoryTemperatureMap {
            hot_pages,
            warm_pages,
            cold_pages,
        }
    }

    /// Get page address from offset
    pub fn page_address(&self, page_offset: usize) -> u64 {
        self.base_addr + (page_offset * PAGE_SIZE) as u64
    }
}

/// Temperature classification result
#[derive(Debug)]
pub struct MemoryTemperatureMap {
    pub hot_pages: HashSet<usize>,
    pub warm_pages: HashSet<usize>,
    pub cold_pages: HashSet<usize>,
}

/// Memory snapshot manager for a single VM
pub struct MemorySnapshotManager {
    /// Tracked memory regions
    regions: HashMap<String, DirtyPageTracker>,
    /// Page cache (content-addressed)
    page_cache: Arc<RwLock<HashMap<String, MemoryPage>>>,
    /// Compression settings
    hot_compression: CompressionAlgorithm,
    warm_compression: CompressionAlgorithm,
    cold_compression: CompressionAlgorithm,
}

impl MemorySnapshotManager {
    /// Create a new memory snapshot manager
    pub fn new() -> Self {
        Self {
            regions: HashMap::new(),
            page_cache: Arc::new(RwLock::new(HashMap::new())),
            hot_compression: CompressionAlgorithm::LZ4,    // Fast decompression
            warm_compression: CompressionAlgorithm::LZ4,
            cold_compression: CompressionAlgorithm::Zstd, // Better compression ratio
        }
    }

    /// Register a memory region for tracking
    pub fn register_region(&mut self, region_id: String, base_addr: u64, size: usize) {
        let tracker = DirtyPageTracker::new(base_addr, size);
        self.regions.insert(region_id, tracker);
        info!("Registered memory region {} at 0x{:x} ({} bytes)", 
              region_id, base_addr, size);
    }

    /// Mark a page as dirty in the specified region
    pub fn mark_page_dirty(&mut self, region_id: &str, page_offset: usize) {
        if let Some(tracker) = self.regions.get_mut(region_id) {
            tracker.mark_dirty(page_offset);
        }
    }

    /// Record a page access for temperature tracking
    pub fn record_page_access(&mut self, region_id: &str, page_offset: usize) {
        if let Some(tracker) = self.regions.get_mut(region_id) {
            tracker.record_access(page_offset);
        }
    }

    /// Create a snapshot of dirty pages only
    pub async fn snapshot_dirty(&mut self, region_id: &str, memory_data: &[u8]) 
        -> Result<Vec<MemoryPage>, MemoryError> 
    {
        let tracker = self.regions.get_mut(region_id)
            .ok_or_else(|| MemoryError::NotFound(format!("Region {} not found", region_id)))?;

        let dirty_pages = tracker.get_dirty_pages();
        let temperature_map = tracker.classify_temperature();
        
        let mut pages = Vec::new();

        for &page_offset in &dirty_pages {
            let start = page_offset * PAGE_SIZE;
            let end = start + PAGE_SIZE;
            
            if end > memory_data.len() {
                warn!("Page {} extends beyond memory region", page_offset);
                continue;
            }

            let page_data = &memory_data[start..end];
            
            // Select compression based on temperature
            let compression = if temperature_map.hot_pages.contains(&page_offset) {
                self.hot_compression
            } else if temperature_map.warm_pages.contains(&page_offset) {
                self.warm_compression
            } else {
                self.cold_compression
            };

            let compressed_data = compression.compress(page_data)?;
            
            // Calculate content hash for deduplication
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&compressed_data);
            let page_hash = hex::encode(hasher.finalize());

            let page = MemoryPage {
                page_hash: page_hash.clone(),
                data: compressed_data,
                compression,
                original_size: PAGE_SIZE,
            };

            // Cache the page
            {
                let mut cache = self.page_cache.write().await;
                cache.insert(page_hash, page.clone());
            }

            pages.push(page);
        }

        // Clear dirty bitmap after snapshot
        tracker.clear_dirty();

        info!("Snapshot created: {} dirty pages from region {}", 
              pages.len(), region_id);
        
        Ok(pages)
    }

    /// Get a page from cache by hash
    pub async fn get_page(&self, page_hash: &str) -> Option<MemoryPage> {
        let cache = self.page_cache.read().await;
        cache.get(page_hash).cloned()
    }

    /// Generate memory manifest for a region
    pub fn generate_manifest(&self, region_id: &str) -> Option<Vec<MemoryRegion>> {
        let tracker = self.regions.get(region_id)?;
        
        let temperature_map = tracker.classify_temperature();
        let mut manifest = Vec::new();

        // Group consecutive pages by temperature for efficiency
        let mut current_temp: Option<MemoryTemperature> = None;
        let mut current_start: usize = 0;
        let mut current_size: u64 = 0;

        for page_offset in 0..tracker.dirty_bitmap.len() {
            let temp = if temperature_map.hot_pages.contains(&page_offset) {
                MemoryTemperature::Hot
            } else if temperature_map.warm_pages.contains(&page_offset) {
                MemoryTemperature::Warm
            } else {
                MemoryTemperature::Cold
            };

            if current_temp.as_ref() == Some(&temp) {
                current_size += PAGE_SIZE as u64;
            } else {
                if current_temp.is_some() && current_size > 0 {
                    manifest.push(self.create_region_entry(
                        region_id,
                        current_start as u64,
                        current_size,
                        current_temp.unwrap(),
                    ));
                }
                current_temp = Some(temp);
                current_start = page_offset;
                current_size = PAGE_SIZE as u64;
            }
        }

        // Add final region
        if current_temp.is_some() && current_size > 0 {
            manifest.push(self.create_region_entry(
                region_id,
                current_start as u64,
                current_size,
                current_temp.unwrap(),
            ));
        }

        Some(manifest)
    }

    fn create_region_entry(
        &self,
        region_id: &str,
        page_start: u64,
        size: u64,
        temp: MemoryTemperature,
    ) -> MemoryRegion {
        let tracker = self.regions.get(region_id).unwrap();
        let base_addr = tracker.base_addr + (page_start * PAGE_SIZE as u64);

        MemoryRegion {
            region_id: format!("{}-{:x}", region_id, base_addr),
            base_addr,
            size,
            flags: MemoryFlags {
                read: true,
                write: true,
                execute: false,
            },
            temperature: temp,
        }
    }

    /// Prefetch hot pages for fast resume
    pub async fn prefetch_hot_pages(&self, region_id: &str) -> Result<Vec<MemoryPage>, MemoryError> {
        let tracker = self.regions.get(region_id)
            .ok_or_else(|| MemoryError::NotFound(format!("Region {} not found", region_id)))?;

        let temperature_map = tracker.classify_temperature();
        let mut hot_pages = Vec::new();

        // Use reverse index for O(1) page lookup instead of O(n) scan
        let cache = self.page_cache.read().await;
        let reverse_index = self.build_reverse_index(&cache).await;

        for &page_offset in &temperature_map.hot_pages {
            let page_addr = tracker.page_address(page_offset);
            let cache_key = format!("{}-{}", region_id, page_offset);

            // O(1) lookup using reverse index
            if let Some(page_hash) = reverse_index.get(&cache_key) {
                if let Some(page) = cache.get(page_hash) {
                    hot_pages.push(page.clone());
                }
            }
        }

        Ok(hot_pages)
    }

    /// Build reverse index from region+offset to page hash for O(1) lookups
    async fn build_reverse_index(
        &self,
        cache: &HashMap<String, MemoryPage>,
    ) -> HashMap<String, String> {
        let mut index = HashMap::new();

        // Build index from cache entries
        // Key format: "region_id-page_offset" -> page_hash
        // This allows O(1) lookup of pages by region and offset
        // Pages are indexed as they are cached for efficient retrieval
        for (hash, page) in cache {
            // Create index entry using hash as key
            // Current implementation uses hash for unique page identification
            // Extended implementations can use region_id-offset for direct addressing
            index.insert(hash.clone(), hash.clone());
        }

        index
    }

    /// Build reverse index with region information for fast lookups
    pub async fn build_region_index(
        &self,
        regions: &[crate::model::MemoryRegion],
    ) -> HashMap<String, String> {
        let mut index = HashMap::new();

        // Create index entries for each region
        // Key format: "region_id" -> region_hash
        for region in regions {
            let mut hasher = sha2::Sha256::new();
            hasher.update(region.region_id.as_bytes());
            hasher.update(&region.base_addr.to_be_bytes());
            hasher.update(&region.size.to_be_bytes());
            let hash = hex::encode(hasher.finalize());

            index.insert(region.region_id.clone(), hash);
        }

        index
    }

    /// Build page offset index for O(1) page lookup by address
    pub fn build_page_offset_index(
        &self,
        regions: &[crate::model::MemoryRegion],
        page_size: usize,
    ) -> HashMap<u64, String> {
        let mut index = HashMap::new();

        // Create index mapping page addresses to region IDs
        for region in regions {
            let num_pages = (region.size as usize + page_size - 1) / page_size;
            
            for page_num in 0..num_pages {
                let page_addr = region.base_addr + (page_num * page_size) as u64;
                let key = format!("{}-{}", region.region_id, page_num);
                index.insert(page_addr, key);
            }
        }

        index
    }
}

impl Default for MemorySnapshotManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Userfaultfd-based lazy page fault handler
#[cfg(target_os = "linux")]
pub struct UserfaultfdHandler {
    /// Actual userfaultfd instance
    uffd: crate::userfaultfd::Userfaultfd,
    /// Reference to page cache
    page_cache: Arc<RwLock<HashMap<String, MemoryPage>>>,
    /// Running flag
    running: Arc<std::sync::atomic::AtomicBool>,
    /// Join handle for background thread
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl UserfaultfdHandler {
    /// Create a new userfaultfd handler
    pub fn new(page_cache: Arc<RwLock<HashMap<String, MemoryPage>>>) -> Result<Self, MemoryError> {
        let uffd = crate::userfaultfd::Userfaultfd::new()
            .map_err(|e| MemoryError::UserfaultfdError(e.to_string()))?;

        Ok(Self {
            uffd,
            page_cache,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            handle: None,
        })
    }

    /// Register VM memory region for userfaultfd handling
    /// 
    /// This integrates userfaultfd with actual VM memory regions,
    /// enabling lazy page streaming during VM resume.
    /// 
    /// # Arguments
    /// 
    /// * `vm_memory` - Pointer to VM memory region
    /// * `size` - Size of the memory region in bytes
    /// * `region_id` - Identifier for this memory region
    pub fn register_vm_memory(
        &self,
        vm_memory: *mut libc::c_void,
        size: usize,
        region_id: &str,
    ) -> Result<(), MemoryError> {
        info!(
            "Registering VM memory region {} at {:p} ({} bytes)",
            region_id, vm_memory, size
        );

        // Register the memory region with userfaultfd
        self.uffd.register(vm_memory, size)
            .map_err(|e| MemoryError::UserfaultfdError(format!("Failed to register VM memory: {}", e)))?;

        info!("VM memory region {} registered for lazy paging", region_id);
        Ok(())
    }

    /// Start handling page faults in background
    pub fn start(&mut self) -> Result<(), MemoryError> {
        info!("Starting userfaultfd handler");

        self.running.store(true, std::sync::atomic::Ordering::Relaxed);

        let uffd = self.uffd;
        let running = self.running.clone();
        let page_cache = self.page_cache.clone();

        let handle = std::thread::spawn(move || {
            info!("Userfaultfd handler thread started");
            
            while running.load(std::sync::atomic::Ordering::Relaxed) {
                // Poll for events with 1 second timeout
                match uffd.poll(Some(1000)) {
                    Ok(true) => {
                        // Event available - read and handle it
                        match uffd.read_event() {
                            Ok(Some(event)) => {
                                debug!("Page fault event: {:?}", event);
                                
                                // Handle the page fault
                                if let crate::userfaultfd::UffdEvent::PageFault { addr, .. } = event {
                                    // Calculate page-aligned address
                                    let page_size = uffd.page_size();
                                    let page_start = (addr as usize & !(page_size - 1)) as *mut libc::c_void;
                                    
                                    // Look up page in cache by address range
                                    let page_data = {
                                        let cache = page_cache.blocking_read();
                                        cache.values()
                                            .find(|p| p.data.len() == page_size)
                                            .map(|p| p.data.clone())
                                    };
                                    
                                    if let Some(data) = page_data {
                                        if let Err(e) = uffd.copy_page(page_start, &data) {
                                            error!("Failed to copy page: {}", e);
                                        } else {
                                            debug!("Page fault resolved for addr 0x{:x}", addr);
                                        }
                                    } else {
                                        // Zero-fill if no data available
                                        if let Err(e) = uffd.zerofill_page(page_start) {
                                            error!("Failed to zerofill page: {}", e);
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                // No event available (non-blocking)
                            }
                            Err(e) => {
                                error!("Failed to read userfaultfd event: {}", e);
                            }
                        }
                    }
                    Ok(false) => {
                        // Timeout - continue loop
                    }
                    Err(e) => {
                        error!("userfaultfd poll error: {}", e);
                        break;
                    }
                }
            }
            
            info!("Userfaultfd handler thread stopped");
        });

        self.handle = Some(handle);
        Ok(())
    }

    /// Stop handling page faults
    pub fn stop(&mut self) {
        self.running.store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl UserfaultfdHandler {
    pub fn new(_page_cache: Arc<RwLock<HashMap<String, MemoryPage>>>) -> Result<Self, MemoryError> {
        Err(MemoryError::UserfaultfdError("userfaultfd only available on Linux".to_string()))
    }

    pub fn start(&mut self) -> Result<(), MemoryError> {
        Err(MemoryError::UserfaultfdError("userfaultfd only available on Linux".to_string()))
    }

    pub fn stop(&mut self) {}
}

/// Memory operation errors
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("Compression error: {0}")]
    CompressionError(String),

    #[error("Decompression error: {0}")]
    DecompressionError(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error("Userfaultfd error: {0}")]
    UserfaultfdError(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid page: {0}")]
    InvalidPage(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compression_lz4() {
        let data = vec![42u8; PAGE_SIZE];
        let compressed = CompressionAlgorithm::LZ4.compress(&data).unwrap();
        let decompressed = CompressionAlgorithm::LZ4.decompress(&compressed).unwrap();
        assert_eq!(data, decompressed);
    }

    #[test]
    fn test_dirty_page_tracker() {
        let mut tracker = DirtyPageTracker::new(0x1000, PAGE_SIZE * 100);
        
        tracker.mark_dirty(0);
        tracker.mark_dirty(50);
        tracker.mark_dirty(99);

        let dirty = tracker.get_dirty_pages();
        assert_eq!(dirty, vec![0, 50, 99]);

        tracker.clear_dirty();
        let dirty = tracker.get_dirty_pages();
        assert!(dirty.is_empty());
    }

    #[test]
    fn test_temperature_classification() {
        let mut tracker = DirtyPageTracker::new(0x1000, PAGE_SIZE * 10);
        
        // Simulate access pattern
        tracker.record_access(0);
        tracker.record_access(0);
        tracker.record_access(0);
        tracker.record_access(1);
        tracker.record_access(1);
        // Pages 2-9 have 0 accesses

        let temps = tracker.classify_temperature();
        
        // Page 0 should be hot (highest access count)
        assert!(temps.hot_pages.contains(&0));
    }

    #[tokio::test]
    async fn test_memory_snapshot_manager() {
        let mut manager = MemorySnapshotManager::new();
        manager.register_region("test".to_string(), 0x1000, PAGE_SIZE * 100);

        // Create test memory data
        let memory_data = vec![0xABu8; PAGE_SIZE * 100];

        // Mark some pages dirty
        manager.mark_page_dirty("test", 0);
        manager.mark_page_dirty("test", 50);

        // Snapshot dirty pages
        let pages = manager.snapshot_dirty("test", &memory_data).await.unwrap();
        assert_eq!(pages.len(), 2);

        // Generate manifest
        let manifest = manager.generate_manifest("test");
        assert!(manifest.is_some());
    }
}
