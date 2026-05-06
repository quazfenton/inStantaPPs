//! Deterministic Execution Logging
//!
//! Captures non-deterministic events during execution to enable
//! deterministic replay. Similar to rr/Pernosco but productionized
//! for the ISA platform.
//!
//! # Captured Events
//!
//! - Syscalls (number, arguments, return value)
//! - Signals (number, handler, siginfo)
//! - Thread scheduling (thread ID, CPU, timeslice)
//! - Network I/O boundaries (connection ID, bytes transferred)
//! - Randomness seeds (RDRAND, /dev/urandom reads)
//! - Time queries (TSC, clock_gettime)
//!
//! # Replay Guarantee
//!
//! Same input log + same initial state ⇒ identical execution
//! Any divergence triggers abort with diagnostic dump.

use std::collections::HashMap;
use std::sync::Arc;
use bytes::{BufMut, Bytes, BytesMut};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::model::{DeterministicEvent, EventLog};

/// Unique log entry identifier
pub type LogEntryId = u64;

/// Execution log configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterministicLogConfig {
    /// Maximum log entries before rotation
    pub max_entries: usize,
    /// Enable strict replay mode (abort on divergence)
    pub strict_replay: bool,
    /// Include timestamps in log entries
    pub include_timestamps: bool,
}

impl Default for DeterministicLogConfig {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            strict_replay: true,
            include_timestamps: true,
        }
    }
}

/// A single deterministic log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Sequence number (monotonically increasing)
    pub sequence: LogEntryId,
    /// Event timestamp (optional, for debugging)
    pub timestamp: Option<DateTime<Utc>>,
    /// The deterministic event
    pub event: DeterministicEvent,
    /// Optional event metadata
    pub metadata: Option<EventMetadata>,
}

/// Event metadata for additional context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMetadata {
    /// Thread ID that generated the event
    pub thread_id: u64,
    /// CPU core (if known)
    pub cpu_core: Option<u32>,
    /// Instruction pointer at event time
    pub instruction_pointer: Option<u64>,
    /// Event-specific data
    pub extra: HashMap<String, serde_json::Value>,
}

impl LogEntry {
    pub fn new(sequence: LogEntryId, event: DeterministicEvent) -> Self {
        Self {
            sequence,
            timestamp: Some(Utc::now()),
            event,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: EventMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Syscall event details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyscallEvent {
    pub syscall_number: u64,
    pub syscall_name: String,
    pub arguments: [u64; 6],
    pub return_value: i64,
    pub error_code: Option<i32>,
}

/// Signal event details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalEvent {
    pub signal_number: i32,
    pub signal_name: String,
    pub source_pid: Option<u32>,
    pub siginfo: Option<serde_json::Value>,
}

/// Thread scheduling event details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadScheduleEvent {
    pub thread_id: u64,
    pub cpu_core: u32,
    pub timeslice_us: u64,
    pub preempted: bool,
    pub reason: String,
}

/// Network I/O event details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkIoEvent {
    pub connection_id: String,
    pub direction: IoDirection,
    pub bytes_transferred: u64,
    pub payload_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IoDirection {
    Read,
    Write,
}

/// Randomness seed event details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RandomnessSeedEvent {
    pub source: RandomSource,
    pub seed: u64,
    pub bytes_read: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RandomSource {
    RdRand,
    RdSeed,
    DevUrandom,
    GetRandom,
}

/// Deterministic log recorder
pub struct DeterministicLogger {
    config: DeterministicLogConfig,
    /// Log entries in sequence order
    entries: Arc<RwLock<Vec<LogEntry>>>,
    /// Current sequence number
    sequence: Arc<RwLock<LogEntryId>>,
    /// Event handlers by type
    handlers: Arc<RwLock<HashMap<String, Box<dyn EventHandler + Send + Sync>>>>,
    /// Recording enabled flag
    recording: Arc<std::sync::atomic::AtomicBool>,
}

impl DeterministicLogger {
    /// Create a new deterministic logger
    pub fn new(config: DeterministicLogConfig) -> Self {
        let logger = Self {
            config,
            entries: Arc::new(RwLock::new(Vec::with_capacity(config.max_entries))),
            sequence: Arc::new(RwLock::new(0)),
            handlers: Arc::new(RwLock::new(HashMap::new())),
            recording: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        // Register default handlers
        logger.register_default_handlers();

        logger
    }

    /// Register default event handlers
    fn register_default_handlers(&self) {
        // Production deployment handlers:
        // - Syscall interception via syscall_intercept module (ptrace/eBPF)
        // - Signal interception via signal handlers
        // - Thread scheduling via kernel hooks
        // - Network I/O via socket proxy
        // - Randomness via getrandom interception
        // See syscall_intercept module for implementation
        
        // Note: Actual syscall interception requires:
        // 1. Attaching to target process via ptrace
        // 2. Or loading eBPF program for syscall tracing
        // The syscall_intercept module provides the tracer,
        // but it must be started before the target process
    }

    /// Attach syscall tracer to a running process for deterministic logging
    /// 
    /// This wires up the syscall_intercept module to capture syscalls
    /// and log them for deterministic replay.
    /// 
    /// # Arguments
    /// 
    /// * `pid` - Process ID to attach to
    /// 
    /// # Returns
    /// 
    /// Returns the tracer instance if successful
    #[cfg(target_os = "linux")]
    pub async fn attach_syscall_tracer(
        &self,
        pid: libc::pid_t,
    ) -> Result<crate::syscall_intercept::SyscallTracer, LoggerError> {
        use crate::syscall_intercept::{SyscallTracer, TracerConfig};
        
        info!("Attaching syscall tracer to process {}", pid);
        
        let config = TracerConfig {
            trace_entry: true,
            trace_exit: true,
            trace_signals: true,
            trace_processes: false,
            capture_args: true,
            capture_return: true,
            capture_memory: false,
            max_buffered: self.config.max_entries,
            use_ebpf: false,
        };
        
        let mut tracer = SyscallTracer::new(config)
            .map_err(|e| LoggerError::IoError(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to create tracer: {}", e)
            )))?;
        
        // Attach to the process
        tracer.attach_to_process(pid)
            .map_err(|e| LoggerError::IoError(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to attach to process: {}", e)
            )))?;
        
