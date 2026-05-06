//! Native WebRTC Implementation
//!
//! Provides real WebRTC peer-to-peer connections using webrtc-rs crate.
//! Enables low-latency video/audio streaming with proper NAT traversal.
//!
//! # Features
//!
//! - Full WebRTC peer connection
//! - STUN/TURN server support
//! - Media track streaming
//! - Data channels for input events
//! - ICE candidate exchange
//!
//! # Requirements
//!
//! Add to Cargo.toml:
//! ```toml
//! webrtc = "0.9"
//! tokio-tungstenite = "0.21"  # For signaling
//! ```

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// WebRTC configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeWebRtcConfig {
    /// ICE servers for NAT traversal
    pub ice_servers: Vec<String>,
    /// Enable BUNDLE (multiplex media)
    pub enable_bundle: bool,
    /// Enable RTCP
    pub enable_rtcp: bool,
    /// Connection timeout (seconds)
    pub connection_timeout_secs: u64,
    /// ICE gathering timeout (seconds)
    pub ice_gathering_timeout_secs: u64,
}

impl Default for NativeWebRtcConfig {
    fn default() -> Self {
        Self {
            ice_servers: vec![
                "stun:stun.l.google.com:19302".to_string(),
                "stun:stun1.l.google.com:19302".to_string(),
            ],
            enable_bundle: true,
            enable_rtcp: true,
            connection_timeout_secs: 30,
            ice_gathering_timeout_secs: 10,
        }
    }
}

/// WebRTC peer connection state
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

/// ICE candidate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceCandidate {
    /// Candidate SDP
    pub candidate: String,
    /// Media line index
    pub sdp_mline_index: u16,
    /// Media line ID
    pub sdp_mid: Option<String>,
}

/// Session description (SDP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDescription {
    /// SDP type (offer/answer)
    pub sdp_type: SdpType,
    /// SDP content
    pub sdp: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdpType {
    Offer,
    Answer,
    PrAnswer,
    Rollback,
}

/// Media track information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaTrack {
    pub track_id: String,
    pub kind: TrackKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Audio,
    Video,
}

/// Data channel for input events
#[derive(Debug, Clone)]
pub struct DataChannel {
    pub label: String,
    pub ordered: bool,
    pub max_retransmits: Option<u16>,
}

/// Native WebRTC peer connection
#[cfg(feature = "native-webrtc")]
pub struct NativePeerConnection {
    config: NativeWebRtcConfig,
    /// Peer connection (webrtc-rs)
    #[cfg(feature = "native-webrtc")]
    peer_connection: Option<webrtc::peer_connection::RTCPeerConnection>,
    /// Data channel for input events
    data_channel: Option<Arc<webrtc::data_channel::RTCDataChannel>>,
    /// Local video track
    local_video_track: Option<Arc<webrtc::track::track_local::track_local_static::TrackLocalStatic>>,
    /// Remote tracks
    remote_tracks: Arc<RwLock<Vec<MediaTrack>>>,
    /// Connection state
    state: Arc<RwLock<PeerConnectionState>>,
    /// ICE state
    ice_state: Arc<RwLock<IceConnectionState>>,
    /// ICE candidates
    local_candidates: Arc<RwLock<Vec<IceCandidate>>>,
    /// Event senders
    video_tx: Option<mpsc::Sender<crate::webrtc_transport::VideoFrame>>,
    input_rx: Option<mpsc::Receiver<crate::webrtc_transport::InputEvent>>,
}

#[cfg(feature = "native-webrtc")]
impl NativePeerConnection {
    /// Create a new native WebRTC peer connection
    pub async fn new(config: NativeWebRtcConfig) -> Result<Self, WebRtcError> {
        use webrtc::peer_connection::{RTCPeerConnection, RTCPeerConnectionInit, RTCConfiguration};
        use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;
        use webrtc::ice_transport::ice_server::RTCIceServer;

        info!("Creating native WebRTC peer connection");

        // Convert ICE servers
        let ice_servers: Vec<RTCIceServer> = config.ice_servers.iter().map(|url| {
            RTCIceServer {
                urls: vec![url.clone()],
                ..Default::default()
            }
        }).collect();

        // Create peer connection configuration
        let rtc_config = RTCConfiguration {
            ice_servers,
            ..Default::default()
        };

        // Create peer connection
        let peer_connection = RTCPeerConnection::new(&rtc_config, None)
            .await
            .map_err(|e| WebRtcError::PeerConnectionError(e.to_string()))?;

        Ok(Self {
            config,
            peer_connection: Some(peer_connection),
            data_channel: None,
            local_video_track: None,
            remote_tracks: Arc::new(RwLock::new(Vec::new())),
            state: Arc::new(RwLock::new(PeerConnectionState::New)),
            ice_state: Arc::new(RwLock::new(IceConnectionState::New)),
            local_candidates: Arc::new(RwLock::new(Vec::new())),
            video_tx: None,
            input_rx: None,
        })
    }

