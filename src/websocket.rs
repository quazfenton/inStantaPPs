//! WebSocket Module for Real-Time State Updates
//!
//! Provides WebSocket endpoints for:
//! - Live state streaming during snapshot/resume
//! - Progress notifications
//! - Collaborative session events
//!
//! # Endpoints
//!
//! - `WS /ws/state/{state_id}` - Stream state transfer progress
//! - `WS /ws/collab/{session_id}` - Collaborative session events

use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Path, State},
    response::IntoResponse,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn, error};

use crate::api::AppState;

/// WebSocket event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    /// Snapshot progress update
    SnapshotProgress {
        state_id: String,
        phase: String,
        percent: f32,
        bytes_transferred: u64,
        elapsed_ms: u64,
    },
    /// Resume progress update
    ResumeProgress {
        state_id: String,
        phase: String,
        percent: f32,
        pages_loaded: u64,
        elapsed_ms: u64,
    },
    /// Collaborative session event
    CollaborativeEvent {
        session_id: String,
        user_id: String,
        action: String,
        payload: serde_json::Value,
    },
    /// Error notification
    Error {
        message: String,
        code: u16,
    },
    /// Heartbeat
    Heartbeat {
        timestamp: u64,
    },
}

/// WebSocket connection manager
pub struct WebSocketManager {
    /// Broadcast channel for state updates
    state_updates: broadcast::Sender<WsEvent>,
    /// Active connections by state_id
    connections: Arc<RwLock<std::collections::HashMap<String, Vec<broadcast::Receiver<WsEvent>>>>>,
}

impl WebSocketManager {
    /// Create a new WebSocket manager
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1000);
        Self {
            state_updates: tx,
            connections: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Subscribe to state updates for a specific state_id
    pub async fn subscribe(&self, state_id: &str) -> broadcast::Receiver<WsEvent> {
        let mut connections = self.connections.write().await;
        let (tx, rx) = broadcast::channel(100);
        
        connections.entry(state_id.to_string()).or_insert_with(Vec::new).push(rx.resubscribe());
        rx
    }

    /// Broadcast event to all subscribers of a state
    pub async fn broadcast(&self, state_id: &str, event: WsEvent) {
        let connections = self.connections.read().await;
        
        if let Some(receivers) = connections.get(state_id) {
            for tx in receivers {
                let _ = tx.send(event.clone());
            }
        }

        // Also send to global channel
        let _ = self.state_updates.send(event);
    }

    /// Send snapshot progress update
    pub async fn snapshot_progress(
        &self,
        state_id: &str,
        phase: &str,
        percent: f32,
        bytes_transferred: u64,
        elapsed_ms: u64,
    ) {
        let event = WsEvent::SnapshotProgress {
            state_id: state_id.to_string(),
            phase: phase.to_string(),
            percent,
            bytes_transferred,
            elapsed_ms,
        };
        self.broadcast(state_id, event).await;
    }

    /// Send resume progress update
    pub async fn resume_progress(
        &self,
        state_id: &str,
        phase: &str,
        percent: f32,
        pages_loaded: u64,
        elapsed_ms: u64,
    ) {
        let event = WsEvent::ResumeProgress {
            state_id: state_id.to_string(),
            phase: phase.to_string(),
            percent,
            pages_loaded,
            elapsed_ms,
        };
        self.broadcast(state_id, event).await;
    }

    /// Send error notification
    pub async fn send_error(&self, state_id: &str, message: String, code: u16) {
        let event = WsEvent::Error { message, code };
        self.broadcast(state_id, event).await;
    }
}

impl Default for WebSocketManager {
    fn default() -> Self {
        Self::new()
    }
}

/// WebSocket handler for state streaming
pub async fn state_websocket_handler(
    ws: WebSocket,
    Path(state_id): Path<String>,
    State(state): State<AppState>,
) -> impl into_response {
    ws.on_upgrade(move |socket| handle_state_socket(socket, state_id, state))
}

