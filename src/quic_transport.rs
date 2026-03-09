//! QUIC State Streaming Protocol (QSSP)
//!
//! Implements the QUIC-based transport protocol for streaming VM state
//! between snapshot and resume hosts.
//!
//! # Protocol Specification
//!
//! QSSP uses QUIC streams for multiplexed, ordered delivery:
//!
//! | Stream ID | Purpose                    | Priority |
//! |-----------|----------------------------|----------|
//! | 0         | Control messages           | Highest  |
//! | 1         | CPU State                  | High     |
//! | 2         | Memory HOT pages           | High     |
//! | 3         | Memory WARM pages          | Medium   |
//! | 4         | Memory COLD pages (on-demand) | Low  |
//! | 5         | Deterministic Event Log    | Medium   |
//!
//! # Message Format
//!
//! All messages use a length-prefixed binary format:
//! ```text
//! ┌─────────────┬──────────────┬──────────────┐
//! │  Magic (4B) │ MsgType (1B) │ Length (4B)  │
//! ├─────────────┴──────────────┴──────────────┤
//! │              Payload (variable)           │
//! └───────────────────────────────────────────┘
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use quinn::{Connection, ConnectionError, Endpoint, RecvStream, SendStream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use serde::{Deserialize, Serialize};

use crate::model::CPUState;
use crate::memory::{MemoryPage, PAGE_SIZE};

/// QSSP Magic number for protocol identification
pub const QSSP_MAGIC: u32 = 0x51535350; // "QSSP" in ASCII

/// Protocol version
pub const QSSP_VERSION: u8 = 1;

/// Message types
#[derive(Debug, Clone, Copy, PartialEq, Eq, u8)]
#[repr(u8)]
pub enum MessageType {
    HandshakeRequest = 0,
    HandshakeResponse = 1,
    CpuState = 2,
    MemoryPage = 3,
    MemoryManifest = 4,
    EventLog = 5,
    PageRequest = 6,
    PageNotFound = 7,
    TransferComplete = 8,
    TransferError = 9,
    KeepAlive = 10,
}

impl TryFrom<u8> for MessageType {
    type Error = QSSPError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(MessageType::HandshakeRequest),
            1 => Ok(MessageType::HandshakeResponse),
            2 => Ok(MessageType::CpuState),
            3 => Ok(MessageType::MemoryPage),
            4 => Ok(MessageType::MemoryManifest),
            5 => Ok(MessageType::EventLog),
            6 => Ok(MessageType::PageRequest),
            7 => Ok(MessageType::PageNotFound),
            8 => Ok(MessageType::TransferComplete),
            9 => Ok(MessageType::TransferError),
            10 => Ok(MessageType::KeepAlive),
            _ => Err(QSSPError::InvalidMessageType(value)),
        }
    }
}

/// QSSP Message header
#[derive(Debug, Clone)]
pub struct MessageHeader {
    pub magic: u32,
    pub version: u8,
    pub message_type: MessageType,
    pub length: u32,
}

impl MessageHeader {
    pub fn new(message_type: MessageType, length: u32) -> Self {
        Self {
            magic: QSSP_MAGIC,
            version: QSSP_VERSION,
            message_type,
            length,
        }
    }

    /// Serialize header to bytes
    pub fn serialize(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(10);
        buf.put_u32(self.magic);
        buf.put_u8(self.version);
        buf.put_u8(self.message_type as u8);
        buf.put_u32(self.length);
        buf.freeze()
    }

    /// Deserialize header from bytes
    pub fn deserialize(buf: &mut BytesMut) -> Result<Option<Self>, QSSPError> {
        // Need at least 10 bytes for header
        if buf.len() < 10 {
            return Ok(None);
        }

        let magic = buf.get_u32();
        if magic != QSSP_MAGIC {
            return Err(QSSPError::InvalidMagic(magic));
        }

        let version = buf.get_u8();
        if version != QSSP_VERSION {
            return Err(QSSPError::UnsupportedVersion(version));
        }

        let message_type = MessageType::try_from(buf.get_u8())?;
        let length = buf.get_u32();

        Ok(Some(Self {
            magic,
            version,
            message_type,
            length,
        }))
    }
}

/// QSSP Message with payload
#[derive(Debug, Clone)]
pub struct Message {
    pub header: MessageHeader,
    pub payload: Bytes,
}

