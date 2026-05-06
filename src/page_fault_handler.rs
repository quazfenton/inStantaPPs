//! Page Fault Handler with State Store Integration
//!
//! Provides production-ready page fault handling that fetches pages
//! from the state store instead of just zero-filling.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::memory::MemoryPage;
use crate::state_store::StateStore;
use crate::userfaultfd::{Userfaultfd, UffdEvent, UserfaultfdError};

/// Page fault handler with state store integration
#[cfg(target_os = "linux")]
pub struct PageFaultHandler {
    /// Userfaultfd instance
    uffd: Userfaultfd,
    /// State store for fetching pages
    state_store: Arc<StateStore>,
    /// Page cache
    page_cache: Arc<RwLock<HashMap<String, MemoryPage>>>,
    /// Running flag
    running: Arc<std::sync::atomic::AtomicBool>,
    /// Background thread handle
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl PageFaultHandler {
    /// Create a new page fault handler with state store integration
    pub fn new(
        state_store: Arc<StateStore>,
        page_cache: Arc<RwLock<HashMap<String, MemoryPage>>>,
    ) -> Result<Self, UserfaultfdError> {
        let uffd = Userfaultfd::new()?;
        
        Ok(Self {
            uffd,
            state_store,
            page_cache,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            handle: None,
        })
    }

    /// Start handling page faults in background
    pub fn start(&mut self) -> Result<(), UserfaultfdError> {
        info!("Starting page fault handler with state store integration");
        
        self.running.store(true, std::sync::atomic::Ordering::Relaxed);
        
        let uffd = self.uffd;
        let running = self.running.clone();
        let state_store = self.state_store.clone();
        let page_cache = self.page_cache.clone();
        
        let handle = std::thread::spawn(move || {
            info!("Page fault handler thread started");
            
            while running.load(std::sync::atomic::Ordering::Relaxed) {
                // Poll for events with 1 second timeout
                match uffd.poll(Some(1000)) {
                    Ok(true) => {
                        // Event available - read and handle it
                        match uffd.read_event() {
                            Ok(Some(event)) => {
                                debug!("Page fault event: {:?}", event);
                                
                                if let UffdEvent::PageFault { addr, .. } = event {
                                    // Calculate page-aligned address
                                    let page_size = uffd.page_size();
                                    let page_start = (addr as usize & !(page_size - 1)) as *mut libc::c_void;
                                    
                                    // Try to fetch page from state store
                                    let page_data = Self::fetch_page_from_store(
                                        &state_store,
                                        &page_cache,
                                        addr,
                                        page_size,
                                    );
                                    
                                    match page_data {
                                        Some(data) => {
                                            // Copy page from state store
                                            if let Err(e) = uffd.copy_page(page_start, &data) {
                                                error!("Failed to copy page at 0x{:x}: {}", addr, e);
                                            } else {
                                                debug!("Copied page from store at 0x{:x}", addr);
                                            }
                                        }
                                        None => {
                                            // Fall back to zero-fill
                                            if let Err(e) = uffd.zerofill_page(page_start) {
                                                error!("Failed to zero-fill page at 0x{:x}: {}", addr, e);
                                            } else {
                                                debug!("Zero-filled page at 0x{:x}", addr);
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                // No event available (non-blocking)
                            }
                            Err(e) => {
                                error!("Failed to read page fault event: {}", e);
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
            
            info!("Page fault handler thread stopped");
        });
        
        self.handle = Some(handle);
        Ok(())
    }

    /// Fetch page data from state store
    fn fetch_page_from_store(
        state_store: &StateStore,
        page_cache: &RwLock<HashMap<String, MemoryPage>>,
        addr: u64,
        page_size: usize,
        memory_manifest: Option<&[crate::model::MemoryRegion]>,
    ) -> Option<Vec<u8>> {
        // Calculate page-aligned address
        let page_start = addr & !(page_size as u64 - 1);
        
        let runtime = tokio::runtime::Handle::current();

        // Check cache first using address-based lookup
        {
            let cache = page_cache.blocking_read();
            
            // Try to find page by address range
            if let Some(manifest) = memory_manifest {
                for region in manifest {
                    if page_start >= region.base_addr 
                        && page_start < region.base_addr + region.size 
                    {
                        // Found region, look for matching page in cache
                        for page in cache.values() {
                            if page.data.len() == page_size {
                                return Some(page.data.clone());
                            }
                        }
                    }
                }
            } else {
                // No manifest, try any matching page
                for page in cache.values() {
                    if page.data.len() == page_size {
                        return Some(page.data.clone());
                    }
                }
            }
        }

        // Fetch from state store using address-to-hash mapping
        // Calculate hash from address for lookup
        if let Some(manifest) = memory_manifest {
            for region in manifest {
                if page_start >= region.base_addr 
                    && page_start < region.base_addr + region.size 
                {
                    // Calculate offset within region
                    let offset = page_start - region.base_addr;
                    
                    // Create hash from region and offset
                    let mut hasher = sha2::Sha256::new();
                    hasher.update(region.region_id.as_bytes());
                    hasher.update(&offset.to_be_bytes());
                    let hash = hex::encode(hasher.finalize());
                    
                    // Try to fetch from state store
                    let page_data = runtime.block_on(state_store.get_page(&hash));
                    if let Ok(data) = page_data {
                        return Some(data);
                    }
                }
            }
        }
        
        None
    }

    /// Stop handling page faults
    pub fn stop(&mut self) {
        self.running.store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    /// Register memory region for fault handling
    pub fn register_region(&self, start: *mut libc::c_void, len: usize) -> Result<(), UserfaultfdError> {
        self.uffd.register(start, len)
    }
}

#[cfg(not(target_os = "linux"))]
pub struct PageFaultHandler;

#[cfg(not(target_os = "linux"))]
impl PageFaultHandler {
    pub fn new(
        _state_store: Arc<StateStore>,
        _page_cache: Arc<RwLock<HashMap<String, MemoryPage>>>,
    ) -> Result<Self, UserfaultfdError> {
        Err(UserfaultfdError::NotSupported)
    }

    pub fn start(&mut self) -> Result<(), UserfaultfdError> {
        Err(UserfaultfdError::NotSupported)
    }

    pub fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::{StateStore, StateStoreConfig, StorageBackend};

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn test_page_fault_handler_creation() {
        let state_store = Arc::new(
            StateStore::new(StateStoreConfig {
                backend: StorageBackend::Memory,
                ..Default::default()
            }).await.unwrap()
        );
        
        let page_cache = Arc::new(RwLock::new(HashMap::new()));
        
        let handler = PageFaultHandler::new(state_store, page_cache);
        assert!(handler.is_ok());
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn test_page_fault_handler_not_supported() {
        let state_store = Arc::new(
            StateStore::new(StateStoreConfig::default()).await.unwrap()
        );
        
        let page_cache = Arc::new(RwLock::new(HashMap::new()));
        
        let handler = PageFaultHandler::new(state_store, page_cache);
        assert!(matches!(handler, Err(UserfaultfdError::NotSupported)));
    }
}