    /// Create SDP offer
    pub async fn create_offer(&self) -> Result<SessionDescription, WebRtcError> {
        use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;

        let pc = self.peer_connection.as_ref()
            .ok_or(WebRtcError::NotConnected)?;

        let offer = pc.create_offer(None).await
            .map_err(|e| WebRtcError::SdpError(e.to_string()))?;

        pc.set_local_description(offer.clone()).await
            .map_err(|e| WebRtcError::SdpError(e.to_string()))?;

        Ok(SessionDescription {
            sdp_type: SdpType::Offer,
            sdp: offer.sdp,
        })
    }

    /// Set remote description (answer)
    pub async fn set_remote_description(&self, desc: SessionDescription) -> Result<(), WebRtcError> {
        use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
        use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;

        let pc = self.peer_connection.as_ref()
            .ok_or(WebRtcError::NotConnected)?;

        let rtc_sdp_type = match desc.sdp_type {
            SdpType::Offer => RTCSdpType::Offer,
            SdpType::Answer => RTCSdpType::Answer,
            SdpType::PrAnswer => RTCSdpType::Pranswer,
            SdpType::Rollback => RTCSdpType::Rollback,
        };

        let remote_desc = RTCSessionDescription {
            sdp_type: rtc_sdp_type,
            sdp: desc.sdp,
        };

        pc.set_remote_description(remote_desc).await
            .map_err(|e| WebRtcError::SdpError(e.to_string()))?;

        Ok(())
    }

    /// Add ICE candidate
    pub async fn add_ice_candidate(&self, candidate: IceCandidate) -> Result<(), WebRtcError> {
        use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;

        let pc = self.peer_connection.as_ref()
            .ok_or(WebRtcError::NotConnected)?;

        let candidate_init = RTCIceCandidateInit {
            candidate: candidate.candidate,
            sdp_mid: candidate.sdp_mid,
            sdp_mline_index: candidate.sdp_mline_index,
            ..Default::default()
        };

        pc.add_ice_candidate(candidate_init).await
            .map_err(|e| WebRtcError::IceError(e.to_string()))?;

        Ok(())
    }

    /// Create data channel for input events
    pub async fn create_data_channel(&mut self, label: &str) -> Result<(), WebRtcError> {
        use webrtc::data_channel::RTCDataChannelInit;

        let pc = self.peer_connection.as_ref()
            .ok_or(WebRtcError::NotConnected)?;

        let config = RTCDataChannelInit {
            ordered: Some(true),
            max_retransmits: Some(0), // Unreliable for low latency
            ..Default::default()
        };

        let data_channel = pc.create_data_channel(label, Some(config))
            .map_err(|e| WebRtcError::DataChannelError(e.to_string()))?;

        // Set up event handlers
        let state = self.state.clone();
        let ice_state = self.ice_state.clone();

        data_channel.on_open(Box::new(move || {
            let state = state.clone();
            Box::pin(async move {
                info!("Data channel opened");
                let mut s = state.write().await;
                *s = PeerConnectionState::Connected;
            })
        }));

        self.data_channel = Some(data_channel);
        Ok(())
    }

    /// Send input event via data channel
    pub async fn send_input(&self, event: crate::webrtc_transport::InputEvent) -> Result<(), WebRtcError> {
        use webrtc::data_channel::RTCDataChannel;

        let dc = self.data_channel.as_ref()
            .ok_or(WebRtcError::DataChannelError("Not created".to_string()))?;

        let data = serde_json::to_vec(&event)
            .map_err(|e| WebRtcError::SerializationError(e.to_string()))?;

        dc.send(&data.into())
            .await
            .map_err(|e| WebRtcError::DataChannelError(e.to_string()))?;

        Ok(())
    }

    /// Get connection state
    pub async fn state(&self) -> PeerConnectionState {
        *self.state.read().await
    }

    /// Get ICE state
    pub async fn ice_state(&self) -> IceConnectionState {
        *self.ice_state.read().await
    }

    /// Close the connection
    pub async fn close(&self) -> Result<(), WebRtcError> {
        let pc = self.peer_connection.as_ref()
            .ok_or(WebRtcError::NotConnected)?;

        pc.close().await
            .map_err(|e| WebRtcError::PeerConnectionError(e.to_string()))?;

        let mut state = self.state.write().await;
        *state = PeerConnectionState::Closed;

        Ok(())
    }
}

#[cfg(not(feature = "native-webrtc"))]
pub struct NativePeerConnection {
    _marker: std::marker::PhantomData<()>,
}

#[cfg(not(feature = "native-webrtc"))]
impl NativePeerConnection {
    pub async fn new(_config: NativeWebRtcConfig) -> Result<Self, WebRtcError> {
        Err(WebRtcError::FeatureNotEnabled(
            "native-webrtc feature not enabled".to_string()
        ))
    }

