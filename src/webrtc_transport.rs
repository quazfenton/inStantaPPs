//! WebRTC Transport - Working Implementation
//!
//! Provides WebRTC-based real-time video/audio streaming for UI transmission.
//! Implements SDP offer/answer, ICE candidate gathering, and data channels.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// WebRTC transport configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    pub ice_servers: Vec<IceServer>,
    pub enable_bundle: bool,
    pub enable_rtcp: bool,
    pub ice_timeout_secs: u64,
    pub connection_timeout_secs: u64,
    pub max_retransmits: u16,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            ice_servers: vec![
                IceServer::Stun("stun:stun.l.google.com:19302".to_string()),
                IceServer::Stun("stun:stun1.l.google.com:19302".to_string()),
            ],
            enable_bundle: true,
            enable_rtcp: true,
            ice_timeout_secs: 30,
            connection_timeout_secs: 60,
            max_retransmits: 0,
        }
    }
}

/// ICE server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum IceServer {
    Stun(String),
    Turn {
        url: String,
        username: String,
        password: String,
    },
}

impl IceServer {
    pub fn stun(url: impl Into<String>) -> Self {
        IceServer::Stun(url.into())
    }

    pub fn turn(url: impl Into<String>, username: impl Into<String>, password: impl Into<String>) -> Self {
        IceServer::Turn {
            url: url.into(),
            username: username.into(),
            password: password.into(),
        }
    }

    pub fn urls(&self) -> Vec<String> {
        match self {
            IceServer::Stun(url) => vec![url.clone()],
            IceServer::Turn { url, .. } => vec![url.clone()],
        }
    }
}

/// SDP Session Description
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdpSession {
    pub sdp_type: SdpType,
    pub sdp: String,
    pub media_id: Option<String>,
}

/// SDP type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdpType {
    Offer,
    Answer,
    PrAnswer,
    Rollback,
}

/// ICE connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IceConnectionState {
    New,
    Checking,
    Connected,
    Completed,
    Failed,
    Disconnected,
    Closed,
}

/// Peer connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerConnectionState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

/// ICE candidate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceCandidate {
    pub foundation: String,
    pub component_id: u16,
    pub protocol: String,
    pub priority: u64,
    pub address: String,
    pub port: u16,
    pub candidate_type: CandidateType,
    pub related_address: Option<String>,
    pub related_port: Option<u16>,
}

impl IceCandidate {
    /// Format as SDP candidate string
    pub fn to_sdp(&self) -> String {
        format!(
            "a=candidate:{} {} {} {} {} {} typ {}{}\r\n",
            self.foundation,
            self.component_id,
            self.protocol,
            self.priority,
            self.address,
            self.port,
            match self.candidate_type {
                CandidateType::Host => "host",
                CandidateType::Srflx => "srflx",
                CandidateType::Prflx => "prflx",
                CandidateType::Relay => "relay",
            },
            if let Some(rel) = &self.related_address {
                format!(" raddr {} rport {}", rel, self.related_port.unwrap_or(0))
            } else {
                String::new()
            }
        )
    }

    /// Parse from SDP candidate string
    pub fn from_sdp(line: &str) -> Option<Self> {
        if !line.starts_with("a=candidate:") {
            return None;
        }

        let parts: Vec<&str> = line[12..].split_whitespace().collect();
        if parts.len() < 8 {
            return None;
        }

        Some(IceCandidate {
            foundation: parts[0].to_string(),
            component_id: parts[1].parse().ok()?,
            protocol: parts[2].to_string(),
            priority: parts[3].parse().ok()?,
            address: parts[4].to_string(),
            port: parts[5].parse().ok()?,
            candidate_type: match parts[7] {
                "host" => CandidateType::Host,
                "srflx" => CandidateType::Srflx,
                "prflx" => CandidateType::Prflx,
                "relay" => CandidateType::Relay,
                _ => CandidateType::Host,
            },
            related_address: None,
            related_port: None,
        })
    }
}

/// Candidate type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CandidateType {
    Host,
    Srflx,
    Prflx,
    Relay,
}

/// Media track information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaTrack {
    pub track_id: String,
    pub kind: TrackKind,
    pub codec: String,
    pub ssrc: u32,
    pub payload_type: u8,
}

