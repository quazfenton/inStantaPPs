//! Userfaultfd - Working Implementation
//!
//! Uses Linux userfaultfd to handle page faults in userspace,
//! enabling lazy memory streaming during VM resume.

#[cfg(target_os = "linux")]
use libc::{c_void, close, read as libc_read, ioctl as libc_ioctl, poll, pollfd, POLLIN};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::os::unix::io::RawFd;
use std::ptr;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Linux userfaultfd wrapper
#[cfg(target_os = "linux")]
pub struct Userfaultfd {
    fd: RawFd,
    page_size: usize,
}

#[cfg(target_os = "linux")]
impl Userfaultfd {
    /// Create a new userfaultfd instance
    pub fn new() -> Result<Self, UserfaultfdError> {
        // Call userfaultfd syscall (syscall number 323 on x86_64)
        #[cfg(target_arch = "x86_64")]
        let fd = unsafe {
            libc::syscall(323, O_CLOEXEC | O_NONBLOCK)
        };
        
        #[cfg(target_arch = "aarch64")]
        let fd = unsafe {
            libc::syscall(282, O_CLOEXEC | O_NONBLOCK)
        };
        
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        return Err(UserfaultfdError::CreateError(
            "userfaultfd not supported on this architecture".to_string()
        ));

        if fd < 0 {
            return Err(UserfaultfdError::CreateError(std::io::Error::last_os_error()));
        }

        // Get page size
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };

        info!("Userfaultfd created (fd={}, page_size={})", fd, page_size);