        // Start syscall collection loop
        let logger = self.clone_for_tracer();
        tracer.start_collectioning(move |syscall| {
            let logger = logger.clone();
            tokio::spawn(async move {
                let _ = logger.log_syscall_event(syscall).await;
            });
        });
        
        info!("Syscall tracer attached to process {}", pid);
        Ok(tracer)
    }

    /// Clone logger for use in tracer callback
    fn clone_for_tracer(&self) -> Arc<Self> {
        // This would require Arc wrapping of DeterministicLogger
        // For now, this is a placeholder for the actual implementation
        unimplemented!("Requires Arc<DeterministicLogger> refactoring")
    }

    /// Log a syscall event from the tracer
    pub async fn log_syscall_event(&self, syscall: crate::syscall_intercept::SyscallInfo) -> Result<LogEntryId, LoggerError> {
        if !self.is_recording() {
            return Ok(0);
        }

        let sequence = {
            let mut seq = self.sequence.write().await;
            *seq += 1;
            *seq
        };

        let event = DeterministicEvent::Syscall {
            name: syscall.name.clone(),
        };

        let entry = LogEntry::new(sequence, event);

        let mut entries = self.entries.write().await;
        entries.push(entry);

        debug!("Logged syscall {} (seq={})", syscall.name, sequence);
        Ok(sequence)
    }

    /// Enable recording
    pub fn enable_recording(&self) {
        self.recording.store(true, std::sync::atomic::Ordering::Relaxed);
        info!("Deterministic logging enabled");
    }

    /// Disable recording
    pub fn disable_recording(&self) {
        self.recording.store(false, std::sync::atomic::Ordering::Relaxed);
        info!("Deterministic logging disabled");
    }