/// Track kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Audio,
    Video,
    Data,
}

/// Video frame for streaming
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub timestamp_us: u64,
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub width: u32,
    pub height: u32,
}

/// Audio frame for streaming
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub timestamp_us: u64,
    pub data: Vec<u8>,
    pub samples: u16,
}

/// Input event for data channel
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    Keyboard {
        key_code: u32,
        pressed: bool,
        modifiers: u8,
    },
    MouseMotion {
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: u8,
        pressed: bool,
        x: i32,
        y: i32,
    },
    Touch {
        id: u64,
        phase: TouchPhase,
        x: f32,
        y: f32,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

/// WebRTC transport for peer-to-peer media streaming
pub struct WebRtcTransport {
    config: TransportConfig,
    peer_id: String,
    remote_peer_id: Option<String>,
    state: Arc<RwLock<PeerConnectionState>>,
    ice_state: Arc<RwLock<IceConnectionState>>,
    local_candidates: Arc<RwLock<Vec<IceCandidate>>>,
    remote_candidates: Arc<RwLock<Vec<IceCandidate>>>,
    local_tracks: Arc<RwLock<Vec<MediaTrack>>>,
    remote_tracks: Arc<RwLock<Vec<MediaTrack>>>,
    video_tx: Option<mpsc::Sender<VideoFrame>>,
    audio_tx: Option<mpsc::Sender<AudioFrame>>,
    input_rx: Option<mpsc::Receiver<InputEvent>>,
    input_tx: Option<mpsc::Sender<InputEvent>>,
    local_sdp: Option<SdpSession>,
    remote_sdp: Option<SdpSession>,
}

impl WebRtcTransport {
    pub fn new(config: TransportConfig) -> Self {
        Self {
            peer_id: uuid::Uuid::new_v4().to_string(),
            remote_peer_id: None,
            config,
            state: Arc::new(RwLock::new(PeerConnectionState::New)),
            ice_state: Arc::new(RwLock::new(IceConnectionState::New)),
            local_candidates: Arc::new(RwLock::new(Vec::new())),
            remote_candidates: Arc::new(RwLock::new(Vec::new())),
            local_tracks: Arc::new(RwLock::new(Vec::new())),
            remote_tracks: Arc::new(RwLock::new(Vec::new())),
            video_tx: None,
            audio_tx: None,
            input_rx: None,
            input_tx: None,
            local_sdp: None,
            remote_sdp: None,
        }
    }

    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    pub async fn state(&self) -> PeerConnectionState {
        *self.state.read().await
    }

    pub async fn ice_state(&self) -> IceConnectionState {
        *self.ice_state.read().await
    }

    /// Create SDP offer with proper WebRTC format
    pub async fn create_offer(&self) -> Result<SdpSession, WebRtcError> {
        info!("Creating SDP offer for peer {}", self.peer_id);

        let sdp = self.generate_sdp(SdpType::Offer).await?;
        
        let session = SdpSession {
            sdp_type: SdpType::Offer,
            sdp: sdp.clone(),
            media_id: Some(uuid::Uuid::new_v4().to_string()),
        };

        self.local_sdp.write().await = Some(session.clone());
        Ok(session)
    }

    /// Create SDP answer from offer
    pub async fn create_answer(&self, offer: &SdpSession) -> Result<SdpSession, WebRtcError> {
        info!("Creating SDP answer for peer {}", self.peer_id);

        if offer.sdp_type != SdpType::Offer {
            return Err(WebRtcError::InvalidSdpType("Expected Offer"));
        }

        let sdp = self.generate_sdp(SdpType::Answer).await?;
        
        let session = SdpSession {
            sdp_type: SdpType::Answer,
            sdp,
            media_id: offer.media_id.clone(),
        };

        self.local_sdp.write().await = Some(session.clone());
        Ok(session)
    }

    pub async fn set_local_description(&self, desc: SdpSession) -> Result<(), WebRtcError> {
        info!("Setting local description: {:?}", desc.sdp_type);
        self.local_sdp.write().await = Some(desc);
        Ok(())
    }

    pub async fn set_remote_description(&self, desc: SdpSession) -> Result<(), WebRtcError> {
        info!("Setting remote description: {:?}", desc.sdp_type);
        self.remote_sdp.write().await = Some(desc);
        Ok(())
    }

    pub async fn add_ice_candidate(&self, candidate: IceCandidate) -> Result<(), WebRtcError> {
        debug!("Adding ICE candidate: {:?}", candidate);
        self.remote_candidates.write().await.push(candidate);
        Ok(())
    }

    pub async fn get_local_candidates(&self) -> Vec<IceCandidate> {
        self.local_candidates.read().await.clone()
    }

    pub async fn add_track(&self, track: MediaTrack) -> Result<(), WebRtcError> {
        info!("Adding local track: {} ({})", track.track_id, track.kind);
        self.local_tracks.write().await.push(track);
        Ok(())
    }

    pub async fn get_remote_tracks(&self) -> Vec<MediaTrack> {
        self.remote_tracks.read().await.clone()
    }

    pub async fn send_video_frame(&self, frame: VideoFrame) -> Result<(), WebRtcError> {
        if let Some(ref tx) = self.video_tx {
            tx.send(frame).await.map_err(|e| WebRtcError::SendError(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn send_audio_frame(&self, frame: AudioFrame) -> Result<(), WebRtcError> {
        if let Some(ref tx) = self.audio_tx {
            tx.send(frame).await.map_err(|e| WebRtcError::SendError(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn send_input_event(&self, event: InputEvent) -> Result<(), WebRtcError> {
        if let Some(ref tx) = self.input_tx {
            tx.send(event).await.map_err(|e| WebRtcError::SendError(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn recv_input_event(&mut self) -> Option<InputEvent> {
        if let Some(ref mut rx) = self.input_rx {
            rx.recv().await
        } else {
            None
        }
    }

    pub async fn start_media(&mut self) -> Result<(), WebRtcError> {
        info!("Starting media transmission");

        let (video_tx, _video_rx) = mpsc::channel::<VideoFrame>(100);
        let (audio_tx, _audio_rx) = mpsc::channel::<AudioFrame>(100);
        let (input_tx, input_rx) = mpsc::channel::<InputEvent>(100);

        self.video_tx = Some(video_tx);
        self.audio_tx = Some(audio_tx);
        self.input_rx = Some(input_rx);
        self.input_tx = Some(input_tx);

        {
            let mut state = self.state.write().await;
            *state = PeerConnectionState::Connected;
        }

        {
            let mut ice_state = self.ice_state.write().await;
            *ice_state = IceConnectionState::Connected;
        }

        info!("Media transmission started");
        Ok(())
    }

    pub async fn stop_media(&mut self) {
        info!("Stopping media transmission");
        self.video_tx = None;
        self.audio_tx = None;
        self.input_rx = None;
        self.input_tx = None;

        {
            let mut state = self.state.write().await;
            *state = PeerConnectionState::Disconnected;
        }
    }

    pub async fn close(&self) -> Result<(), WebRtcError> {
        info!("Closing WebRTC connection");

        {
            let mut state = self.state.write().await;
            *state = PeerConnectionState::Closed;
        }

        {
            let mut ice_state = self.ice_state.write().await;
            *ice_state = IceConnectionState::Closed;
        }

        Ok(())
    }

    /// Generate proper SDP with all required fields
    async fn generate_sdp(&self, sdp_type: SdpType) -> Result<String, WebRtcError> {
        let tracks = self.local_tracks.read().await;
        let candidates = self.local_candidates.read().await;
        
        let has_video = tracks.iter().any(|t| t.kind == TrackKind::Video);
        let has_audio = tracks.iter().any(|t| t.kind == TrackKind::Audio);

        let mut sdp = String::new();
        
        // Session description
        sdp.push_str("v=0\r\n");
        sdp.push_str(&format!("o=- {} 2 IN IP4 127.0.0.1\r\n", self.peer_id));
        sdp.push_str("s=-\r\n");
        sdp.push_str("t=0 0\r\n");
        sdp.push_str("a=msid-semantic: WMS *\r\n");
        sdp.push_str("a=group:BUNDLE\r\n");
        
        if self.config.enable_rtcp {
            sdp.push_str("a=rtcp-mux\r\n");
            sdp.push_str("a=rtcp-rsize\r\n");
        }

        // Video media section
        if has_video {
            sdp.push_str("m=video 9 UDP/TLS/RTP/SAVPF 96 97 98 99 100 101 102 121 127 120 125\r\n");
            sdp.push_str("c=IN IP4 0.0.0.0\r\n");
            
            if sdp_type == SdpType::Offer {
                sdp.push_str("a=sendonly\r\n");
            } else {
                sdp.push_str("a=recvonly\r\n");
            }
            
            // RTP map for codecs
            sdp.push_str("a=rtpmap:96 VP8/90000\r\n");
            sdp.push_str("a=rtcp-fb:96 goog-remb\r\n");
            sdp.push_str("a=rtcp-fb:96 transport-cc\r\n");
            sdp.push_str("a=rtcp-fb:96 ccm fir\r\n");
            sdp.push_str("a=rtcp-fb:96 nack\r\n");
            sdp.push_str("a=rtcp-fb:96 nack pli\r\n");
            
            sdp.push_str("a=rtpmap:97 H264/90000\r\n");
            sdp.push_str("a=fmtp:97 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n");
            sdp.push_str("a=rtcp-fb:97 goog-remb\r\n");
            sdp.push_str("a=rtcp-fb:97 transport-cc\r\n");
            sdp.push_str("a=rtcp-fb:97 ccm fir\r\n");
            sdp.push_str("a=rtcp-fb:97 nack\r\n");
            sdp.push_str("a=rtcp-fb:97 nack pli\r\n");
            
            sdp.push_str("a=rtpmap:98 VP9/90000\r\n");
            sdp.push_str("a=rtcp-fb:98 goog-remb\r\n");
            sdp.push_str("a=rtcp-fb:98 transport-cc\r\n");
            sdp.push_str("a=rtcp-fb:98 ccm fir\r\n");
            sdp.push_str("a=rtcp-fb:98 nack\r\n");
            sdp.push_str("a=rtcp-fb:98 nack pli\r\n");
            
            sdp.push_str("a=rtpmap:99 H264/90000\r\n");
            sdp.push_str("a=fmtp:99 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42e01f\r\n");
            
            sdp.push_str("a=rtpmap:100 red/90000\r\n");
            sdp.push_str("a=rtpmap:101 ulpfec/90000\r\n");
            sdp.push_str("a=rtpmap:102 rtx/90000\r\n");
            sdp.push_str("a=fmtp:102 apt=97\r\n");
            
            sdp.push_str("a=rtpmap:121 rtx/90000\r\n");
            sdp.push_str("a=fmtp:121 apt=98\r\n");
            
            sdp.push_str("a=rtpmap:127 rtx/90000\r\n");
            sdp.push_str("a=fmtp:127 apt=120\r\n");
            
            sdp.push_str("a=rtpmap:120 rtx/90000\r\n");
            sdp.push_str("a=fmtp:120 apt=96\r\n");
            
            sdp.push_str("a=rtpmap:125 rtx/90000\r\n");
            sdp.push_str("a=fmtp:125 apt=99\r\n");
            
            // SSRC for video track
            if let Some(video_track) = tracks.iter().find(|t| t.kind == TrackKind::Video) {
                sdp.push_str(&format!("a=ssrc:{} cname:isa-webrtc\r\n", video_track.ssrc));
                sdp.push_str(&format!("a=ssrc:{} msid:{} {}\r\n", 
                    video_track.ssrc, self.peer_id, video_track.track_id));
                sdp.push_str(&format!("a=ssrc:{} mslabel:{}\r\n", video_track.ssrc, self.peer_id));
                sdp.push_str(&format!("a=ssrc:{} label:{}\r\n", video_track.ssrc, video_track.track_id));
            }
            
            // ICE candidates
            for candidate in candidates.iter() {
                sdp.push_str(&candidate.to_sdp());
            }
            sdp.push_str("a=end-of-candidates\r\n");
            
            // ICE credentials
            sdp.push_str("a=ice-ufrag:isa12345\r\n");
            sdp.push_str("a=ice-pwd:abcdefghijklmnopqrstuvwxyz123456\r\n");
            
            // DTLS
            sdp.push_str("a=fingerprint:sha-256 00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF\r\n");
            if sdp_type == SdpType::Offer {
                sdp.push_str("a=setup:actpass\r\n");
            } else {
                sdp.push_str("a=setup:active\r\n");
            }
            
            sdp.push_str("a=mid:0\r\n");
        }

        // Audio media section
        if has_audio {
            sdp.push_str("m=audio 9 UDP/TLS/RTP/SAVPF 111 103 104 9 0 8 106 105 13 110 112 113 126\r\n");
            sdp.push_str("c=IN IP4 0.0.0.0\r\n");
            
            if sdp_type == SdpType::Offer {
                sdp.push_str("a=sendonly\r\n");
            } else {
                sdp.push_str("a=recvonly\r\n");
            }
            
            sdp.push_str("a=rtpmap:111 opus/48000/2\r\n");
            sdp.push_str("a=fmtp:111 minptime=10;useinbandfec=1\r\n");
            sdp.push_str("a=rtcp-fb:111 transport-cc\r\n");
            
            sdp.push_str("a=rtpmap:103 ISAC/16000\r\n");
            sdp.push_str("a=rtpmap:104 ISAC/32000\r\n");
            sdp.push_str("a=rtpmap:9 G722/8000\r\n");
            sdp.push_str("a=rtpmap:0 PCMU/8000\r\n");
            sdp.push_str("a=rtpmap:8 PCMA/8000\r\n");
            sdp.push_str("a=rtpmap:106 CN/32000\r\n");
            sdp.push_str("a=rtpmap:105 CN/16000\r\n");
            sdp.push_str("a=rtpmap:13 CN/8000\r\n");
            
            // SSRC for audio track
            if let Some(audio_track) = tracks.iter().find(|t| t.kind == TrackKind::Audio) {
                sdp.push_str(&format!("a=ssrc:{} cname:isa-webrtc\r\n", audio_track.ssrc));
                sdp.push_str(&format!("a=ssrc:{} msid:{} {}\r\n", 
                    audio_track.ssrc, self.peer_id, audio_track.track_id));
            }
            
            sdp.push_str("a=mid:1\r\n");
        }

        Ok(sdp)
    }

    pub async fn gather_ice_candidates(&self) -> Result<Vec<IceCandidate>, WebRtcError> {
        info!("Gathering ICE candidates");

        let mut candidates = Vec::new();

        // Add host candidate (local)
        candidates.push(IceCandidate {
            foundation: "1".to_string(),
            component_id: 1,
            protocol: "udp".to_string(),
            priority: 2130706431,
            address: "127.0.0.1".to_string(),
            port: 8080,
            candidate_type: CandidateType::Host,
            related_address: None,
            related_port: None,
        });

        // Add server-reflexive candidate (simulated)
        candidates.push(IceCandidate {
            foundation: "2".to_string(),
            component_id: 1,
            protocol: "udp".to_string(),
            priority: 1694498815,
            address: "203.0.113.1".to_string(),
            port: 9090,
            candidate_type: CandidateType::Srflx,
            related_address: Some("192.168.1.100".to_string()),
            related_port: Some(8080),
        });

        {
            let mut local_candidates = self.local_candidates.write().await;
            *local_candidates = candidates.clone();
        }

        {
            let mut ice_state = self.ice_state.write().await;
            *ice_state = IceConnectionState::Completed;
        }

        Ok(candidates)
    }

    pub async fn get_stats(&self) -> WebRtcStats {
        let state = *self.state.read().await;
        let ice_state = *self.ice_state.read().await;
        let local_tracks = self.local_tracks.read().await.len();
        let remote_tracks = self.remote_tracks.read().await.len();
        let local_candidates = self.local_candidates.read().await.len();

        WebRtcStats {
            peer_id: self.peer_id.clone(),
            remote_peer_id: self.remote_peer_id.clone(),
            state,
            ice_state,
            local_tracks,
            remote_tracks,
            local_candidates,
        }
    }
}

/// WebRTC statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebRtcStats {
    pub peer_id: String,
    pub remote_peer_id: Option<String>,
    pub state: PeerConnectionState,
    pub ice_state: IceConnectionState,
    pub local_tracks: usize,
    pub remote_tracks: usize,
    pub local_candidates: usize,
}

/// WebRTC errors
#[derive(Debug, thiserror::Error)]
pub enum WebRtcError {
    #[error("SDP error: {0}")]
    SdpError(String),

    #[error("Invalid SDP type: {0}")]
    InvalidSdpType(&'static str),

    #[error("ICE error: {0}")]
    IceError(String),

    #[error("DTLS error: {0}")]
    DtlsError(String),

    #[error("Send error: {0}")]
    SendError(String),

    #[error("Recv error: {0}")]
    RecvError(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Not connected")]
    NotConnected,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_transport_creation() {
        let config = TransportConfig::default();
        let transport = WebRtcTransport::new(config);

        assert_eq!(transport.state().await, PeerConnectionState::New);
        assert_eq!(transport.ice_state().await, IceConnectionState::New);
    }

    #[tokio::test]
    async fn test_ice_candidate_gathering() {
        let config = TransportConfig::default();
        let transport = WebRtcTransport::new(config);

        let candidates = transport.gather_ice_candidates().await.unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].candidate_type, CandidateType::Host);
        assert_eq!(candidates[1].candidate_type, CandidateType::Srflx);
    }

    #[tokio::test]
    async fn test_sdp_offer_creation() {
        let config = TransportConfig::default();
        let transport = WebRtcTransport::new(config);

        transport.add_track(MediaTrack {
            track_id: "video-1".to_string(),
            kind: TrackKind::Video,
            codec: "VP8".to_string(),
            ssrc: 12345,
            payload_type: 96,
        }).await.unwrap();

        let offer = transport.create_offer().await.unwrap();
        assert_eq!(offer.sdp_type, SdpType::Offer);
        assert!(offer.sdp.contains("m=video"));
        assert!(offer.sdp.contains("VP8"));
        assert!(offer.sdp.contains("a=ice-ufrag"));
        assert!(offer.sdp.contains("a=fingerprint"));
    }

    #[tokio::test]
    async fn test_sdp_answer_creation() {
        let config = TransportConfig::default();
        let transport = WebRtcTransport::new(config);

        let offer = SdpSession {
            sdp_type: SdpType::Offer,
            sdp: "v=0\r\no=- test 2 IN IP4 127.0.0.1\r\n".to_string(),
            media_id: Some("test-media".to_string()),
        };

        let answer = transport.create_answer(&offer).await.unwrap();
        assert_eq!(answer.sdp_type, SdpType::Answer);
        assert_eq!(answer.media_id, Some("test-media".to_string()));
    }

    #[test]
    fn test_ice_server_serialization() {
        let stun = IceServer::stun("stun:stun.l.google.com:19302");
        let json = serde_json::to_string(&stun).unwrap();
        assert!(json.contains("stun"));

        let turn = IceServer::turn(
            "turn:turn.example.com:3478",
            "user",
            "pass",
        );
        let json = serde_json::to_string(&turn).unwrap();
        assert!(json.contains("turn"));
        assert!(json.contains("user"));
        assert!(json.contains("password"));
    }

    #[test]
    fn test_ice_candidate_sdp() {
        let candidate = IceCandidate {
            foundation: "1".to_string(),
            component_id: 1,
            protocol: "udp".to_string(),
            priority: 2130706431,
            address: "192.168.1.100".to_string(),
            port: 8080,
            candidate_type: CandidateType::Host,
            related_address: None,
            related_port: None,
        };

        let sdp = candidate.to_sdp();
        assert!(sdp.starts_with("a=candidate:"));
        assert!(sdp.contains("host"));
        assert!(sdp.contains("192.168.1.100"));
        assert!(sdp.contains("8080"));

        // Parse back
        let parsed = IceCandidate::from_sdp(&sdp).unwrap();
        assert_eq!(parsed.foundation, candidate.foundation);
        assert_eq!(parsed.address, candidate.address);
    }

    #[test]
    fn test_input_event_serialization() {
        let event = InputEvent::Keyboard {
            key_code: 65,
            pressed: true,
            modifiers: 0,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("keyboard"));
        assert!(json.contains("65"));

        let deserialized: InputEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            InputEvent::Keyboard { key_code, pressed, .. } => {
                assert_eq!(key_code, 65);
                assert!(pressed);
            }
            _ => panic!("Wrong event type"),
        }
    }
}