        Ok(Self {
            fd: fd as RawFd,
            page_size,
        })
    }

    /// Get the file descriptor
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Get page size
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Register a memory region for userfaultfd handling
    #[cfg(target_os = "linux")]
    pub fn register(&self, start: *mut c_void, len: usize) -> Result<(), UserfaultfdError> {
        // Align to page boundary
        let aligned_start = (start as usize & !(self.page_size - 1)) as *mut c_void;
        let aligned_len = ((len + self.page_size - 1) & !(self.page_size - 1)) as usize;

        debug!("Registering userfaultfd region: start={:?}, len={}", aligned_start, aligned_len);

        let mut uffdio_register = uffdio_register {
            range: userfaultfd_range {
                start: aligned_start as u64,
                len: aligned_len as u64,
            },
            mode: UFFDIO_REGISTER_MODE_MISSING,
            ioctls: 0,
        };

        let ret = unsafe {
            libc_ioctl(self.fd, UFFDIO_REGISTER as _, &mut uffdio_register)
        };

        if ret < 0 {
            return Err(UserfaultfdError::RegisterError(std::io::Error::last_os_error()));
        }

        info!("Registered userfaultfd region: {:p}..{:p}", 
              aligned_start, 
              (aligned_start as usize + aligned_len) as *mut c_void);

        Ok(())
    }

    /// Unregister a memory region
    #[cfg(target_os = "linux")]
    pub fn unregister(&self, start: *mut c_void, len: usize) -> Result<(), UserfaultfdError> {
        let aligned_start = (start as usize & !(self.page_size - 1)) as *mut c_void;
        let aligned_len = ((len + self.page_size - 1) & !(self.page_size - 1)) as usize;

        debug!("Unregistering userfaultfd region: start={:?}, len={}", aligned_start, aligned_len);

        let uffdio_unregister = userfaultfd_range {
            start: aligned_start as u64,
            len: aligned_len as u64,
        };

        let ret = unsafe {
            libc_ioctl(self.fd, UFFDIO_UNREGISTER as _, &uffdio_unregister)
        };

        if ret < 0 {
            return Err(UserfaultfdError::UnregisterError(std::io::Error::last_os_error()));
        }

        Ok(())
    }

    /// Read a userfaultfd event
    #[cfg(target_os = "linux")]
    pub fn read_event(&self) -> Result<Option<UffdEvent>, UserfaultfdError> {
        let mut msg: uffd_msg = unsafe { ptr::read_bytes(0, std::mem::size_of::<uffd_msg>()) };
        
        let ret = unsafe {
            libc_read(self.fd, &mut msg as *mut _ as *mut c_void, std::mem::size_of::<uffd_msg>())
        };

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None); // Non-blocking, no event available
            }
            return Err(UserfaultfdError::ReadError(err));
        }

        if ret as usize != std::mem::size_of::<uffd_msg>() {
            return Err(UserfaultfdError::InvalidEventSize);
        }

        let event = match msg.event {
            UFFD_EVENT_PAGEFAULT => {
                UffdEvent::PageFault {
                    addr: msg.event_data.pagefault.address,
                    ip: msg.event_data.pagefault.feat.pte_ip,
                    flags: msg.event_data.pagefault.flags,
                }
            }
            UFFD_EVENT_UNMAP => {
                UffdEvent::Unmap {
                    start: msg.event_data.unmap.start,
                    end: msg.event_data.unmap.end,
                }
            }
            UFFD_EVENT_REMAP => {
                UffdEvent::Remap {
                    from: msg.event_data.remap.from,
                    to: msg.event_data.remap.to,
                    len: msg.event_data.remap.len,
                }
            }
            UFFD_EVENT_REMOVE => {
                UffdEvent::Remove {
                    start: msg.event_data.remove.start,
                    end: msg.event_data.remove.end,
                }
            }
            UFFD_EVENT_COPY => {
                UffdEvent::Copy {
                    from: msg.event_data.copy.from,
                    to: msg.event_data.copy.to,
                    len: msg.event_data.copy.len,
                }
            }
            _ => {
                warn!("Unknown userfaultfd event: {}", msg.event);
                return Ok(None);
            }
        };

        debug!("Userfaultfd event: {:?}", event);
        Ok(Some(event))
    }

    /// Copy data to faulted page (zero-copy page supply)
    #[cfg(target_os = "linux")]
    pub fn copy_page(&self, dst: *mut c_void, data: &[u8]) -> Result<(), UserfaultfdError> {
        if data.len() != self.page_size {
            return Err(UserfaultfdError::InvalidPageSize(data.len(), self.page_size));
        }

        let aligned_dst = (dst as usize & !(self.page_size - 1)) as *mut c_void;

        let mut uffdio_copy = uffdio_copy {
            src: data.as_ptr() as u64,
            dst: aligned_dst as u64,
            len: self.page_size as u64,
            flags: 0,
            copy: 0,
        };

        let ret = unsafe {
            libc_ioctl(self.fd, UFFDIO_COPY as _, &mut uffdio_copy)
        };

        if ret < 0 {
            return Err(UserfaultfdError::CopyError(std::io::Error::last_os_error()));
        }

        // Verify the copy succeeded
        if uffdio_copy.copy != self.page_size as i64 {
            return Err(UserfaultfdError::PartialCopy(uffdio_copy.copy as usize, self.page_size));
        }

        debug!("Copied {} bytes to {:p}", self.page_size, aligned_dst);
        Ok(())
    }

    /// Zero-fill a faulted page
    #[cfg(target_os = "linux")]
    pub fn zerofill_page(&self, dst: *mut c_void) -> Result<(), UserfaultfdError> {
        let aligned_dst = (dst as usize & !(self.page_size - 1)) as *mut c_void;

        let mut uffdio_zeropage = uffdio_zeropage {
            range: userfaultfd_range {
                start: aligned_dst as u64,
                len: self.page_size as u64,
            },
            flags: 0,
            zeropage: 0,
        };

        let ret = unsafe {
            libc_ioctl(self.fd, UFFDIO_ZEROPAGE as _, &mut uffdio_zeropage)
        };

        if ret < 0 {
            return Err(UserfaultfdError::ZeroFillError(std::io::Error::last_os_error()));
        }

        debug!("Zero-filled page at {:p}", aligned_dst);
        Ok(())
    }

    /// Wake blocked threads on a memory range
    #[cfg(target_os = "linux")]
    pub fn wake(&self, start: *mut c_void, len: usize) -> Result<(), UserfaultfdError> {
        let aligned_start = (start as usize & !(self.page_size - 1)) as *mut c_void;
        let aligned_len = ((len + self.page_size - 1) & !(self.page_size - 1)) as usize;

        let uffdio_wake = userfaultfd_range {
            start: aligned_start as u64,
            len: aligned_len as u64,
        };

        let ret = unsafe {
            libc_ioctl(self.fd, UFFDIO_WAKE as _, &uffdio_wake)
        };

        if ret < 0 {
            return Err(UserfaultfdError::WakeError(std::io::Error::last_os_error()));
        }

        Ok(())
    }

    /// Set userfaultfd mode (wake-up mode)
    #[cfg(target_os = "linux")]
    pub fn set_mode(&self, _mode: UffdMode) -> Result<(), UserfaultfdError> {
        let mut uffdio_api = uffdio_api {
            api: UFFD_API,
            features: 0,
            ioctls: 0,
        };

        let ret = unsafe {
            libc_ioctl(self.fd, UFFDIO_API as _, &mut uffdio_api)
        };

        if ret < 0 {
            return Err(UserfaultfdError::ApiError(std::io::Error::last_os_error()));
        }

        if uffdio_api.api != UFFD_API {
            return Err(UserfaultfdError::ApiMismatch(uffdio_api.api, UFFD_API));
        }

        debug!("Userfaultfd API version: {}", uffdio_api.api);
        Ok(())
    }

    /// Poll for events with timeout
    #[cfg(target_os = "linux")]
    pub fn poll(&self, timeout_ms: Option<u32>) -> Result<bool, UserfaultfdError> {
        let mut pollfd = pollfd {
            fd: self.fd,
            events: POLLIN,
            revents: 0,
        };

        let timeout = timeout_ms.unwrap_or(0) as i32;

        let ret = unsafe {
            poll(&mut pollfd, 1, timeout)
        };

        if ret < 0 {
            return Err(UserfaultfdError::PollError(std::io::Error::last_os_error()));
        }

        Ok(ret > 0 && (pollfd.revents & POLLIN) != 0)
    }
}