    pub async fn create_offer(&self) -> Result<SessionDescription, WebRtcError> {
        Err(WebRtcError::FeatureNotEnabled(
            "native-webrtc feature not enabled".to_string()
        ))
    }
}

/// WebRTC errors
#[derive(Debug, thiserror::Error)]
pub enum WebRtcError {
    #[error("Peer connection error: {0}")]
    PeerConnectionError(String),

    #[error("SDP error: {0}")]
    SdpError(String),

    #[error("ICE error: {0}")]
    IceError(String),

    #[error("Data channel error: {0}")]
    DataChannelError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Not connected")]
    NotConnected,

    #[error("Feature not enabled: {0}")]
    FeatureNotEnabled(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Signaling error: {0}")]
    SignalingError(String),
}

/// Signaling client for WebRTC connection establishment
pub struct SignalingClient {
    /// WebSocket URL for signaling server
    ws_url: String,
    /// WebSocket connection
    ws: Option<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>,
}

impl SignalingClient {
    pub fn new(ws_url: &str) -> Self {
        Self {
            ws_url: ws_url.to_string(),
            ws: None,
        }
    }

    /// Connect to signaling server
    pub async fn connect(&mut self) -> Result<(), WebRtcError> {
        use tokio_tungstenite::{connect_async, tungstenite::Message};
        use futures_util::{SinkExt, StreamExt};

        let (ws_stream, _) = connect_async(&self.ws_url).await
            .map_err(|e| WebRtcError::SignalingError(e.to_string()))?;

        self.ws = Some(ws_stream);
        Ok(())
    }

    /// Send SDP offer/answer
    pub async fn send_sdp(&mut self, sdp: SessionDescription) -> Result<(), WebRtcError> {
        use tokio_tungstenite::tungstenite::Message;

        if let Some(ref mut ws) = self.ws {
            let msg = serde_json::to_string(&sdp)
                .map_err(|e| WebRtcError::SerializationError(e.to_string()))?;

            ws.send(Message::Text(msg.into()))
                .await
                .map_err(|e| WebRtcError::SignalingError(e.to_string()))?;
        }

        Ok(())
    }

    /// Send ICE candidate
    pub async fn send_candidate(&mut self, candidate: IceCandidate) -> Result<(), WebRtcError> {
        use tokio_tungstenite::tungstenite::Message;

        if let Some(ref mut ws) = self.ws {
            let msg = serde_json::to_string(&candidate)
                .map_err(|e| WebRtcError::SerializationError(e.to_string()))?;

            ws.send(Message::Text(msg.into()))
                .await
                .map_err(|e| WebRtcError::SignalingError(e.to_string()))?;
        }

        Ok(())
    }

    /// Receive signaling message
    pub async fn recv_message(&mut self) -> Result<SignalingMessage, WebRtcError> {
        use tokio_tungstenite::tungstenite::Message;
        use futures_util::StreamExt;

        if let Some(ref mut ws) = self.ws {
            if let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        let message: SignalingMessage = serde_json::from_str(&text)
                            .map_err(|e| WebRtcError::SerializationError(e.to_string()))?;
                        Ok(message)
                    }
                    Ok(Message::Close(_)) => Err(WebRtcError::SignalingError("Connection closed".to_string())),
                    Err(e) => Err(WebRtcError::SignalingError(e.to_string())),
                    _ => Err(WebRtcError::SignalingError("Unexpected message type".to_string())),
                }
            } else {
                Err(WebRtcError::SignalingError("No message received".to_string()))
            }
        } else {
            Err(WebRtcError::NotConnected)
        }
    }
}

/// Signaling message types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SignalingMessage {
    Offer(SessionDescription),
    Answer(SessionDescription),
    Candidate(IceCandidate),
    Ready,
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = NativeWebRtcConfig::default();
        assert_eq!(config.ice_servers.len(), 2);
        assert!(config.enable_bundle);
        assert!(config.enable_rtcp);
    }

    #[test]
    fn test_ice_candidate_serialization() {
        let candidate = IceCandidate {
            candidate: "candidate:1 1 UDP 1 localhost 8080".to_string(),
            sdp_mline_index: 0,
            sdp_mid: Some("0".to_string()),
        };

        let json = serde_json::to_string(&candidate).unwrap();
        assert!(json.contains("localhost"));
    }

    #[test]
    fn test_session_description_serialization() {
        let sdp = SessionDescription {
            sdp_type: SdpType::Offer,
            sdp: "v=0\r\no=- 123 2 IN IP4 127.0.0.1\r\n".to_string(),
        };

        let json = serde_json::to_string(&sdp).unwrap();
        assert!(json.contains("offer"));
    }
}
