//! Metrics and Telemetry Module
//!
//! Provides Prometheus-compatible metrics for monitoring ISA operations.
//!
//! # Metrics
//!
//! - `isa_snapshots_total` - Total snapshots created
//! - `isa_resumes_total` - Total resumes performed
//! - `isa_forks_total` - Total forks performed
//! - `isa_snapshot_duration_seconds` - Snapshot creation latency
//! - `isa_resume_duration_seconds` - Resume latency
//! - `isa_memory_bytes` - Memory tracked by temperature
//! - `isa_state_store_size_bytes` - Total state store size
//! - `isa_active_connections` - Active WebSocket connections
//!
//! # Endpoint
//!
//! - `GET /metrics` - Prometheus metrics

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Metrics registry
pub struct MetricsRegistry {
    /// Counter metrics
    counters: Arc<RwLock<HashMap<String, u64>>>,
    /// Gauge metrics
    gauges: Arc<RwLock<HashMap<String, i64>>>,
    /// Histogram metrics (stored as summaries)
    histograms: Arc<RwLock<HashMap<String, HistogramData>>>,
    /// Start time for uptime calculation
    start_time: Instant,
}

/// Histogram data for summaries
#[derive(Debug, Clone)]
struct HistogramData {
    count: u64,
    sum: f64,
    buckets: Vec<(f64, u64)>, // (bucket_bound, cumulative_count)
}

impl MetricsRegistry {
    /// Create a new metrics registry
    pub fn new() -> Self {
        let registry = Self {
            counters: Arc::new(RwLock::new(HashMap::new())),
            gauges: Arc::new(RwLock::new(HashMap::new())),
            histograms: Arc::new(RwLock::new(HashMap::new())),
            start_time: Instant::now(),
        };

        // Initialize default counters
        registry.inc("isa_snapshots_total", 0);
        registry.inc("isa_resumes_total", 0);
        registry.inc("isa_forks_total", 0);
        registry.inc("isa_errors_total", 0);

        registry
    }

    /// Increment a counter
    pub fn inc(&self, name: &str, value: u64) {
        let mut counters = self.counters.write();
        *counters.entry(name.to_string()).or_insert(0) += value;
    }

    /// Decrement a gauge
    pub fn dec(&self, name: &str, value: i64) {
        let mut gauges = self.gauges.write();
        *gauges.entry(name.to_string()).or_insert(0) -= value;
    }

    /// Set a gauge value
    pub fn gauge(&self, name: &str, value: i64) {
        let mut gauges = self.gauges.write();
        *gauges.entry(name.to_string()).or_insert(0) = value;
    }

    /// Observe a histogram value
    pub fn observe(&self, name: &str, value: f64) {
        let mut histograms = self.histograms.write();
        
        let data = histograms.entry(name.to_string()).or_insert_with(|| HistogramData {
            count: 0,
            sum: 0.0,
            buckets: vec![
                (0.001, 0),
                (0.005, 0),
                (0.01, 0),
                (0.025, 0),
                (0.05, 0),
                (0.1, 0),
                (0.25, 0),
                (0.5, 0),
                (1.0, 0),
                (2.5, 0),
                (5.0, 0),
                (10.0, 0),
                (f64::INFINITY, 0),
            ],
        });

        data.count += 1;
        data.sum += value;

        // Update buckets
        for (bound, count) in &mut data.buckets {
            if value <= *bound {
                *count += 1;
            }
        }
    }

    /// Record snapshot duration
    pub fn record_snapshot(&self, duration_secs: f64, bytes: u64) {
        self.inc("isa_snapshots_total", 1);
        self.observe("isa_snapshot_duration_seconds", duration_secs);
        self.observe("isa_snapshot_size_bytes", bytes as f64);
    }

    /// Record resume duration
    pub fn record_resume(&self, duration_secs: f64, pages: u64) {
        self.inc("isa_resumes_total", 1);
        self.observe("isa_resume_duration_seconds", duration_secs);
        self.observe("isa_resume_pages_loaded", pages as f64);
    }

    /// Record fork operation
    pub fn record_fork(&self) {
        self.inc("isa_forks_total", 1);
    }