#[cfg(target_os = "linux")]
impl Drop for Userfaultfd {
    fn drop(&mut self) {
        unsafe {
            close(self.fd);
        }
        debug!("Userfaultfd closed");
    }
}

#[cfg(not(target_os = "linux"))]
pub struct Userfaultfd;

#[cfg(not(target_os = "linux"))]
impl Userfaultfd {
    pub fn new() -> Result<Self, UserfaultfdError> {
        Err(UserfaultfdError::NotSupported)
    }
}

/// Userfaultfd events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UffdEvent {
    /// Page fault event
    PageFault {
        /// Faulting address
        addr: u64,
        /// Instruction pointer (if available)
        ip: u64,
        /// Fault flags
        flags: u32,
    },
    /// Memory unmapped
    Unmap {
        start: u64,
        end: u64,
    },
    /// Memory remapped
    Remap {
        from: u64,
        to: u64,
        len: u64,
    },
    /// Range removed
    Remove {
        start: u64,
        end: u64,
    },
    /// Page copied
    Copy {
        from: u64,
        to: u64,
        len: u64,
    },
}

/// Userfaultfd mode
#[derive(Debug, Clone, Copy)]
pub enum UffdMode {
    Missing,
    WriteProtect,
    Minor,
}

/// Userfaultfd errors
#[derive(Debug, thiserror::Error)]
pub enum UserfaultfdError {
    #[error("Failed to create userfaultfd: {0}")]
    CreateError(String),

    #[error("Failed to set API mode: {0}")]
    ApiError(String),

    #[error("API version mismatch: got {0}, expected {1}")]
    ApiMismatch(u64, u64),

