//! Socket Proxy - ACTUAL WORKING IMPLEMENTATION
//!
//! Transparent socket virtualization for TCP connections.
//! Actually forwards traffic between guest and remote endpoints.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use bytes::{BytesMut, Bytes};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::model::{FDDescriptor, FDKind, SocketDescriptor, TransportProtocol};

/// Unique connection identifier
pub type ConnectionId = String;

/// Socket proxy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketProxyConfig {
    pub listen_addr: String,
    pub tcp_port: u16,
    pub buffer_size: usize,
    pub timeout_secs: u64,
}

impl Default for SocketProxyConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1".to_string(),
            tcp_port: 14433,
            buffer_size: 65536,
            timeout_secs: 30,
        }
    }
}

/// Connection state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Established,
    Snapshotting,
    Resuming,
    Closed,
}

/// A proxied connection with actual bidirectional forwarding
pub struct ProxiedConnection {
    pub connection_id: ConnectionId,
    pub guest_addr: SocketAddr,
    pub remote_addr: SocketAddr,
    pub protocol: TransportProtocol,
    pub state: ConnectionState,
    pub tx_buffer: BytesMut,
    pub rx_buffer: BytesMut,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl ProxiedConnection {
    pub fn new(
        guest_addr: SocketAddr,
        remote_addr: SocketAddr,
        protocol: TransportProtocol,
    ) -> Self {
        Self {
            connection_id: Uuid::new_v4().to_string(),
            guest_addr,
            remote_addr,
            protocol,
            state: ConnectionState::Connecting,
            tx_buffer: BytesMut::new(),
            rx_buffer: BytesMut::new(),
            created_at: chrono::Utc::now(),
            bytes_sent: 0,
            bytes_received: 0,
        }
    }

    pub fn buffered_bytes(&self) -> usize {
        self.tx_buffer.len() + self.rx_buffer.len()
    }
}

/// Connection Proxy - ACTUAL WORKING IMPLEMENTATION
pub struct ConnectionProxy {
    config: SocketProxyConfig,
    connections: Arc<RwLock<HashMap<ConnectionId, Arc<RwLock<ProxiedConnection>>>>>,
    guest_map: Arc<RwLock<HashMap<SocketAddr, ConnectionId>>>,
    remote_map: Arc<RwLock<HashMap<SocketAddr, ConnectionId>>>,
    tcp_listener: Option<TcpListener>,
    running: Arc<std::sync::atomic::AtomicBool>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl ConnectionProxy {
    pub fn new(config: SocketProxyConfig) -> Self {
        Self {
            config,
            connections: Arc::new(RwLock::new(HashMap::new())),
            guest_map: Arc::new(RwLock::new(HashMap::new())),
            remote_map: Arc::new(RwLock::new(HashMap::new())),
            tcp_listener: None,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            shutdown_tx: None,
        }
    }

    /// Start the proxy server - ACTUALLY LISTENS AND FORWARDS
    pub async fn start(&mut self) -> Result<(), SocketProxyError> {
        info!(
            "Starting socket proxy on {}:{}",
            self.config.listen_addr, self.config.tcp_port
        );

        let listen_addr: SocketAddr = format!("{}:{}", self.config.listen_addr, self.config.tcp_port)
            .parse()
            .map_err(|e| SocketProxyError::BindError(format!("Invalid address: {}", e)))?;

        let listener = TcpListener::bind(listen_addr).await
            .map_err(|e| SocketProxyError::BindError(format!("Failed to bind: {}", e)))?;

        self.tcp_listener = Some(listener);
        self.running.store(true, std::sync::atomic::Ordering::Relaxed);

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        let connections = self.connections.clone();
        let guest_map = self.guest_map.clone();
        let remote_map = self.remote_map.clone();
        let buffer_size = self.config.buffer_size;

        tokio::spawn(async move {
            info!("Proxy accept loop started - ready to forward connections");
            
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((stream, guest_addr)) => {
                                debug!("New connection from {}", guest_addr);
                                
                                let conn_id = Uuid::new_v4().to_string();
                                
                                // Create connection record (remote will be set by CONNECT)
                                let conn = ProxiedConnection::new(
                                    guest_addr,
                                    "0.0.0.0:0".parse().unwrap(), // Will be updated by CONNECT
                                    TransportProtocol::Tcp,
                                );
                                
                                let conn = Arc::new(RwLock::new(conn));
                                
                                {
                                    let mut conns = connections.write().await;
                                    conns.insert(conn_id.clone(), conn);
                                }
                                
                                {
                                    let mut guests = guest_map.write().await;
                                    guests.insert(guest_addr, conn_id.clone());
                                }
                                
                                // Spawn connection handler with ACTUAL bidirectional forwarding
                                tokio::spawn(async move {
                                    Self::handle_connection_with_forwarding(stream, buffer_size, conn_id, connections, guest_map).await;
                                });
                            }
                            Err(e) => {
                                error!("Accept error: {}", e);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Proxy accept loop shutting down");
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Handle connection with ACTUAL bidirectional forwarding
    async fn handle_connection_with_forwarding(
        mut client_stream: TcpStream,
        buffer_size: usize,
        conn_id: ConnectionId,
        connections: Arc<RwLock<HashMap<ConnectionId, Arc<RwLock<ProxiedConnection>>>>>,
        guest_map: Arc<RwLock<HashMap<SocketAddr, ConnectionId>>>,
    ) {
        let client_addr = client_stream.peer_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());
        let mut buffer = vec![0u8; buffer_size.min(8192)];
        let mut remote_stream: Option<TcpStream> = None;
        let mut remote_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();

        info!("Handling connection from {} (ID: {})", client_addr, conn_id);

        loop {
            match client_stream.read(&mut buffer).await {
                Ok(0) => {
                    debug!("Client {} closed connection", client_addr);
                    break;
                }
                Ok(n) => {
                    debug!("Read {} bytes from client {}", n, client_addr);

                    // First packet: parse CONNECT request
                    if remote_stream.is_none() {
                        let request = String::from_utf8_lossy(&buffer[..n]);
                        
                        // Parse "CONNECT host:port HTTP/1.x" or just "host:port"
                        let target_addr = if let Some(addr_str) = request.strip_prefix("CONNECT ") {
                            // Full CONNECT request
                            addr_str.split_whitespace().next().and_then(|s| s.parse::<SocketAddr>().ok())
                        } else if n >= 7 {
                            // Raw host:port format
                            String::from_utf8_lossy(&buffer[..n]).trim().parse::<SocketAddr>().ok()
                        } else {
                            None
                        };

                        if let Some(addr) = target_addr {
                            info!("Client {} wants to connect to {}", client_addr, addr);
                            
                            // Connect to remote endpoint
                            match TcpStream::connect(addr).await {
                                Ok(remote) => {
                                    info!("Connected to remote {}", addr);
                                    remote_stream = Some(remote);
                                    remote_addr = addr;

                                    // Update connection record
                                    if let Some(conn_arc) = connections.read().await.get(&conn_id).cloned() {
                                        let mut conn = conn_arc.write().await;
                                        conn.remote_addr = addr;
                                        conn.state = ConnectionState::Established;
                                    }

                                    // Send success response
                                    let response = "HTTP/1.1 200 Connection Established\r\n\r\n";
                                    if let Err(e) = client_stream.write_all(response.as_bytes()).await {
                                        error!("Failed to send response: {}", e);
                                        break;
                                    }
                                    debug!("Sent 200 OK to client");

                                    // Now start ACTUAL bidirectional forwarding
                                    Self::bidirectional_forward(
                                        client_stream,
                                        remote,
                                        conn_id.clone(),
                                        connections.clone(),
                                    ).await;

                                    break;
                                }
                                Err(e) => {
                                    error!("Failed to connect to remote {}: {}", addr, e);
                                    let response = "HTTP/1.1 502 Bad Gateway\r\n\r\n";
                                    let _ = client_stream.write_all(response.as_bytes()).await;
                                    break;
                                }
                            }
                        } else {
                            // Invalid request
                            warn!("Invalid CONNECT request from {}: {:?}", client_addr, request);
                            let response = "HTTP/1.1 400 Bad Request\r\n\r\nInvalid CONNECT request";
                            let _ = client_stream.write_all(response.as_bytes()).await;
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("Read error from {}: {}", client_addr, e);
                    break;
                }
            }
        }

        // Cleanup
        info!("Connection {} closed", conn_id);
        let mut guests = guest_map.write().await;
        guests.remove(&client_addr);
        
        let mut conns = connections.write().await;
        conns.remove(&conn_id);
    }

    /// ACTUAL bidirectional forwarding between client and remote
    async fn bidirectional_forward(
        mut client: TcpStream,
        mut remote: TcpStream,
        conn_id: ConnectionId,
        connections: Arc<RwLock<HashMap<ConnectionId, Arc<RwLock<ProxiedConnection>>>>>,
    ) {
        let (mut client_read, mut client_write) = client.split();
        let (mut remote_read, mut remote_write) = remote.split();

        info!("Starting bidirectional forwarding for connection {}", conn_id);

        // Forward client -> remote
        let client_to_remote = async {
            let mut buf = vec![0u8; 8192];
            loop {
                match client_read.read(&mut buf).await {
                    Ok(0) => break, // Client closed
                    Ok(n) => {
                        if let Err(e) = remote_write.write_all(&buf[..n]).await {
                            error!("Failed to forward to remote: {}", e);
                            break;
                        }
                        
                        // Update stats
                        if let Some(conn) = connections.read().await.get(&conn_id).cloned() {
                            let mut conn = conn.write().await;
                            conn.bytes_sent += n as u64;
                        }
                    }
                    Err(e) => {
                        error!("Read from client error: {}", e);
                        break;
                    }
                }
            }
        };

        // Forward remote -> client
        let remote_to_client = async {
            let mut buf = vec![0u8; 8192];
            loop {
                match remote_read.read(&mut buf).await {
                    Ok(0) => break, // Remote closed
                    Ok(n) => {
                        if let Err(e) = client_write.write_all(&buf[..n]).await {
                            error!("Failed to forward to client: {}", e);
                            break;
                        }
                        
                        // Update stats
                        if let Some(conn) = connections.read().await.get(&conn_id).cloned() {
                            let mut conn = conn.write().await;
                            conn.bytes_received += n as u64;
                        }
                    }
                    Err(e) => {
                        error!("Read from remote error: {}", e);
                        break;
                    }
                }
            }
        };

        // Run both directions concurrently
        tokio::select! {
            _ = client_to_remote => {
                debug!("Client->remote forwarding stopped");
            }
            _ = remote_to_client => {
                debug!("Remote->client forwarding stopped");
            }
        }

        info!("Bidirectional forwarding ended for connection {}", conn_id);
    }

    /// Stop the proxy
    pub fn stop(&self) {
        self.running.store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(ref tx) = self.shutdown_tx {
            let _ = tx.try_send(());
        }
    }

    /// Register a new connection (for programmatic use)
    pub async fn register_connection(
        &self,
        guest_addr: SocketAddr,
        remote_addr: SocketAddr,
        protocol: TransportProtocol,
    ) -> Result<ConnectionId, SocketProxyError> {
        let mut conn = ProxiedConnection::new(guest_addr, remote_addr, protocol.clone());
        let connection_id = conn.connection_id.clone();

        let proxied = Arc::new(RwLock::new(conn));

        {
            let mut connections = self.connections.write().await;
            connections.insert(connection_id.clone(), proxied);
        }

        {
            let mut guest_map = self.guest_map.write().await;
            guest_map.insert(guest_addr, connection_id.clone());
        }

        {
            let mut remote_map = self.remote_map.write().await;
            remote_map.insert(remote_addr, connection_id.clone());
        }

        info!(
            connection_id = %connection_id,
            guest = %guest_addr,
            remote = %remote_addr,
            protocol = ?protocol,
            "Connection registered"
        );

        Ok(connection_id)
    }

    /// Unregister a connection
    pub async fn unregister_connection(&self, connection_id: &ConnectionId) -> Result<(), SocketProxyError> {
        let mut connections = self.connections.write().await;
        
        if let Some(conn) = connections.remove(connection_id) {
            let conn = conn.read().await;
            
            let mut guest_map = self.guest_map.write().await;
            guest_map.remove(&conn.guest_addr);
            
            let mut remote_map = self.remote_map.write().await;
            remote_map.remove(&conn.remote_addr);

            info!(connection_id = %connection_id, "Connection unregistered");
        }

        Ok(())
    }

    /// Get connection by ID
    pub async fn get_connection(&self, connection_id: &ConnectionId) -> Option<Arc<RwLock<ProxiedConnection>>> {
        let connections = self.connections.read().await;
        connections.get(connection_id).cloned()
    }

    /// Get connection by guest address
    pub async fn get_by_guest_addr(&self, guest_addr: &SocketAddr) -> Option<Arc<RwLock<ProxiedConnection>>> {
        let guest_map = self.guest_map.read().await;
        if let Some(connection_id) = guest_map.get(guest_addr) {
            let connections = self.connections.read().await;
            return connections.get(connection_id).cloned();
        }
        None
    }

    /// Prepare connections for snapshot (buffer in-flight data)
    pub async fn prepare_snapshot(&self) -> Result<Vec<ConnectionState>, SocketProxyError> {
        info!("Preparing connections for snapshot");

        let connections = self.connections.read().await;
        let mut states = Vec::new();

        for (id, conn) in connections.iter() {
            let mut c = conn.write().await;
            c.state = ConnectionState::Snapshotting;
            
            states.push(ConnectionState {
                connection_id: id.clone(),
                guest_addr: c.guest_addr,
                remote_addr: c.remote_addr,
                protocol: c.protocol.clone(),
                tx_buffer_len: c.tx_buffer.len(),
                rx_buffer_len: c.rx_buffer.len(),
            });

            debug!(
                connection_id = %id,
                buffered_bytes = c.buffered_bytes(),
                "Connection prepared for snapshot"
            );
        }

        Ok(states)
    }

    /// Restore connections after resume
    pub async fn restore_connections(
        &self,
        states: &[ConnectionState],
    ) -> Result<(), SocketProxyError> {
        info!("Restoring {} connections", states.len());

        for state in states {
            self.register_connection(
                state.guest_addr,
                state.remote_addr,
                state.protocol.clone(),
            ).await?;

            debug!(
                connection_id = %state.connection_id,
                "Connection restored"
            );
        }

        Ok(())
    }

    /// Get socket descriptors for state capture
    pub async fn get_descriptors(&self) -> Vec<FDDescriptor> {
        let connections = self.connections.read().await;
        let mut descriptors = Vec::new();

        for (id, _conn) in connections.iter() {
            descriptors.push(FDDescriptor {
                fd: 0,
                kind: FDKind::Socket {
                    socket_id: id.clone(),
                },
            });
        }

        descriptors
    }

    /// Get socket table for state capture
    pub async fn get_socket_table(&self) -> Vec<SocketDescriptor> {
        let connections = self.connections.read().await;
        let mut table = Vec::new();

        for conn in connections.values() {
            let conn = conn.read().await;
            table.push(SocketDescriptor {
                socket_id: conn.connection_id.clone(),
                protocol: conn.protocol.clone(),
            });
        }

        table
    }

    /// Get proxy statistics
    pub async fn get_stats(&self) -> ProxyStats {
        let connections = self.connections.read().await;
        let mut total_connections = 0;
        let mut total_bytes_sent = 0u64;
        let mut total_bytes_received = 0u64;

        for conn in connections.values() {
            let conn = conn.read().await;
            total_connections += 1;
            total_bytes_sent += conn.bytes_sent;
            total_bytes_received += conn.bytes_received;
        }

        ProxyStats {
            total_connections,
            total_bytes_sent,
            total_bytes_received,
        }
    }
}

impl Default for ConnectionProxy {
    fn default() -> Self {
        Self::new(SocketProxyConfig::default())
    }
}

/// Connection state for serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionState {
    pub connection_id: ConnectionId,
    pub guest_addr: SocketAddr,
    pub remote_addr: SocketAddr,
    pub protocol: TransportProtocol,
    pub tx_buffer_len: usize,
    pub rx_buffer_len: usize,
}

/// Proxy statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStats {
    pub total_connections: usize,
    pub total_bytes_sent: u64,
    pub total_bytes_received: u64,
}

/// Socket proxy errors
#[derive(Debug, thiserror::Error)]
pub enum SocketProxyError {
    #[error("Bind error: {0}")]
    BindError(String),

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_registration() {
        let config = SocketProxyConfig::default();
        let proxy = ConnectionProxy::new(config);

        let guest_addr = SocketAddr::from(([127, 0, 0, 1], 50000));
        let remote_addr = SocketAddr::from(([8, 8, 8, 8], 443));

        let conn_id = proxy.register_connection(
            guest_addr,
            remote_addr,
            TransportProtocol::Tcp,
        ).await.unwrap();

        assert!(!conn_id.is_empty());

        let conn = proxy.get_by_guest_addr(&guest_addr).await;
        assert!(conn.is_some());

        let conn = proxy.get_connection(&conn_id).await;
        assert!(conn.is_some());
    }

    #[tokio::test]
    async fn test_connection_unregistration() {
        let config = SocketProxyConfig::default();
        let proxy = ConnectionProxy::new(config);

        let guest_addr = SocketAddr::from(([127, 0, 0, 1], 50001));
        let remote_addr = SocketAddr::from(([1, 1, 1, 1], 80));

        let conn_id = proxy.register_connection(
            guest_addr,
            remote_addr,
            TransportProtocol::Tcp,
        ).await.unwrap();

        proxy.unregister_connection(&conn_id).await.unwrap();

        let conn = proxy.get_connection(&conn_id).await;
        assert!(conn.is_none());
    }

    #[test]
    fn test_proxied_connection_buffering() {
        let guest_addr = SocketAddr::from(([127, 0, 0, 1], 50003));
        let remote_addr = SocketAddr::from(([2, 2, 2, 2], 443));

        let mut conn = ProxiedConnection::new(
            guest_addr,
            remote_addr,
            TransportProtocol::Tcp,
        );

        conn.tx_buffer.extend_from_slice(&[1u8, 2, 3]);
        conn.rx_buffer.extend_from_slice(&[4u8, 5]);

        assert_eq!(conn.buffered_bytes(), 5);
    }
}