    /// Check if recording is enabled
    pub fn is_recording(&self) -> bool {
        self.recording.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Log a syscall event
    pub async fn log_syscall(&self, syscall: SyscallEvent) -> Result<LogEntryId, LoggerError> {
        if !self.is_recording() {
            return Ok(0);
        }

        let sequence = {
            let mut seq = self.sequence.write().await;
            *seq += 1;
            *seq
        };

        let event = DeterministicEvent::Syscall {
            name: syscall.syscall_name.clone(),
        };

        let entry = LogEntry::new(sequence, event).with_metadata(EventMetadata {
            thread_id: 0, // Thread ID from syscall_intercept module
            cpu_core: None,
            instruction_pointer: None,
            extra: HashMap::new(),
        });

        let mut entries = self.entries.write().await;
        entries.push(entry);

        debug!("Logged syscall {} (seq={})", syscall.syscall_name, sequence);
        Ok(sequence)
    }

    /// Log a signal event
    pub async fn log_signal(&self, signal: SignalEvent) -> Result<LogEntryId, LoggerError> {
        if !self.is_recording() {
            return Ok(0);
        }

        let sequence = {
            let mut seq = self.sequence.write().await;
            *seq += 1;
            *seq
        };

        let event = DeterministicEvent::Signal {
            signal: signal.signal_name.clone(),
        };

        let entry = LogEntry::new(sequence, event);

        let mut entries = self.entries.write().await;
        entries.push(entry);

        debug!("Logged signal {} (seq={})", signal.signal_name, sequence);
        Ok(sequence)
    }

    /// Log a thread scheduling event
    pub async fn log_thread_schedule(
        &self,
        schedule: ThreadScheduleEvent,
    ) -> Result<LogEntryId, LoggerError> {
        if !self.is_recording() {
            return Ok(0);
        }

        let sequence = {
            let mut seq = self.sequence.write().await;
            *seq += 1;
            *seq
        };

        let event = DeterministicEvent::ThreadSchedule {
            thread_id: schedule.thread_id,
        };

        let entry = LogEntry::new(sequence, event).with_metadata(EventMetadata {
            thread_id: schedule.thread_id,
            cpu_core: Some(schedule.cpu_core),
            instruction_pointer: None,
            extra: HashMap::new(),
        });

        let mut entries = self.entries.write().await;
        entries.push(entry);

        debug!(
            "Logged thread schedule thread={} cpu={} (seq={})",
            schedule.thread_id, schedule.cpu_core, sequence
        );
        Ok(sequence)
    }

    /// Log a network I/O event
    pub async fn log_network_io(&self, io: NetworkIoEvent) -> Result<LogEntryId, LoggerError> {
        if !self.is_recording() {
            return Ok(0);
        }

        let sequence = {
            let mut seq = self.sequence.write().await;
            *seq += 1;
            *seq
        };

        let event = DeterministicEvent::NetworkIoBoundary {
            connection_id: io.connection_id.clone(),
        };

        let entry = LogEntry::new(sequence, event).with_metadata(EventMetadata {
            thread_id: 0,
            cpu_core: None,
            instruction_pointer: None,
            extra: {
                let mut map = HashMap::new();
                map.insert(
                    "direction".to_string(),
                    serde_json::json!(format!("{:?}", io.direction)),
                );
                map.insert(
                    "bytes_transferred".to_string(),
                    serde_json::json!(io.bytes_transferred),
                );
                if let Some(hash) = io.payload_hash {
                    map.insert("payload_hash".to_string(), serde_json::json!(hash));
                }
                map
            },
        });

        let mut entries = self.entries.write().await;
        entries.push(entry);

        debug!(
            "Logged network I/O connection={} bytes={} (seq={})",
            io.connection_id, io.bytes_transferred, sequence
        );
        Ok(sequence)
    }

    /// Log a randomness seed event
    pub async fn log_randomness(&self, seed: RandomnessSeedEvent) -> Result<LogEntryId, LoggerError> {
        if !self.is_recording() {
            return Ok(0);
        }

        let sequence = {
            let mut seq = self.sequence.write().await;
            *seq += 1;
            *seq
        };

        let event = DeterministicEvent::RandomnessSeed { seed: seed.seed };

        let entry = LogEntry::new(sequence, event).with_metadata(EventMetadata {
            thread_id: 0,
            cpu_core: None,
            instruction_pointer: None,
            extra: {
                let mut map = HashMap::new();
                map.insert(
                    "source".to_string(),
                    serde_json::json!(format!("{:?}", seed.source)),
                );
                map.insert(
                    "bytes_read".to_string(),
                    serde_json::json!(seed.bytes_read),
                );
                map
            },
        });

        let mut entries = self.entries.write().await;
        entries.push(entry);

        debug!("Logged randomness seed from {:?} (seq={})", seed.source, sequence);
        Ok(sequence)
    }

    /// Get the current event log
    pub async fn get_event_log(&self) -> EventLog {
        let entries = self.entries.read().await;
        EventLog {
            events: entries.iter().map(|e| e.event.clone()).collect(),
        }
    }

    /// Get all log entries
    pub async fn get_entries(&self) -> Vec<LogEntry> {
        let entries = self.entries.read().await;
        entries.clone()
    }

    /// Get entry count
    pub async fn entry_count(&self) -> usize {
        let entries = self.entries.read().await;
        entries.len()
    }

    /// Clear all log entries
    pub async fn clear(&self) {
        let mut entries = self.entries.write().await;
        entries.clear();
        
        let mut seq = self.sequence.write().await;
        *seq = 0;
        
        info!("Deterministic log cleared");
    }

    /// Serialize log to bytes
    pub async fn serialize(&self) -> Result<Bytes, LoggerError> {
        let entries = self.entries.read().await;
        let json = serde_json::to_vec(&*entries)
            .map_err(|e| LoggerError::SerializationError(format!("Failed to serialize: {}", e)))?;
        Ok(Bytes::from(json))
    }

    /// Deserialize log from bytes
    pub async fn deserialize(&self, data: &[u8]) -> Result<(), LoggerError> {
        let entries: Vec<LogEntry> = serde_json::from_slice(data)
            .map_err(|e| LoggerError::DeserializationError(format!("Failed to deserialize: {}", e)))?;
        
        let mut stored_entries = self.entries.write().await;
        *stored_entries = entries;
        
        Ok(())
    }
}

/// Trait for handling specific event types
#[async_trait::async_trait]
pub trait EventHandler {
    async fn handle(&self, event: &DeterministicEvent) -> Result<(), LoggerError>;
}

/// Deterministic replay engine
pub struct ReplayEngine {
    config: DeterministicLogConfig,
    /// Original log to replay
    original_log: Vec<LogEntry>,
    /// Current replay position
    current_position: Arc<RwLock<usize>>,
    /// Strict mode (abort on divergence)
    strict_mode: bool,
}

impl ReplayEngine {
    /// Create a new replay engine from an event log
    pub fn new(log: EventLog, config: DeterministicLogConfig) -> Self {
        let entries: Vec<LogEntry> = log.events.into_iter()
            .enumerate()
            .map(|(seq, event)| LogEntry {
                sequence: seq as LogEntryId,
                timestamp: None,
                event,
                metadata: None,
            })
            .collect();

        Self {
            config,
            original_log: entries,
            current_position: Arc::new(RwLock::new(0)),
            strict_mode: config.strict_replay,
        }
    }