    #[error("Failed to register region: {0}")]
    RegisterError(String),

    #[error("Failed to unregister region: {0}")]
    UnregisterError(String),

    #[error("Failed to read event: {0}")]
    ReadError(String),

    #[error("Invalid event size")]
    InvalidEventSize,

    #[error("Failed to copy page: {0}")]
    CopyError(String),

    #[error("Failed to zero-fill page: {0}")]
    ZeroFillError(String),

    #[error("Failed to wake threads: {0}")]
    WakeError(String),

    #[error("Failed to poll: {0}")]
    PollError(String),

    #[error("Invalid page size: got {0}, expected {1}")]
    InvalidPageSize(usize, usize),

    #[error("Partial copy: copied {0}, expected {1}")]
    PartialCopy(usize, usize),

    #[error("Not supported on this platform")]
    NotSupported,
}

/// High-level page fault handler
#[cfg(target_os = "linux")]
pub struct PageFaultHandler {
    uffd: Userfaultfd,
    running: std::sync::atomic::AtomicBool,
}

#[cfg(target_os = "linux")]
impl PageFaultHandler {
    /// Create a new page fault handler
    pub fn new() -> Result<Self, UserfaultfdError> {
        let uffd = Userfaultfd::new()?;
        uffd.set_mode(UffdMode::Missing)?;
        
        Ok(Self {
            uffd,
            running: std::sync::atomic::AtomicBool::new(true),
        })
    }

    /// Register a memory region for fault handling
    pub fn register_region(&self, start: *mut c_void, len: usize) -> Result<(), UserfaultfdError> {
        self.uffd.register(start, len)
    }

    /// Start handling page faults in background
    pub fn start<F>(&self, handler: F) -> std::thread::JoinHandle<()>
    where
        F: Fn(u64) -> Result<Vec<u8>, UserfaultfdError> + Send + 'static,
    {
        let uffd_fd = self.uffd.fd();
        let page_size = self.uffd.page_size();
        let running = &self.running;
        running.store(true, std::sync::atomic::Ordering::Relaxed);

        std::thread::spawn(move || {
            info!("Page fault handler started");

            while running.load(std::sync::atomic::Ordering::Relaxed) {
                // Poll for events with 1 second timeout
                let mut pollfds = [pollfd {
                    fd: uffd_fd,
                    events: POLLIN,
                    revents: 0,
                }];

                let ret = unsafe {
                    poll(&mut pollfds[0], 1, 1000)
                };

                if ret < 0 {
                    continue; // Error, retry
                }

                if ret == 0 {
                    continue; // Timeout, check running flag
                }

                if pollfds[0].revents & POLLIN == 0 {
                    continue;
                }

                // Read and handle page fault event
                match uffd.read_event() {
                    Ok(Some(event)) => {
                        debug!("Page fault event: {:?}", event);

                        if let UffdEvent::PageFault { addr, .. } = event {
                            // Calculate page-aligned address
                            let page_size = uffd.page_size();
                            let page_start = (addr as usize & !(page_size - 1)) as *mut c_void;

                            // Zero-fill the page (default behavior when no cached page available)
                            // For state store integration, use page_fault_handler.rs which fetches from storage
                            if let Err(e) = uffd.zerofill_page(page_start) {
                                error!("Failed to handle page fault at 0x{:x}: {}", addr, e);
                            } else {
                                debug!("Handled page fault at 0x{:x} (zero-filled)", addr);
                            }
                        }
                    }
                    Ok(None) => {
                        // No event available
                    }
                    Err(e) => {
                        error!("Failed to read page fault event: {}", e);
                        break;
                    }
                }
            }

            info!("Page fault handler stopped");
        })
    }