async fn handle_state_socket(
    socket: WebSocket,
    state_id: String,
    _state: AppState,
) {
    let (mut sender, mut receiver) = socket.split();

    info!("WebSocket connection established for state: {}", state_id);

    // Send initial connection message
    let init_event = WsEvent::SnapshotProgress {
        state_id: state_id.clone(),
        phase: "connected".to_string(),
        percent: 0.0,
        bytes_transferred: 0,
        elapsed_ms: 0,
    };

    if let Ok(msg) = serde_json::to_string(&init_event) {
        let _ = sender.send(Message::Text(msg.into())).await;
    }

    // Handle incoming messages
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // Handle client messages (ping, subscribe, etc.)
                if let Ok(event) = serde_json::from_str::<WsEvent>(&text) {
                    match event {
                        WsEvent::Heartbeat { timestamp } => {
                            let pong = WsEvent::Heartbeat {
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as u64,
                            };
                            if let Ok(msg) = serde_json::to_string(&pong) {
                                let _ = sender.send(Message::Text(msg.into())).await;
                            }
                        }
                        _ => {
                            warn!("Unhandled WebSocket event: {:?}", event);
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket connection closed for state: {}", state_id);
                break;
            }
            Ok(Message::Ping(data)) => {
                let _ = sender.send(Message::Pong(data)).await;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }
}

/// WebSocket handler for collaborative sessions
pub async fn collab_websocket_handler(
    ws: WebSocket,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
) -> impl into_response {
    ws.on_upgrade(move |socket| handle_collab_socket(socket, session_id, state))
}

async fn handle_collab_socket(
    socket: WebSocket,
    session_id: String,
    _state: AppState,
) {
    let (mut sender, mut receiver) = socket.split();

    info!("Collaborative WebSocket connection established: {}", session_id);

    // Generate user ID for this session
    let user_id = uuid::Uuid::new_v4().to_string();

    // Send welcome message
    let welcome = WsEvent::CollaborativeEvent {
        session_id: session_id.clone(),
        user_id: user_id.clone(),
        action: "joined".to_string(),
        payload: serde_json::json!({
            "timestamp": chrono::Utc::now(),
        }),
    };

    if let Ok(msg) = serde_json::to_string(&welcome) {
        let _ = sender.send(Message::Text(msg.into())).await;
    }

    // Handle incoming messages with room management
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // Parse and broadcast to other collaborators in the same session
                info!("Received collaborative message from {}: {}", user_id, text);

                // Parse message as WsEvent
                if let Ok(event) = serde_json::from_str::<WsEvent>(&text) {
                    // Broadcast to session using WebSocketManager
                    // Session ID serves as the room identifier for message routing
                    let broadcast_event = WsEvent::CollaborativeEvent {
                        session_id: session_id.clone(),
                        user_id: user_id.clone(),
                        action: "message".to_string(),
                        payload: serde_json::json!({
                            "content": text,
                            "timestamp": chrono::Utc::now(),
                        }),
                    };

                    // Broadcast to all subscribers in this session/room
                    manager.broadcast(&session_id, broadcast_event).await;
                }
            }
            Ok(Message::Close(_)) => {
                info!("Collaborative WebSocket closed: {}", session_id);

                // Notify others of departure
                let left = WsEvent::CollaborativeEvent {
                    session_id: session_id.clone(),
                    user_id: user_id.clone(),
                    action: "left".to_string(),
                    payload: serde_json::json!({
                        "timestamp": chrono::Utc::now(),
                    }),
                };
                
                // Broadcast departure to room
                manager.broadcast(&session_id, left).await;
                break;
            }
            Ok(Message::Ping(data)) => {
                let _ = sender.send(Message::Pong(data)).await;
            }
            Err(e) => {
                error!("Collaborative WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_websocket_manager_broadcast() {
        let manager = WebSocketManager::new();

        // Subscribe to a state
        let mut rx = manager.subscribe("test-state").await;

        // Broadcast an event
        manager.snapshot_progress("test-state", "uploading", 50.0, 1024, 100).await;

        // Receive the event
        let event = rx.recv().await.unwrap();
        
        match event {
            WsEvent::SnapshotProgress { percent, .. } => {
                assert_eq!(percent, 50.0);
            }
            _ => panic!("Expected SnapshotProgress event"),
        }
    }

    #[test]
    fn test_ws_event_serialization() {
        let event = WsEvent::Heartbeat { timestamp: 12345 };
        let json = serde_json::to_string(&event).unwrap();
        
        assert!(json.contains("heartbeat"));
        assert!(json.contains("12345"));
    }
}