    /// Record error
    pub fn record_error(&self, error_type: &str) {
        self.inc("isa_errors_total", 1);
        self.inc(&format!("isa_errors_{}", error_type), 1);
    }

    /// Set memory metrics
    pub fn set_memory_metrics(&self, hot: u64, warm: u64, cold: u64) {
        self.gauge("isa_memory_hot_bytes", hot as i64);
        self.gauge("isa_memory_warm_bytes", warm as i64);
        self.gauge("isa_memory_cold_bytes", cold as i64);
    }

    /// Set state store metrics
    pub fn set_state_store_metrics(&self, size_bytes: u64, states: u64, pages: u64) {
        self.gauge("isa_state_store_size_bytes", size_bytes as i64);
        self.gauge("isa_state_store_states_total", states as i64);
        self.gauge("isa_state_store_pages_total", pages as i64);
    }

    /// Generate Prometheus-format metrics
    pub fn prometheus_metrics(&self) -> String {
        let mut output = String::new();

        // Counters
        let counters = self.counters.read();
        output.push_str("# HELP isa_snapshots_total Total number of snapshots created\n");
        output.push_str("# TYPE isa_snapshots_total counter\n");
        if let Some(v) = counters.get("isa_snapshots_total") {
            output.push_str(&format!("isa_snapshots_total {}\n", v));
        }

        output.push_str("# HELP isa_resumes_total Total number of resumes performed\n");
        output.push_str("# TYPE isa_resumes_total counter\n");
        if let Some(v) = counters.get("isa_resumes_total") {
            output.push_str(&format!("isa_resumes_total {}\n", v));
        }

        output.push_str("# HELP isa_forks_total Total number of forks performed\n");
        output.push_str("# TYPE isa_forks_total counter\n");
        if let Some(v) = counters.get("isa_forks_total") {
            output.push_str(&format!("isa_forks_total {}\n", v));
        }

        output.push_str("# HELP isa_errors_total Total number of errors\n");
        output.push_str("# TYPE isa_errors_total counter\n");
        if let Some(v) = counters.get("isa_errors_total") {
            output.push_str(&format!("isa_errors_total {}\n", v));
        }

        // Gauges
        let gauges = self.gauges.read();
        output.push_str("# HELP isa_memory_hot_bytes Hot memory bytes\n");
        output.push_str("# TYPE isa_memory_hot_bytes gauge\n");
        if let Some(v) = gauges.get("isa_memory_hot_bytes") {
            output.push_str(&format!("isa_memory_hot_bytes {}\n", v));
        }

        output.push_str("# HELP isa_state_store_size_bytes Total state store size in bytes\n");
        output.push_str("# TYPE isa_state_store_size_bytes gauge\n");
        if let Some(v) = gauges.get("isa_state_store_size_bytes") {
            output.push_str(&format!("isa_state_store_size_bytes {}\n", v));
        }

        // Histograms (as summaries)
        let histograms = self.histograms.read();
        if let Some(data) = histograms.get("isa_snapshot_duration_seconds") {
            output.push_str("# HELP isa_snapshot_duration_seconds Snapshot duration in seconds\n");
            output.push_str("# TYPE isa_snapshot_duration_seconds summary\n");
            output.push_str(&format!(
                "isa_snapshot_duration_seconds{{quantile=\"0.5\"}} {}\n",
                self.calculate_quantile(&data.buckets, data.count, 0.5)
            ));
            output.push_str(&format!(
                "isa_snapshot_duration_seconds{{quantile=\"0.9\"}} {}\n",
                self.calculate_quantile(&data.buckets, data.count, 0.9)
            ));
            output.push_str(&format!(
                "isa_snapshot_duration_seconds{{quantile=\"0.99\"}} {}\n",
                self.calculate_quantile(&data.buckets, data.count, 0.99)
            ));
            output.push_str(&format!(
                "isa_snapshot_duration_seconds_sum {}\n",
                data.sum
            ));
            output.push_str(&format!(
                "isa_snapshot_duration_seconds_count {}\n",
                data.count
            ));
        }

        // Uptime
        let uptime = self.start_time.elapsed().as_secs();
        output.push_str("# HELP isa_uptime_seconds Server uptime in seconds\n");
        output.push_str("# TYPE isa_uptime_seconds gauge\n");
        output.push_str(&format!("isa_uptime_seconds {}\n", uptime));

        output
    }