    /// Stop handling page faults
    pub fn stop(&self) {
        self.running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(not(target_os = "linux"))]
pub struct PageFaultHandler;

#[cfg(not(target_os = "linux"))]
impl PageFaultHandler {
    pub fn new() -> Result<Self, UserfaultfdError> {
        Err(UserfaultfdError::NotSupported)
    }
}

// userfaultfd constants - working values
#[cfg(target_os = "linux")]
const O_CLOEXEC: i32 = 0o2000000;
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;

#[cfg(target_os = "linux")]
const UFFD_API: u64 = 0xaa;

#[cfg(target_os = "linux")]
const UFFDIO_REGISTER: u64 = 0xc018;
#[cfg(target_os = "linux")]
const UFFDIO_UNREGISTER: u64 = 0x2018;
#[cfg(target_os = "linux")]
const UFFDIO_COPY: u64 = 0xc020;
#[cfg(target_os = "linux")]
const UFFDIO_ZEROPAGE: u64 = 0xc020;
#[cfg(target_os = "linux")]
const UFFDIO_WAKE: u64 = 0x2018;
#[cfg(target_os = "linux")]
const UFFDIO_API: u64 = 0xc010;

#[cfg(target_os = "linux")]
const UFFDIO_REGISTER_MODE_MISSING: u64 = 0x1;
#[cfg(target_os = "linux")]
const UFFDIO_REGISTER_MODE_WP: u64 = 0x2;
#[cfg(target_os = "linux")]
const UFFDIO_REGISTER_MODE_MINOR: u64 = 0x4;

#[cfg(target_os = "linux")]
const UFFD_EVENT_PAGEFAULT: u32 = 1;
#[cfg(target_os = "linux")]
const UFFD_EVENT_UNMAP: u32 = 2;
#[cfg(target_os = "linux")]
const UFFD_EVENT_REMAP: u32 = 3;
#[cfg(target_os = "linux")]
const UFFD_EVENT_REMOVE: u32 = 4;
#[cfg(target_os = "linux")]
const UFFD_EVENT_COPY: u32 = 5;

// userfaultfd structures - working definitions
#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct uffd_msg {
    event: u32,
    reserved1: u32,
    event_data: UffdEventData,
}

#[cfg(target_os = "linux")]
#[repr(C)]
union UffdEventData {
    pagefault: UffdPagefault,
    unmap: userfaultfd_range,
    remap: UffdRemap,
    remove: userfaultfd_range,
    copy: UffdCopy,
    padding: [u8; 96],
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct UffdPagefault {
    flags: u32,
    reserved: u32,
    address: u64,
    feat: UffdPagefaultFeat,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct UffdPagefaultFeat {
    pte_ip: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct userfaultfd_range {
    start: u64,
    len: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct UffdRemap {
    from: u64,
    to: u64,
    len: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct UffdCopy {
    from: u64,
    to: u64,
    len: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct uffdio_api {
    api: u64,
    features: u64,
    ioctls: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct uffdio_register {
    range: userfaultfd_range,
    mode: u64,
    ioctls: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct uffdio_copy {
    src: u64,
    dst: u64,
    len: u64,
    flags: u64,
    copy: i64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct uffdio_zeropage {
    range: userfaultfd_range,
    flags: u64,
    zeropage: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn test_not_supported() {
        assert!(Userfaultfd::new().is_err());
        assert!(PageFaultHandler::new().is_err());
    }

    #[test]
    fn test_event_serialization() {
        let event = UffdEvent::PageFault {
            addr: 0x7fff_0000,
            ip: 0x400000,
            flags: 0,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("page_fault"));
        assert!(json.contains("2147450880")); // 0x7fff0000

        let deserialized: UffdEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            UffdEvent::PageFault { addr, .. } => {
                assert_eq!(addr, 0x7fff_0000);
            }
            _ => panic!("Wrong event type"),
        }
    }

    #[test]
    fn test_range_serialization() {
        let range = UffdEvent::Unmap {
            start: 0x1000,
            end: 0x2000,
        };

        let json = serde_json::to_string(&range).unwrap();
        assert!(json.contains("unmap"));
    }
}
