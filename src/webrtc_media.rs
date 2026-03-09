//! WebRTC Media Transport - RTP/RTCP Implementation
//!
//! Provides actual media transport for WebRTC streaming.
//! Implements RTP packetization and RTCP feedback.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
//! │ VideoFrame  │───▶│  RTP        │───▶│  UDP        │
//! │             │    │  Packetizer │    │  Socket     │
//! └─────────────┘    └─────────────┘    └─────────────┘
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// RTP packet structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtpPacket {
    /// RTP version (2)
    pub version: u8,
    /// Padding flag
    pub padding: bool,
    /// Extension flag
    pub extension: bool,
    /// CSRC count
    pub csrc_count: u8,
    /// Marker bit
    pub marker: bool,
    /// Payload type
    pub payload_type: u8,
    /// Sequence number
    pub sequence_number: u16,
    /// Timestamp
    pub timestamp: u32,
    /// SSRC (synchronization source)
    pub ssrc: u32,
    /// Payload data
    pub payload: Vec<u8>,
}

impl RtpPacket {
    /// Create a new RTP packet
    pub fn new(payload_type: u8, sequence_number: u16, timestamp: u32, ssrc: u32, payload: Vec<u8>) -> Self {
        Self {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            payload,
        }
    }

    /// Serialize RTP packet to bytes
    pub fn serialize(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(12 + self.payload.len());
        
        // First byte: version, padding, extension, CSRC count
        let first_byte = (self.version << 6) 
            | ((if self.padding { 1 } else { 0 }) << 5)
            | ((if self.extension { 1 } else { 0 }) << 4)
            | self.csrc_count;
        buf.put_u8(first_byte);
        
        // Second byte: marker, payload type
        let second_byte = ((if self.marker { 1 } else { 0 }) << 7) | self.payload_type;
        buf.put_u8(second_byte);
        
        // Sequence number
        buf.put_u16(self.sequence_number);
        
        // Timestamp
        buf.put_u32(self.timestamp);
        
        // SSRC
        buf.put_u32(self.ssrc);
        
        // Payload
        buf.put_slice(&self.payload);
        
        buf.freeze()
    }

    /// Deserialize RTP packet from bytes
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }
        
        let first_byte = data[0];
        let version = (first_byte >> 6) & 0x03;
        if version != 2 {
            return None;
        }
        
        let padding = (first_byte & 0x20) != 0;
        let extension = (first_byte & 0x10) != 0;
        let csrc_count = first_byte & 0x0F;
        
        let second_byte = data[1];
        let marker = (second_byte & 0x80) != 0;
        let payload_type = second_byte & 0x7F;
        
        let sequence_number = u16::from_be_bytes([data[2], data[3]]);
        let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
        
        let header_size = 12 + (csrc_count as usize * 4);
        let payload = data[header_size..].to_vec();
        
        Some(Self {
            version,
            padding,
            extension,
            csrc_count,
            marker,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            payload,
        })
    }
}

/// RTCP packet types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RtcpPacketType {
    SenderReport = 200,
    ReceiverReport = 201,
    SourceDescription = 202,
    Bye = 203,
    ApplicationDefined = 204,
    TransportLayerNack = 205,
    PayloadSpecificFeedback = 206,
}

/// RTCP Sender Report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtcpSenderReport {
    /// SSRC of sender
    pub ssrc: u32,
    /// NTP timestamp (seconds)
    pub ntp_secs: u32,
    /// NTP timestamp (fraction)
    pub ntp_frac: u32,
    /// RTP timestamp
    pub rtp_timestamp: u32,
    /// Sender's packet count
    pub packet_count: u32,
    /// Sender's octet count
    pub octet_count: u32,
}

impl RtcpSenderReport {
    /// Serialize RTCP sender report
    pub fn serialize(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(28);
        
        // Version, padding, reception report count
        buf.put_u8(0x80);
        // Packet type (Sender Report)
        buf.put_u8(RtcpPacketType::SenderReport as u8);
        // Length (in 32-bit words, minus one)
        buf.put_u16(6);
        
        // SSRC
        buf.put_u32(self.ssrc);
        // NTP timestamp
        buf.put_u32(self.ntp_secs);
        buf.put_u32(self.ntp_frac);
        // RTP timestamp
        buf.put_u32(self.rtp_timestamp);
        // Packet count
        buf.put_u32(self.packet_count);
        // Octet count
        buf.put_u32(self.octet_count);
        
        buf.freeze()
    }
}

/// RTP session statistics
#[derive(Debug, Clone, Default)]
pub struct RtpStats {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub packets_lost: u64,
    pub jitter: f64,
    pub round_trip_time_ms: Option<f64>,
}