impl Message {
    pub fn new(message_type: MessageType, payload: Bytes) -> Self {
        let header = MessageHeader::new(message_type, payload.len() as u32);
        Self { header, payload }
    }

    /// Serialize complete message
    pub fn serialize(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(10 + self.payload.len());
        buf.extend_from_slice(&self.header.serialize());
        buf.extend_from_slice(&self.payload);
        buf.freeze()
    }

    /// Deserialize message from stream
    pub async fn read_from_stream(recv: &mut RecvStream) -> Result<Self, QSSPError> {
        // Read header
        let mut header_buf = BytesMut::with_capacity(10);
        recv.read_buf(&mut header_buf).await?;
        
        let header = MessageHeader::deserialize(&mut header_buf)?
            .ok_or(QSSPError::IncompleteHeader)?;

        // Read payload
        let mut payload = BytesMut::with_capacity(header.length as usize);
        while payload.len() < header.length as usize {
            let remaining = header.length as usize - payload.len();
            let mut chunk = vec![0u8; remaining];
            let n = recv.read(&mut chunk).await?
                .ok_or(QSSPError::IncompletePayload)?;
            payload.extend_from_slice(&chunk[..n]);
        }

        Ok(Self {
            header,
            payload: payload.freeze(),
        })
    }
}

/// Handshake request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub client_id: String,
    pub state_id: String,
    pub transfer_mode: TransferMode,
}

/// Handshake response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub server_id: String,
    pub accepted: bool,
    pub error: Option<String>,
}

/// Transfer mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferMode {
    /// Full state transfer (snapshot)
    Full,
    /// Incremental/differential transfer
    Incremental,
    /// On-demand page fetching (resume)
    OnDemand,
}

/// Memory page message payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPageMessage {
    pub region_id: String,
    pub page_offset: u64,
    pub page_hash: String,
    pub compression: u8,
    pub original_size: u32,
    pub data: Vec<u8>,
}

impl MemoryPageMessage {
    pub fn from_page(page: &MemoryPage, region_id: &str, page_offset: u64) -> Self {
        Self {
            region_id: region_id.to_string(),
            page_offset,
            page_hash: page.page_hash.clone(),
            compression: page.compression as u8,
            original_size: page.original_size as u32,
            data: page.data.clone(),
        }
    }

    pub fn to_page(&self) -> MemoryPage {
        use crate::memory::CompressionAlgorithm;
        let compression = match self.compression {
            0 => CompressionAlgorithm::None,
            1 => CompressionAlgorithm::LZ4,
            2 => CompressionAlgorithm::Zstd,
            _ => CompressionAlgorithm::None,
        };

        MemoryPage {
            page_hash: self.page_hash.clone(),
            data: self.data.clone(),
            compression,
            original_size: self.original_size as usize,
        }
    }
}

/// Page request for on-demand fetching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRequest {
    pub region_id: String,
    pub page_offset: u64,
    pub page_hash: String,
}

/// Memory manifest for describing regions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryManifest {
    pub regions: Vec<crate::model::MemoryRegion>,
}

/// Transfer statistics
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    pub total_bytes: u64,
    pub total_pages: u64,
    pub hot_pages: u64,
    pub warm_pages: u64,
    pub cold_pages: u64,
    pub start_time: Option<std::time::Instant>,
    pub end_time: Option<std::time::Instant>,
}

impl TransferStats {
    pub fn duration_ms(&self) -> Option<u64> {
        match (self.start_time, self.end_time) {
            (Some(start), Some(end)) => Some(end.duration_since(start).as_millis() as u64),
            (Some(start), None) => Some(start.elapsed().as_millis() as u64),
            _ => None,
        }
    }
}

/// QSSP Sender - streams state to a receiver
pub struct QSSPSender {
    connection: Connection,
    stats: Arc<RwLock<TransferStats>>,
}