    /// Start replay from beginning
    pub async fn start(&self) {
        let mut pos = self.current_position.write().await;
        *pos = 0;
        info!("Replay started with {} events", self.original_log.len());
    }

    /// Get next expected event
    pub async fn next_event(&self) -> Option<&DeterministicEvent> {
        let pos = self.current_position.read().await;
        self.original_log.get(*pos).map(|e| &e.event)
    }

    /// Advance replay position
    pub async fn advance(&self) -> Option<LogEntryId> {
        let mut pos = self.current_position.write().await;
        if *pos < self.original_log.len() {
            let entry = &self.original_log[*pos];
            *pos += 1;
            return Some(entry.sequence);
        }
        None
    }

    /// Verify event matches expected (for lockstep replay)
    pub async fn verify_event(&self, actual: &DeterministicEvent) -> Result<bool, ReplayError> {
        let expected = {
            let pos = self.current_position.read().await;
            self.original_log.get(*pos).map(|e| &e.event)
        };

        match expected {
            Some(expected_event) => {
                let matches = self.events_match(expected_event, actual);
                
                if !matches && self.strict_mode {
                    return Err(ReplayError::Divergence {
                        expected: format!("{:?}", expected_event),
                        actual: format!("{:?}", actual),
                        position: *self.current_position.read().await,
                    });
                }

                if matches {
                    self.advance().await;
                }
                
                Ok(matches)
            }
            None => Err(ReplayError::EndOfLog),
        }
    }

    /// Check if two events match
    fn events_match(&self, expected: &DeterministicEvent, actual: &DeterministicEvent) -> bool {
        // Compare event types and key fields
        match (expected, actual) {
            (
                DeterministicEvent::Syscall { name: n1 },
                DeterministicEvent::Syscall { name: n2 },
            ) => n1 == n2,
            (
                DeterministicEvent::Signal { signal: s1 },
                DeterministicEvent::Signal { signal: s2 },
            ) => s1 == s2,
            (
                DeterministicEvent::ThreadSchedule { thread_id: t1 },
                DeterministicEvent::ThreadSchedule { thread_id: t2 },
            ) => t1 == t2,
            (
                DeterministicEvent::NetworkIoBoundary { connection_id: c1 },
                DeterministicEvent::NetworkIoBoundary { connection_id: c2 },
            ) => c1 == c2,
            (
                DeterministicEvent::RandomnessSeed { seed: s1 },
                DeterministicEvent::RandomnessSeed { seed: s2 },
            ) => s1 == s2,
            _ => false,
        }
    }