    fn calculate_quantile(&self, buckets: &[(f64, u64)], total: u64, quantile: f64) -> f64 {
        let target = (total as f64 * quantile) as u64;
        for (bound, count) in buckets {
            if *count >= target {
                return *bound;
            }
        }
        buckets.last().map(|(b, _)| *b).unwrap_or(f64::INFINITY)
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Timer helper for measuring durations
pub struct Timer {
    start: Instant,
    name: String,
    registry: Arc<MetricsRegistry>,
}

impl Timer {
    pub fn new(registry: Arc<MetricsRegistry>, name: String) -> Self {
        Self {
            start: Instant::now(),
            name,
            registry,
        }
    }

    pub fn observe_duration(self) {
        let duration = self.start.elapsed().as_secs_f64();
        self.registry.observe(&self.name, duration);
    }
}

/// Metrics snapshot for serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub snapshots_total: u64,
    pub resumes_total: u64,
    pub forks_total: u64,
    pub errors_total: u64,
    pub avg_snapshot_duration_ms: f64,
    pub avg_resume_duration_ms: f64,
    pub uptime_secs: u64,
}

impl MetricsRegistry {
    /// Get metrics snapshot
    pub fn snapshot(&self) -> MetricsSnapshot {
        let counters = self.counters.read();
        let histograms = self.histograms.read();

        let avg_snapshot = histograms
            .get("isa_snapshot_duration_seconds")
            .map(|h| if h.count > 0 { h.sum / h.count as f64 * 1000.0 } else { 0.0 })
            .unwrap_or(0.0);

        let avg_resume = histograms
            .get("isa_resume_duration_seconds")
            .map(|h| if h.count > 0 { h.sum / h.count as f64 * 1000.0 } else { 0.0 })
            .unwrap_or(0.0);

        MetricsSnapshot {
            snapshots_total: *counters.get("isa_snapshots_total").unwrap_or(&0),
            resumes_total: *counters.get("isa_resumes_total").unwrap_or(&0),
            forks_total: *counters.get("isa_forks_total").unwrap_or(&0),
            errors_total: *counters.get("isa_errors_total").unwrap_or(&0),
            avg_snapshot_duration_ms: avg_snapshot,
            avg_resume_duration_ms: avg_resume,
            uptime_secs: self.start_time.elapsed().as_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_increment() {
        let registry = MetricsRegistry::new();
        registry.inc("test_counter", 1);
        registry.inc("test_counter", 2);

        let counters = registry.counters.read();
        assert_eq!(counters.get("test_counter"), Some(&3));
    }

    #[test]
    fn test_gauge_set() {
        let registry = MetricsRegistry::new();
        registry.gauge("test_gauge", 100);

        let gauges = registry.gauges.read();
        assert_eq!(gauges.get("test_gauge"), Some(&100));
    }

    #[test]
    fn test_histogram_observe() {
        let registry = MetricsRegistry::new();
        
        registry.observe("test_histogram", 0.05);
        registry.observe("test_histogram", 0.1);
        registry.observe("test_histogram", 0.5);

        let histograms = registry.histograms.read();
        let data = histograms.get("test_histogram").unwrap();
        
        assert_eq!(data.count, 3);
        assert!((data.sum - 0.65).abs() < 0.001);
    }

    #[test]
    fn test_prometheus_output() {
        let registry = MetricsRegistry::new();
        registry.inc("isa_snapshots_total", 5);
        
        let output = registry.prometheus_metrics();
        
        assert!(output.contains("isa_snapshots_total 5"));
        assert!(output.contains("# TYPE isa_snapshots_total counter"));
    }

    #[test]
    fn test_metrics_snapshot() {
        let registry = MetricsRegistry::new();
        registry.inc("isa_snapshots_total", 10);
        registry.inc("isa_resumes_total", 5);
        
        let snapshot = registry.snapshot();
        
        assert_eq!(snapshot.snapshots_total, 10);
        assert_eq!(snapshot.resumes_total, 5);
        assert!(snapshot.uptime_secs > 0);
    }
}