/// RTP session for a single media stream
pub struct RtpSession {
    /// Session ID
    pub session_id: String,
    /// Local SSRC
    pub local_ssrc: u32,
    /// Remote SSRC
    pub remote_ssrc: Option<u32>,
    /// Payload type
    pub payload_type: u8,
    /// Sequence number (incrementing)
    pub sequence_number: u16,
    /// Timestamp base
    pub timestamp_base: u32,
    /// UDP socket for sending
    pub send_socket: Option<Arc<UdpSocket>>,
    /// Remote address for sending
    pub remote_addr: Option<SocketAddr>,
    /// Statistics
    pub stats: Arc<RwLock<RtpStats>>,
    /// Running flag
    pub running: bool,
}

impl RtpSession {
    /// Create a new RTP session
    pub fn new(payload_type: u8) -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            local_ssrc: rand::random(),
            remote_ssrc: None,
            payload_type,
            sequence_number: 0,
            timestamp_base: rand::random(),
            send_socket: None,
            remote_addr: None,
            stats: Arc::new(RwLock::new(RtpStats::default())),
            running: false,
        }
    }

    /// Set the remote destination for RTP packets
    pub fn set_remote(&mut self, addr: SocketAddr, socket: Arc<UdpSocket>) {
        self.remote_addr = Some(addr);
        self.send_socket = Some(socket);
    }

    /// Send an RTP packet
    pub async fn send_packet(&mut self, payload: Vec<u8>, timestamp: u32, marker: bool) -> Result<(), RtpError> {
        let remote_addr = self.remote_addr.ok_or(RtpError::NotConnected)?;
        let socket = self.send_socket.clone().ok_or(RtpError::NotConnected)?;
        
        // Create RTP packet
        let packet = RtpPacket {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker,
            payload_type: self.payload_type,
            sequence_number: self.sequence_number,
            timestamp: self.timestamp_base.wrapping_add(timestamp),
            ssrc: self.local_ssrc,
            payload,
        };
        
        let data = packet.serialize();
        
        // Send packet
        let sent = socket.send_to(&data, remote_addr).await
            .map_err(|e| RtpError::SendError(e.to_string()))?;
        
        // Update sequence number
        self.sequence_number = self.sequence_number.wrapping_add(1);
        
        // Update stats
        let mut stats = self.stats.write().await;
        stats.packets_sent += 1;
        stats.bytes_sent += sent as u64;
        
        debug!("Sent RTP packet seq={} size={}", self.sequence_number, sent);
        
        Ok(())
    }

    /// Send RTCP sender report
    pub async fn send_sender_report(&self) -> Result<(), RtpError> {
        let remote_addr = self.remote_addr.ok_or(RtpError::NotConnected)?;
        let socket = self.send_socket.clone().ok_or(RtpError::NotConnected)?;
        
        // Get current time for NTP timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let ntp_secs = (now.as_secs() + 2208988800) as u32; // Convert to NTP epoch
        // Convert sub-second nanoseconds to NTP fractional units
        let ntp_frac = ((now.subsec_nanos() as u64 * (1u64 << 32)) / 1_000_000_000) as u32;
        
        let stats = self.stats.read().await;
        
        let report = RtcpSenderReport {
            ssrc: self.local_ssrc,
            ntp_secs,
            ntp_frac,
            rtp_timestamp: self.timestamp_base,
            packet_count: stats.packets_sent as u32,
            octet_count: stats.bytes_sent as u32,
        };
        
        let data = report.serialize();
        
        socket.send_to(&data, remote_addr).await
            .map_err(|e| RtpError::SendError(e.to_string()))?;
        
        debug!("Sent RTCP sender report");
        
        Ok(())
    }

    /// Get session statistics
    pub async fn get_stats(&self) -> RtpStats {
        self.stats.read().await.clone()
    }
}

/// RTP packetizer for video frames
pub struct VideoPacketizer {
    /// Maximum RTP payload size (MTU - headers)
    pub max_payload_size: usize,
    /// Payload type for video
    pub payload_type: u8,
    /// Clock rate (90000 for video)
    pub clock_rate: u32,
}

impl Default for VideoPacketizer {
    fn default() -> Self {
        Self {
            max_payload_size: 1200, // Leave room for IP/UDP/RTP headers
            payload_type: 96, // Dynamic payload type for H.264
            clock_rate: 90000,
        }
    }
}

impl VideoPacketizer {
    /// Create a new video packetizer
    pub fn new(payload_type: u8, clock_rate: u32) -> Self {
        Self {
            max_payload_size: 1200,
            payload_type,
            clock_rate,
        }
    }