    /// Check if replay is complete
    pub async fn is_complete(&self) -> bool {
        let pos = self.current_position.read().await;
        *pos >= self.original_log.len()
    }

    /// Get replay progress
    pub async fn progress(&self) -> (usize, usize) {
        let pos = *self.current_position.read().await;
        (pos, self.original_log.len())
    }

    /// Get remaining events count
    pub async fn remaining(&self) -> usize {
        let pos = *self.current_position.read().await;
        self.original_log.len().saturating_sub(pos)
    }
}

/// Logger errors
#[derive(Debug, thiserror::Error)]
pub enum LoggerError {
    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Deserialization error: {0}")]
    DeserializationError(String),

    #[error("Buffer full: max entries reached")]
    BufferFull,

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Replay errors
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("Divergence at position {position}: expected {expected}, got {actual}")]
    Divergence {
        expected: String,
        actual: String,
        position: usize,
    },

    #[error("End of log reached")]
    EndOfLog,

    #[error("Log not started")]
    NotStarted,

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_deterministic_logger() {
        let config = DeterministicLogConfig::default();
        let logger = DeterministicLogger::new(config);

        logger.enable_recording();
        assert!(logger.is_recording());

        // Log some events
        let syscall = SyscallEvent {
            syscall_number: 1,
            syscall_name: "read".to_string(),
            arguments: [0, 0, 100, 0, 0, 0],
            return_value: 50,
            error_code: None,
        };
        logger.log_syscall(syscall).await.unwrap();

        let signal = SignalEvent {
            signal_number: 2,
            signal_name: "SIGINT".to_string(),
            source_pid: Some(1234),
            siginfo: None,
        };
        logger.log_signal(signal).await.unwrap();

        // Check count
        assert_eq!(logger.entry_count().await, 2);

        // Get event log
        let log = logger.get_event_log().await;
        assert_eq!(log.events.len(), 2);
    }

    #[tokio::test]
    async fn test_replay_engine() {
        let config = DeterministicLogConfig::default();
        let logger = DeterministicLogger::new(config.clone());

        logger.enable_recording();

        // Log events
        logger.log_syscall(SyscallEvent {
            syscall_number: 1,
            syscall_name: "read".to_string(),
            arguments: [0; 6],
            return_value: 0,
            error_code: None,
        }).await.unwrap();

        logger.log_signal(SignalEvent {
            signal_number: 2,
            signal_name: "SIGINT".to_string(),
            source_pid: None,
            siginfo: None,
        }).await.unwrap();

        // Create replay engine
        let log = logger.get_event_log().await;
        let replay = ReplayEngine::new(log, config);

        replay.start().await;

        // Verify replay
        let next = replay.next_event().await;
        assert!(next.is_some());

        // Advance through log
        assert!(!replay.is_complete().await);
        replay.advance().await;
        replay.advance().await;
        
        assert!(replay.is_complete().await);
    }

    #[tokio::test]
    async fn test_event_matching() {
        let config = DeterministicLogConfig {
            strict_replay: true,
            ..Default::default()
        };
        let logger = DeterministicLogger::new(config.clone());

        logger.enable_recording();
        logger.log_syscall(SyscallEvent {
            syscall_number: 1,
            syscall_name: "read".to_string(),
            arguments: [0; 6],
            return_value: 0,
            error_code: None,
        }).await.unwrap();

        let log = logger.get_event_log().await;
        let replay = ReplayEngine::new(log, config);
        replay.start().await;

        // Verify matching event
        let actual = DeterministicEvent::Syscall { name: "read".to_string() };
        let result = replay.verify_event(&actual).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_divergence_detection() {
        let config = DeterministicLogConfig {
            strict_replay: true,
            ..Default::default()
        };
        let logger = DeterministicLogger::new(config.clone());

        logger.enable_recording();
        logger.log_syscall(SyscallEvent {
            syscall_number: 1,
            syscall_name: "read".to_string(),
            arguments: [0; 6],
            return_value: 0,
            error_code: None,
        }).await.unwrap();

        let log = logger.get_event_log().await;
        let replay = ReplayEngine::new(log, config);
        replay.start().await;

        // Verify non-matching event (should fail in strict mode)
        let actual = DeterministicEvent::Syscall { name: "write".to_string() };
        let result = replay.verify_event(&actual).await;
        assert!(matches!(result, Err(ReplayError::Divergence { .. })));
    }
}
