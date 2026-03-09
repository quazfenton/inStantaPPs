//! Transparent Socket Interception Library
//!
//! Provides LD_PRELOAD-based socket interception for transparent redirection
//! through the ISA socket proxy.
//!
//! # Usage
//!
//! ```bash
//! # Compile as shared library
//! gcc -shared -fPIC -o libisa_intercept.so src/socket_intercept.c -ldl
//!
//! # Preload when running applications
//! LD_PRELOAD=./libisa_intercept.so ISA_PROXY=127.0.0.1:14433 ./your-application
//! ```
//!
//! # How It Works
//!
//! This library intercepts socket system calls:
//! - `socket()` - Create socket
//! - `connect()` - Redirect to proxy with CONNECT header
//! - `send()/recv()` - Pass through to proxied connection
//! - `close()` - Cleanup
//!
//! The intercepted calls wrap the original connection in a CONNECT request
//! that the socket_proxy module understands.

use std::collections::HashMap;
use std::env;
use std::ffi::CString;
use std::net::SocketAddr;
use std::sync::Arc;
use libc::{c_int, c_void, size_t, sockaddr, socklen_t};
use parking_lot::RwLock;
use tracing::{debug, error, info, warn};

/// Interception configuration
#[derive(Debug, Clone)]
pub struct InterceptConfig {
    /// Proxy address to redirect connections to
    pub proxy_addr: SocketAddr,
    /// Enable interception
    pub enabled: bool,
    /// Log intercepted connections
    pub log_connections: bool,
}

impl Default for InterceptConfig {
    fn default() -> Self {
        // Read configuration from environment
        let proxy_addr = env::var("ISA_PROXY")
            .unwrap_or_else(|_| "127.0.0.1:14433".to_string())
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:14433".parse().unwrap());

        let enabled = env::var("ISA_INTERCEPT")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(true);

        Self {
            proxy_addr,
            enabled,
            log_connections: true,
        }
    }
}

/// Global interception state
pub struct InterceptState {
    config: InterceptConfig,
    /// Original socket function pointers
    orig_socket: RwLock<Option<unsafe extern "C" fn(c_int, c_int, c_int) -> c_int>>,
    orig_connect: RwLock<Option<unsafe extern "C" fn(c_int, *const sockaddr, socklen_t) -> c_int>>,
    orig_send: RwLock<Option<unsafe extern "C" fn(c_int, *const c_void, size_t, c_int) -> isize>>,
    orig_recv: RwLock<Option<unsafe extern "C" fn(c_int, *mut c_void, size_t, c_int) -> isize>>,
    orig_close: RwLock<Option<unsafe extern "C" fn(c_int) -> c_int>>,
    /// Track intercepted sockets
    intercepted_sockets: RwLock<HashMap<c_int, InterceptedSocket>>,
}

/// Information about an intercepted socket
#[derive(Debug, Clone)]
pub struct InterceptedSocket {
    /// Original destination address
    pub dest_addr: SocketAddr,
    /// Proxy connection file descriptor
    pub proxy_fd: c_int,
    /// Connection state
    pub connected: bool,
}

impl InterceptState {
    /// Create new interception state
    pub fn new() -> Self {
        Self {
            config: InterceptConfig::default(),
            orig_socket: RwLock::new(None),
            orig_connect: RwLock::new(None),
            orig_send: RwLock::new(None),
            orig_recv: RwLock::new(None),
            orig_close: RwLock::new(None),
            intercepted_sockets: RwLock::new(HashMap::new()),
        }
    }

    /// Initialize function pointers (called on first use)
    pub fn init(&self) {
        unsafe {
            // Load original socket function
            let socket_sym = libc::dlsym(
                libc::RTLD_NEXT,
                CString::new("socket").unwrap().as_ptr(),
            );
            if !socket_sym.is_null() {
                *self.orig_socket.write() = Some(std::mem::transmute(socket_sym));
            }

            // Load original connect function
            let connect_sym = libc::dlsym(
                libc::RTLD_NEXT,
                CString::new("connect").unwrap().as_ptr(),
            );
            if !connect_sym.is_null() {
                *self.orig_connect.write() = Some(std::mem::transmute(connect_sym));
            }

            // Load original send function
            let send_sym = libc::dlsym(
                libc::RTLD_NEXT,
                CString::new("send").unwrap().as_ptr(),
            );
            if !send_sym.is_null() {
                *self.orig_send.write() = Some(std::mem::transmute(send_sym));
            }

            // Load original recv function
            let recv_sym = libc::dlsym(
                libc::RTLD_NEXT,
                CString::new("recv").unwrap().as_ptr(),
            );
            if !recv_sym.is_null() {
                *self.orig_recv.write() = Some(std::mem::transmute(recv_sym));
            }

            // Load original close function
            let close_sym = libc::dlsym(
                libc::RTLD_NEXT,
                CString::new("close").unwrap().as_ptr(),
            );
            if !close_sym.is_null() {
                *self.orig_close.write() = Some(std::mem::transmute(close_sym));
            }
        }

        info!("Socket interception initialized");
        info!("Proxy address: {}", self.config.proxy_addr);
        info!("Interception enabled: {}", self.config.enabled);
    }