    /// Packetize a video frame into RTP packets
    pub fn packetize(&self, frame_data: &[u8], timestamp: u32, is_keyframe: bool) -> Vec<RtpPacket> {
        let mut packets = Vec::new();
        let mut offset = 0;
        let mut seq = 0;
        
        while offset < frame_data.len() {
            let remaining = frame_data.len() - offset;
            let payload_size = remaining.min(self.max_payload_size);
            let is_last = remaining <= self.max_payload_size;
            
            let payload = frame_data[offset..offset + payload_size].to_vec();
            
            let packet = RtpPacket {
                version: 2,
                padding: false,
                extension: false,
                csrc_count: 0,
                marker: is_last || is_keyframe, // Marker on last packet or keyframe
                payload_type: self.payload_type,
                sequence_number: seq,
                timestamp,
                ssrc: 0, // Will be set by session
                payload,
            };
            
            packets.push(packet);
            offset += payload_size;
            seq += 1;
        }
        
        packets
    }
}

/// RTP errors
#[derive(Debug, thiserror::Error)]
pub enum RtpError {
    #[error("Not connected")]
    NotConnected,
    
    #[error("Send error: {0}")]
    SendError(String),
    
    #[error("Recv error: {0}")]
    RecvError(String),
    
    #[error("Invalid packet: {0}")]
    InvalidPacket(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// WebRTC media sender - integrates RTP with WebRTC transport
pub struct WebRtcMediaSender {
    /// RTP session for video
    pub video_session: RtpSession,
    /// RTP session for audio
    pub audio_session: Option<RtpSession>,
    /// Video packetizer
    pub packetizer: VideoPacketizer,
    /// Running flag
    pub running: Arc<RwLock<bool>>,
}

impl WebRtcMediaSender {
    /// Create a new WebRTC media sender
    pub fn new() -> Self {
        Self {
            video_session: RtpSession::new(96), // H.264
            audio_session: None,
            packetizer: VideoPacketizer::default(),
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Set remote destination for media
    pub async fn set_remote(&mut self, video_addr: SocketAddr, socket: Arc<UdpSocket>) {
        self.video_session.set_remote(video_addr, socket);
    }

    /// Send a video frame
    pub async fn send_video_frame(
        &mut self,
        frame_data: &[u8],
        timestamp_us: u64,
        is_keyframe: bool,
    ) -> Result<(), RtpError> {
        // Convert timestamp to RTP clock (90kHz for video)
        let timestamp = ((timestamp_us * 90) / 1_000_000) as u32;
        
        // Packetize frame
        let packets = self.packetizer.packetize(frame_data, timestamp, is_keyframe);
        
        // Send each packet
        for (i, packet) in packets.into_iter().enumerate() {
            let is_last = i == packets.len() - 1;
            self.video_session.send_packet(packet.payload, packet.timestamp, is_last || is_keyframe).await?;
        }
        
        Ok(())
    }

    /// Start sending RTCP reports periodically
    pub async fn start_rtcp(&self) {
        let session = self.video_session.clone();
        let running = self.running.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
            
            while *running.read().await {
                interval.tick().await;
                
                if let Err(e) = session.send_sender_report().await {
                    warn!("Failed to send RTCP report: {}", e);
                }
            }
        });
    }

    /// Stop media sender
    pub async fn stop(&self) {
        let mut running = self.running.write().await;
        *running = false;
    }
}

impl Default for WebRtcMediaSender {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for RtpSession {
    fn clone(&self) -> Self {
        Self {
            session_id: self.session_id.clone(),
            local_ssrc: self.local_ssrc,
            remote_ssrc: self.remote_ssrc,
            payload_type: self.payload_type,
            sequence_number: self.sequence_number,
            timestamp_base: self.timestamp_base,
            send_socket: self.send_socket.clone(),
            remote_addr: self.remote_addr,
            stats: self.stats.clone(),
            running: self.running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtp_packet_serialization() {
        let packet = RtpPacket::new(96, 1, 1000, 12345, vec![1, 2, 3, 4]);
        let data = packet.serialize();
        
        assert_eq!(data.len(), 16); // 12 byte header + 4 byte payload
        
        let deserialized = RtpPacket::deserialize(&data).unwrap();
        assert_eq!(deserialized.sequence_number, 1);
        assert_eq!(deserialized.timestamp, 1000);
        assert_eq!(deserialized.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_rtcp_sender_report() {
        let report = RtcpSenderReport {
            ssrc: 12345,
            ntp_secs: 3800000000,
            ntp_frac: 0,
            rtp_timestamp: 1000,
            packet_count: 100,
            octet_count: 50000,
        };
        
        let data = report.serialize();
        assert_eq!(data.len(), 28);
    }

    #[test]
    fn test_video_packetizer() {
        let packetizer = VideoPacketizer::new(96, 90000);
        
        // Create a frame larger than MTU
        let frame_data = vec![0xAB; 2000];
        let packets = packetizer.packetize(&frame_data, 1000, true);
        
        assert!(packets.len() > 1);
        assert!(packets[0].marker); // Keyframe marker
        assert!(!packets[0].payload.is_empty());
    }
}