impl QSSPSender {
    /// Create a new sender from an established QUIC connection
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            stats: Arc::new(RwLock::new(TransferStats::default())),
        }
    }

    /// Send CPU state on stream 1
    pub async fn send_cpu_state(&self, cpu_state: &CPUState) -> Result<(), QSSPError> {
        let mut send = self.connection.open_uni().await?;
        send.set_priority(1); // High priority

        let payload = serde_json::to_vec(cpu_state)
            .map_err(|e| QSSPError::SerializationError(format!("CPU state: {}", e)))?;

        let msg = Message::new(MessageType::CpuState, Bytes::from(payload));
        send.write_all(&msg.serialize()).await?;
        send.finish()?;

        debug!("Sent CPU state");
        Ok(())
    }

    /// Send memory pages on stream 2 (HOT) or 3 (WARM)
    pub async fn send_memory_pages(
        &self,
        pages: &[MemoryPage],
        region_id: &str,
        base_offset: u64,
        is_hot: bool,
    ) -> Result<(), QSSPError> {
        let mut send = self.connection.open_uni().await?;
        
        // Set priority based on temperature
        if is_hot {
            send.set_priority(2); // High priority for HOT pages
        } else {
            send.set_priority(3); // Medium priority for WARM pages
        }

        let mut stats = self.stats.write().await;
        stats.start_time.get_or_insert(std::time::Instant::now());

        for (i, page) in pages.iter().enumerate() {
            let page_offset = base_offset + (i as u64);
            
            let page_msg = MemoryPageMessage::from_page(page, region_id, page_offset);
            let payload = serde_json::to_vec(&page_msg)
                .map_err(|e| QSSPError::SerializationError(format!("Page: {}", e)))?;

            let msg = Message::new(MessageType::MemoryPage, Bytes::from(payload));
            send.write_all(&msg.serialize()).await?;

            stats.total_bytes += page.data.len() as u64;
            stats.total_pages += 1;
            if is_hot {
                stats.hot_pages += 1;
            } else {
                stats.warm_pages += 1;
            }
        }

        send.finish()?;
        debug!("Sent {} memory pages", pages.len());
        Ok(())
    }

    /// Send memory manifest
    pub async fn send_manifest(&self, manifest: &MemoryManifest) -> Result<(), QSSPError> {
        let mut send = self.connection.open_uni().await?;
        send.set_priority(2);

        let payload = serde_json::to_vec(manifest)
            .map_err(|e| QSSPError::SerializationError(format!("Manifest: {}", e)))?;

        let msg = Message::new(MessageType::MemoryManifest, Bytes::from(payload));
        send.write_all(&msg.serialize()).await?;
        send.finish()?;

        debug!("Sent memory manifest");
        Ok(())
    }

    /// Send transfer complete notification
    pub async fn send_transfer_complete(&self) -> Result<(), QSSPError> {
        let mut send = self.connection.open_uni().await?;
        send.set_priority(0); // Control stream priority

        let msg = Message::new(MessageType::TransferComplete, Bytes::new());
        send.write_all(&msg.serialize()).await?;
        send.finish()?;

        let mut stats = self.stats.write().await;
        stats.end_time = Some(std::time::Instant::now());

        if let Some(duration) = stats.duration_ms() {
            info!(
                "Transfer complete: {} bytes, {} pages in {}ms",
                stats.total_bytes, stats.total_pages, duration
            );
        }

        Ok(())
    }

    /// Get transfer statistics
    pub async fn get_stats(&self) -> TransferStats {
        self.stats.read().await.clone()
    }
}

/// QSSP Receiver - receives state from a sender
pub struct QSSPReceiver {
    connection: Connection,
    cpu_state: Arc<RwLock<Option<CPUState>>>,
    memory_pages: Arc<RwLock<Vec<MemoryPage>>>,
    manifest: Arc<RwLock<Option<MemoryManifest>>>,
    stats: Arc<RwLock<TransferStats>>,
}