    /// Get or initialize function pointers
    fn ensure_initialized(&self) {
        if self.orig_socket.read().is_none() {
            self.init();
        }
    }

    /// Intercept a connect call
    pub fn intercept_connect(&self, fd: c_int, addr: *const sockaddr, len: socklen_t) -> c_int {
        self.ensure_initialized();

        if !self.config.enabled {
            // Call original connect
            unsafe {
                if let Some(orig_connect) = *self.orig_connect.read() {
                    return orig_connect(fd, addr, len);
                }
            }
            return -1;
        }

        // Parse destination address
        let dest_addr = unsafe {
            match (*addr).sa_family as i32 {
                libc::AF_INET => {
                    let addr_in = &*(addr as *const libc::sockaddr_in);
                    let ip = std::net::Ipv4Addr::from(u32::from_be(addr_in.sin_addr.s_addr));
                    let port = u16::from_be(addr_in.sin_port);
                    SocketAddr::new(std::net::IpAddr::V4(ip), port)
                }
                libc::AF_INET6 => {
                    let addr_in6 = &*(addr as *const libc::sockaddr_in6);
                    let ip = std::net::Ipv6Addr::from(addr_in6.sin6_addr.s6_addr);
                    let port = u16::from_be(addr_in6.sin6_port);
                    SocketAddr::new(std::net::IpAddr::V6(ip), port)
                }
                _ => {
                    error!("Unknown address family: {}", (*addr).sa_family);
                    return -1;
                }
            }
        };

        info!("Intercepting connection to {}", dest_addr);

        // Connect to proxy instead
        let proxy_addr = self.config.proxy_addr;
        let proxy_sockaddr = socketaddr_to_sockaddr(proxy_addr);

        unsafe {
            if let Some(orig_connect) = *self.orig_connect.read() {
                let result = orig_connect(fd, &proxy_sockaddr, std::mem::size_of::<libc::sockaddr_in>() as socklen_t);
                
                if result == 0 {
                    // Successfully connected to proxy
                    // Send CONNECT request
                    let connect_request = format!("CONNECT {} HTTP/1.1\r\nHost: {}\r\n\r\n", 
                        dest_addr, dest_addr);
                    
                    let bytes = connect_request.as_bytes();
                    if let Some(orig_send) = *self.orig_send.read() {
                        let sent = orig_send(fd, bytes.as_ptr() as *const c_void, bytes.len(), 0);
                        if sent > 0 {
                            debug!("Sent CONNECT request for {}", dest_addr);
                            
                            // Read response
                            let mut response = [0u8; 256];
                            if let Some(orig_recv) = *self.orig_recv.read() {
                                let received = orig_recv(fd, response.as_mut_ptr() as *mut c_void, response.len(), 0);
                                if received > 0 {
                                    let response_str = String::from_utf8_lossy(&response[..received as usize]);
                                    if response_str.contains("200") {
                                        info!("CONNECT successful for {}", dest_addr);
                                        
                                        // Track this intercepted socket
                                        self.intercepted_sockets.write().insert(fd, InterceptedSocket {
                                            dest_addr,
                                            proxy_fd: fd,
                                            connected: true,
                                        });
                                        
                                        return 0;
                                    } else {
                                        error!("CONNECT failed: {}", response_str);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    error!("Failed to connect to proxy: {}", std::io::Error::last_os_error());
                }
            }
        }

        -1
    }

    /// Intercept a send call
    pub fn intercept_send(&self, fd: c_int, buf: *const c_void, len: size_t, flags: c_int) -> isize {
        self.ensure_initialized();

        // Check if this is an intercepted socket
        let sockets = self.intercepted_sockets.read();
        if let Some(socket) = sockets.get(&fd) {
            if socket.connected {
                // Send through proxy connection
                unsafe {
                    if let Some(orig_send) = *self.orig_send.read() {
                        return orig_send(socket.proxy_fd, buf, len, flags);
                    }
                }
            }
        }

        // Call original send
        unsafe {
            if let Some(orig_send) = *self.orig_send.read() {
                orig_send(fd, buf, len, flags)
            } else {
                -1
            }
        }
    }

    /// Intercept a recv call
    pub fn intercept_recv(&self, fd: c_int, buf: *mut c_void, len: size_t, flags: c_int) -> isize {
        self.ensure_initialized();

        // Check if this is an intercepted socket
        let sockets = self.intercepted_sockets.read();
        if let Some(socket) = sockets.get(&fd) {
            if socket.connected {
                // Receive from proxy connection
                unsafe {
                    if let Some(orig_recv) = *self.orig_recv.read() {
                        return orig_recv(socket.proxy_fd, buf, len, flags);
                    }
                }
            }
        }

        // Call original recv
        unsafe {
            if let Some(orig_recv) = *self.orig_recv.read() {
                orig_recv(fd, buf, len, flags)
            } else {
                -1
            }
        }
    }

    /// Intercept a close call
    pub fn intercept_close(&self, fd: c_int) -> c_int {
        self.ensure_initialized();

        // Remove from tracked sockets
        self.intercepted_sockets.write().remove(&fd);

        // Call original close
        unsafe {
            if let Some(orig_close) = *self.orig_close.read() {
                orig_close(fd)
            } else {
                -1
            }
        }
    }
}

/// Convert SocketAddr to sockaddr_in
fn socketaddr_to_sockaddr(addr: SocketAddr) -> sockaddr {
    match addr {
        SocketAddr::V4(addr_v4) => {
            let sockaddr_in = libc::sockaddr_in {
                sin_family: libc::AF_INET as u16,
                sin_port: addr_v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_be(addr_v4.ip().into()),
                },
                sin_zero: [0; 8],
            };
            unsafe { std::mem::transmute(sockaddr_in) }
        }
        SocketAddr::V6(_) => {
            // For simplicity, only IPv4 supported in this example
            panic!("IPv6 not supported in interception library");
        }
    }
}

/// Global interception state (lazy initialized)
static mut INTERCEPT_STATE: Option<InterceptState> = None;

/// Get global interception state
fn get_intercept_state() -> &'static InterceptState {
    unsafe {
        if INTERCEPT_STATE.is_none() {
            INTERCEPT_STATE = Some(InterceptState::new());
        }
        INTERCEPT_STATE.as_ref().unwrap()
    }
}

/// C interface for LD_PRELOAD interception

/// Intercepted socket function
#[no_mangle]
pub unsafe extern "C" fn socket(domain: c_int, type_: c_int, protocol: c_int) -> c_int {
    get_intercept_state().ensure_initialized();
    
    if let Some(orig_socket) = *get_intercept_state().orig_socket.read() {
        orig_socket(domain, type_, protocol)
    } else {
        -1
    }
}

/// Intercepted connect function
#[no_mangle]
pub unsafe extern "C" fn connect(sockfd: c_int, addr: *const sockaddr, addrlen: socklen_t) -> c_int {
    get_intercept_state().intercept_connect(sockfd, addr, addrlen)
}

/// Intercepted send function
#[no_mangle]
pub unsafe extern "C" fn send(sockfd: c_int, buf: *const c_void, len: size_t, flags: c_int) -> isize {
    get_intercept_state().intercept_send(sockfd, buf, len, flags)
}

/// Intercepted recv function
#[no_mangle]
pub unsafe extern "C" fn recv(sockfd: c_int, buf: *mut c_void, len: size_t, flags: c_int) -> isize {
    get_intercept_state().intercept_recv(sockfd, buf, len, flags)
}

/// Intercepted close function
#[no_mangle]
pub unsafe extern "C" fn close(fd: c_int) -> c_int {
    get_intercept_state().intercept_close(fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_env() {
        // Test default config
        let config = InterceptConfig::default();
        assert!(config.enabled);
        
        // Test custom proxy address
        env::set_var("ISA_PROXY", "192.168.1.1:9999");
        let config = InterceptConfig::default();
        assert_eq!(config.proxy_addr.to_string(), "192.168.1.1:9999");
        env::remove_var("ISA_PROXY");
    }

    #[test]
    fn test_socketaddr_conversion() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let sockaddr = socketaddr_to_sockaddr(addr);
        assert_eq!(sockaddr.sa_family, libc::AF_INET as u16);
    }
}