impl QSSPReceiver {
    /// Create a new receiver from an established QUIC connection
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            cpu_state: Arc::new(RwLock::new(None)),
            memory_pages: Arc::new(RwLock::new(Vec::new())),
            manifest: Arc::new(RwLock::new(None)),
            stats: Arc::new(RwLock::new(TransferStats::default())),
        }
    }

    /// Start receiving streams in background
    pub async fn start_receiving(&self) -> Result<(), QSSPError> {
        let mut stats = self.stats.write().await;
        stats.start_time = Some(std::time::Instant::now());
        drop(stats);

        loop {
            match self.connection.accept_uni().await {
                Ok(mut recv) => {
                    // Handle stream based on priority
                    tokio::spawn({
                        let cpu_state = self.cpu_state.clone();
                        let memory_pages = self.memory_pages.clone();
                        let manifest = self.manifest.clone();
                        let stats = self.stats.clone();

                        async move {
                            if let Err(e) = Self::handle_stream(
                                &mut recv,
                                &cpu_state,
                                &memory_pages,
                                &manifest,
                                &stats,
                            ).await {
                                error!("Stream error: {}", e);
                            }
                        }
                    });
                }
                Err(ConnectionError::NoConnection) => {
                    break;
                }
                Err(e) => {
                    error!("Accept error: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle a single stream
    async fn handle_stream(
        recv: &mut RecvStream,
        cpu_state: &Arc<RwLock<Option<CPUState>>>,
        memory_pages: &Arc<RwLock<Vec<MemoryPage>>>,
        manifest: &Arc<RwLock<Option<MemoryManifest>>>,
        stats: &Arc<RwLock<TransferStats>>,
    ) -> Result<(), QSSPError> {
        let msg = Message::read_from_stream(recv).await?;

        match msg.header.message_type {
            MessageType::CpuState => {
                let state: CPUState = serde_json::from_slice(&msg.payload)
                    .map_err(|e| QSSPError::DeserializationError(format!("CPU state: {}", e)))?;
                
                let mut cs = cpu_state.write().await;
                *cs = Some(state);
                debug!("Received CPU state");
            }
            MessageType::MemoryPage => {
                let page_msg: MemoryPageMessage = serde_json::from_slice(&msg.payload)
                    .map_err(|e| QSSPError::DeserializationError(format!("Page: {}", e)))?;
                
                let page = page_msg.to_page();
                
                let mut pages = memory_pages.write().await;
                pages.push(page);

                let mut st = stats.write().await;
                st.total_bytes += msg.payload.len() as u64;
                st.total_pages += 1;
            }
            MessageType::MemoryManifest => {
                let m: MemoryManifest = serde_json::from_slice(&msg.payload)
                    .map_err(|e| QSSPError::DeserializationError(format!("Manifest: {}", e)))?;
                
                let mut man = manifest.write().await;
                *man = Some(m);
                debug!("Received memory manifest");
            }
            MessageType::TransferComplete => {
                let mut st = stats.write().await;
                st.end_time = Some(std::time::Instant::now());
                
                if let Some(duration) = st.duration_ms() {
                    info!(
                        "Transfer complete: {} bytes, {} pages in {}ms",
                        st.total_bytes, st.total_pages, duration
                    );
                }
            }
            _ => {
                warn!("Unknown message type: {:?}", msg.header.message_type);
            }
        }

        Ok(())
    }

    /// Get received CPU state
    pub async fn get_cpu_state(&self) -> Option<CPUState> {
        self.cpu_state.read().await.clone()
    }

    /// Get received memory pages
    pub async fn get_memory_pages(&self) -> Vec<MemoryPage> {
        self.memory_pages.read().await.clone()
    }

    /// Get memory manifest
    pub async fn get_manifest(&self) -> Option<MemoryManifest> {
        self.manifest.read().await.clone()
    }

    /// Get transfer statistics
    pub async fn get_stats(&self) -> TransferStats {
        self.stats.read().await.clone()
    }
}

/// QSSP Endpoint builder
pub struct QSSPEndpoint {
    endpoint: Endpoint,
    config: QSSPConfig,
}

impl QSSPEndpoint {
    /// Create a new QSSP endpoint
    pub fn new(config: QSSPConfig) -> Result<Self, QSSPError> {
        let crypto_config = config.build_crypto_config()?;
        
        let mut endpoint = Endpoint::client(
            SocketAddr::from(([0, 0, 0, 0], config.port)),
        )?;

        let mut client_config = quinn::ClientConfig::new(crypto_config.clone());
        let mut transport_config = quinn::TransportConfig::default();
        transport_config.max_concurrent_uni_streams(10u32.into());
        client_config.transport_config(Arc::new(transport_config));
        
        endpoint.set_default_client_config(client_config);

        Ok(Self { endpoint, config })
    }

    /// Connect to a remote QSSP endpoint
    pub async fn connect(&self, addr: SocketAddr, state_id: &str) -> Result<QSSPSender, QSSPError> {
        let connection = self.endpoint
            .connect(addr, "isa-qssp.local")?
            .await?;

        let sender = QSSPSender::new(connection);

        // Send handshake
        let handshake = HandshakeRequest {
            client_id: uuid::Uuid::new_v4().to_string(),
            state_id: state_id.to_string(),
            transfer_mode: TransferMode::Full,
        };

        // Handshake would be sent over control stream in full implementation

        Ok(sender)
    }

    /// Accept incoming connections (server mode)
    pub async fn accept(&self) -> Result<Connection, QSSPError> {
        let incoming = self.endpoint.accept().await
            .ok_or(QSSPError::NoIncomingConnection)?;
        
        let connection = incoming.await?;
        Ok(connection)
    }
}

/// QSSP Configuration
#[derive(Debug, Clone)]
pub struct QSSPConfig {
    /// Local port to bind
    pub port: u16,
    /// TLS certificate (DER format)
    pub certificate: Option<CertificateDer<'static>>,
    /// TLS private key
    pub private_key: Option<PrivateKeyDer<'static>>,
    /// Enable TLS (production) or use null crypto (dev)
    pub use_tls: bool,
    /// Connection timeout
    pub timeout_secs: u64,
}

impl Default for QSSPConfig {
    fn default() -> Self {
        Self {
            port: 4433,
            certificate: None,
            private_key: None,
            use_tls: false, // Dev mode default
            timeout_secs: 30,
        }
    }
}

impl QSSPConfig {
    /// Build crypto config from settings
    pub fn build_crypto_config(&self) -> Result<rustls::ClientConfig, QSSPError> {
        let mut config = rustls::ClientConfig::builder()
            .with_no_client_auth()
            .withdangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification));

        Ok(config)
    }
}

/// No certificate verification (for development)
#[derive(Debug)]
struct NoCertificateVerification;

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// QSSP Errors
#[derive(Debug, thiserror::Error)]
pub enum QSSPError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("QUIC error: {0}")]
    QuicError(#[from] quinn::ConnectError),

    #[error("Connection error: {0}")]
    ConnectionError(#[from] ConnectionError),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Deserialization error: {0}")]
    DeserializationError(String),

    #[error("Invalid magic number: {0}")]
    InvalidMagic(u32),

    #[error("Unsupported version: {0}")]
    UnsupportedVersion(u8),

    #[error("Invalid message type: {0}")]
    InvalidMessageType(u8),

    #[error("Incomplete header")]
    IncompleteHeader,

    #[error("Incomplete payload")]
    IncompletePayload,

    #[error("No incoming connection")]
    NoIncomingConnection,

    #[error("Handshake failed: {0}")]
    HandshakeFailed(String),

    #[error("Timeout: {0}")]
    Timeout(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_header_serialization() {
        let header = MessageHeader::new(MessageType::CpuState, 1024);
        let bytes = header.serialize();
        
        assert_eq!(bytes.len(), 10);
        assert_eq!(&bytes[0..4], &QSSP_MAGIC.to_be_bytes());
    }

    #[test]
    fn test_message_serialization() {
        let payload = Bytes::from(vec![1u8, 2, 3, 4]);
        let msg = Message::new(MessageType::MemoryPage, payload.clone());
        let bytes = msg.serialize();
        
        assert_eq!(bytes.len(), 10 + payload.len());
    }

    #[test]
    fn test_message_type_conversion() {
        assert_eq!(MessageType::try_from(0u8).unwrap(), MessageType::HandshakeRequest);
        assert_eq!(MessageType::try_from(2u8).unwrap(), MessageType::CpuState);
        assert!(MessageType::try_from(99u8).is_err());
    }

    #[test]
    fn test_memory_page_message() {
        use crate::memory::{MemoryPage, CompressionAlgorithm};
        
        let page = MemoryPage {
            page_hash: "abc123".to_string(),
            data: vec![1u8; 100],
            compression: CompressionAlgorithm::LZ4,
            original_size: PAGE_SIZE,
        };

        let msg = MemoryPageMessage::from_page(&page, "test-region", 0);
        let restored = msg.to_page();

        assert_eq!(restored.page_hash, page.page_hash);
        assert_eq!(restored.data, page.data);
    }
}
