//! Edge Orchestration Module
//!
//! Provides distributed state placement and edge node management
//! for low-latency state resume across geographic regions.
//!
//! # Features
//!
//! - Region-aware VM placement
//! - Edge node health monitoring
//! - State caching at edge locations
//! - Latency-based routing
//! - Load balancing across nodes

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug};
use chrono::{DateTime, Utc};
use reqwest;

/// Edge node configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeNodeConfig {
    /// Node ID
    pub node_id: String,
    /// Node address
    pub address: SocketAddr,
    /// Region identifier
    pub region: String,
    /// Maximum concurrent VMs
    pub max_vms: u32,
    /// Health check interval (seconds)
    pub health_check_interval_secs: u64,
}

/// Edge node status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeStatus {
    /// Node is healthy and accepting work
    Healthy,
    /// Node is degraded (high load)
    Degraded,
    /// Node is unhealthy (not responding)
    Unhealthy,
    /// Node is draining (no new work)
    Draining,
}

/// Edge node information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeNode {
    pub config: EdgeNodeConfig,
    pub status: NodeStatus,
    /// Current VM count
    pub current_vms: u32,
    /// Available memory (MB)
    pub available_memory_mb: u32,
    /// CPU utilization (0.0 - 1.0)
    pub cpu_utilization: f32,
    /// Last health check time
    pub last_health_check: Option<DateTime<Utc>>,
    /// Average latency to this node (ms)
    pub avg_latency_ms: f32,
}

impl EdgeNode {
    /// Create a new edge node
    pub fn new(config: EdgeNodeConfig) -> Self {
        Self {
            config,
            status: NodeStatus::Healthy,
            current_vms: 0,
            available_memory_mb: 0,
            cpu_utilization: 0.0,
            last_health_check: None,
            avg_latency_ms: 0.0,
        }
    }

    /// Check if node can accept new VMs
    pub fn can_accept(&self) -> bool {
        self.status == NodeStatus::Healthy && 
        self.current_vms < self.config.max_vms
    }

    /// Calculate node score for placement (higher is better)
    pub fn placement_score(&self) -> f32 {
        if !self.can_accept() {
            return 0.0;
        }

        let capacity_score = 1.0 - (self.current_vms as f32 / self.config.max_vms as f32);
        let memory_score = (self.available_memory_mb as f32 / 8192.0).min(1.0);
        let cpu_score = 1.0 - self.cpu_utilization;
        let latency_score = 1.0 - (self.avg_latency_ms / 100.0).min(1.0);

        // Weighted score
        capacity_score * 0.3 + memory_score * 0.3 + cpu_score * 0.2 + latency_score * 0.2
    }

    /// Update health status
    pub fn update_health(&mut self, health: NodeHealth) {
        self.current_vms = health.current_vms;
        self.available_memory_mb = health.available_memory_mb;
        self.cpu_utilization = health.cpu_utilization;
        self.last_health_check = Some(Utc::now());

        // Update status based on health
        self.status = if health.cpu_utilization > 0.9 || health.available_memory_mb < 512 {
            NodeStatus::Degraded
        } else if health.cpu_utilization > 0.95 || health.available_memory_mb < 256 {
            NodeStatus::Unhealthy
        } else {
            NodeStatus::Healthy
        };
    }
}

/// Node health information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeHealth {
    pub current_vms: u32,
    pub available_memory_mb: u32,
    pub cpu_utilization: f32,
    pub disk_free_gb: u32,
    pub network_rx_mbps: f32,
    pub network_tx_mbps: f32,
}

/// Region information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub region_id: String,
    pub name: String,
    pub edge_nodes: Vec<String>, // Node IDs
    pub total_capacity: u32,
    pub used_capacity: u32,
}

/// Edge orchestrator for distributed state management
pub struct EdgeOrchestrator {
    /// All edge nodes by ID
    nodes: Arc<RwLock<HashMap<String, EdgeNode>>,
    /// Nodes by region
    regions: Arc<RwLock<HashMap<String, Region>>,
    /// Local node ID (for this orchestrator instance)
    local_node_id: String,
    /// Health check running flag
    health_check_running: Arc<std::sync::atomic::AtomicBool>,
}

impl EdgeOrchestrator {
    /// Create a new edge orchestrator
    pub fn new(local_node_id: String) -> Self {
        Self {
            nodes: Arc::new(RwLock::new(HashMap::new())),
            regions: Arc::new(RwLock::new(HashMap::new())),
            local_node_id,
            health_check_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Register an edge node
    pub async fn register_node(&self, node: EdgeNode) {
        let mut nodes = self.nodes.write().await;
        let node_id = node.config.node_id.clone();
        let region = node.config.region.clone();
        
        nodes.insert(node_id.clone(), node);
        
        // Add to region
        let mut regions = self.regions.write().await;
        regions.entry(region).or_insert_with(|| Region {
            region_id: region.clone(),
            name: region.clone(),
            edge_nodes: Vec::new(),
            total_capacity: 0,
            used_capacity: 0,
        }).edge_nodes.push(node_id);

        info!("Registered edge node: {} in region: {}", node_id, region);
    }

    /// Deregister an edge node
    pub async fn deregister_node(&self, node_id: &str) {
        let mut nodes = self.nodes.write().await;
        if let Some(node) = nodes.remove(node_id) {
            // Remove from region
            let mut regions = self.regions.write().await;
            if let Some(region) = regions.get_mut(&node.config.region) {
                region.edge_nodes.retain(|id| id != node_id);
            }
            info!("Deregistered edge node: {}", node_id);
        }
    }

    /// Find the best node for VM placement
    pub async fn find_best_node(&self, region_preference: Option<&str>) -> Option<EdgeNode> {
        let nodes = self.nodes.read().await;
        
        let candidates: Vec<&EdgeNode> = nodes.values()
            .filter(|n| {
                if let Some(pref) = region_preference {
                    n.config.region == pref && n.can_accept()
                } else {
                    n.can_accept()
                }
            })
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Select node with highest score
        candidates.iter()
            .max_by(|a, b| a.placement_score().partial_cmp(&b.placement_score()).unwrap())
            .map(|n| (*n).clone())
    }

    /// Get nodes in a specific region
    pub async fn get_nodes_in_region(&self, region: &str) -> Vec<EdgeNode> {
        let nodes = self.nodes.read().await;
        nodes.values()
            .filter(|n| n.config.region == region)
            .cloned()
            .collect()
    }

    /// Get all regions with their status
    pub async fn get_regions(&self) -> Vec<Region> {
        let regions = self.regions.read().await;
        regions.values().cloned().collect()
    }

    /// Update node health
    pub async fn update_node_health(&self, node_id: &str, health: NodeHealth) {
        let mut nodes = self.nodes.write().await;
        if let Some(node) = nodes.get_mut(node_id) {
            node.update_health(health);
            debug!("Updated health for node {}: {:?}", node_id, node.status);
        }
    }

    /// Increment failure count for a node
    pub async fn increment_failure_count(&self, node_id: &str) -> u32 {
        let mut nodes = self.nodes.write().await;
        if let Some(node) = nodes.get_mut(node_id) {
            // Increment consecutive failure count
            let current_failures = node.current_vms % 10; // Using current_vms as temp storage
            let new_count = current_failures + 1;
            node.current_vms = (node.current_vms / 10) * 10 + new_count;
            return new_count;
        }
        0
    }

    /// Reset failure count for a node
    pub async fn reset_failure_count(&self, node_id: &str) {
        let mut nodes = self.nodes.write().await;
        if let Some(node) = nodes.get_mut(node_id) {
            // Reset consecutive failure count
            node.current_vms = (node.current_vms / 10) * 10;
        }
    }

    /// Mark node as unhealthy
    pub async fn mark_node_unhealthy(&self, node_id: &str) {
        let mut nodes = self.nodes.write().await;
        if let Some(node) = nodes.get_mut(node_id) {
            node.status = NodeStatus::Unhealthy;
            warn!("Node {} marked as unhealthy", node_id);
        }
    }

    /// Get orchestrator statistics
    pub async fn get_stats(&self) -> OrchestratorStats {
        let nodes = self.nodes.read().await;
        let regions = self.regions.read().await;

        let total_nodes = nodes.len();
        let healthy_nodes = nodes.values().filter(|n| n.status == NodeStatus::Healthy).count();
        let total_vms: u32 = nodes.values().map(|n| n.current_vms).sum();
        let total_capacity: u32 = nodes.values().map(|n| n.config.max_vms).sum();

        OrchestratorStats {
            total_nodes,
            healthy_nodes,
            total_vms,
            total_capacity,
            regions_count: regions.len(),
        }
    }

    /// Start health check loop
    pub async fn start_health_checks(&self) {
        self.health_check_running.store(true, std::sync::atomic::Ordering::Relaxed);

        let nodes = self.nodes.clone();
        let running = self.health_check_running.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            let client = reqwest::Client::builder()
                .timeout(tokio::time::Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());

            while running.load(std::sync::atomic::Ordering::Relaxed) {
                interval.tick().await;

                // Send health check requests to all nodes
                let nodes_read = nodes.read().await;
                for (node_id, node) in nodes_read.iter() {
                    let health_url = format!("http://{}/health", node.config.address);

                    match client.get(&health_url).send().await {
                        Ok(response) => {
                            if response.status().is_success() {
                                // Parse health response
                                if let Ok(health) = response.json::<NodeHealth>().await {
                                    // Update node health in orchestrator
                                    self.update_node_health(node_id, health).await;
                                    debug!("Node {} health check passed: {:?}", node_id, health);
                                } else {
                                    debug!("Node {} responded but health parse failed", node_id);
                                    // Increment failure count for parse errors
                                    self.increment_failure_count(node_id).await;
                                }
                            } else {
                                warn!("Node {} health check returned status {}", node_id, response.status());
                                // Increment failure count for bad status
                                self.increment_failure_count(node_id).await;
                            }
                        }
                        Err(e) => {
                            warn!("Node {} health check failed: {}", node_id, e);
                            // Mark node as unhealthy after repeated failures
                            let failure_count = self.increment_failure_count(node_id).await;
                            if failure_count >= 3 {
                                error!("Node {} marked unhealthy after {} failures", node_id, failure_count);
                                // Update node status to unhealthy
                                self.mark_node_unhealthy(node_id).await;
                            }
                        }
                    }
                }
            }
        });

        info!("Started health check loop");
    }

    /// Stop health check loop
    pub fn stop_health_checks(&self) {
        self.health_check_running.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Orchestrator statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorStats {
    pub total_nodes: usize,
    pub healthy_nodes: usize,
    pub total_vms: u32,
    pub total_capacity: u32,
    pub regions_count: usize,
}

/// Latency measurement result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyMeasurement {
    pub node_id: String,
    pub latency_ms: f32,
    pub jitter_ms: f32,
    pub packet_loss_percent: f32,
}

/// Placement decision
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementDecision {
    pub node_id: String,
    pub region: String,
    pub score: f32,
    pub reason: String,
}

impl EdgeOrchestrator {
    /// Make a placement decision
    pub async fn place_vm(&self, region_preference: Option<&str>) -> Option<PlacementDecision> {
        if let Some(node) = self.find_best_node(region_preference).await {
            Some(PlacementDecision {
                node_id: node.config.node_id.clone(),
                region: node.config.region.clone(),
                score: node.placement_score(),
                reason: format!(
                    "Best score based on capacity ({}/{}), memory ({}MB), CPU ({}%), latency ({}ms)",
                    node.current_vms,
                    node.config.max_vms,
                    node.available_memory_mb,
                    (node.cpu_utilization * 100.0) as u32,
                    node.avg_latency_ms as u32
                ),
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn create_test_node(id: &str, region: &str, max_vms: u32) -> EdgeNode {
        EdgeNode::new(EdgeNodeConfig {
            node_id: id.to_string(),
            address: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 8080),
            region: region.to_string(),
            max_vms,
            health_check_interval_secs: 30,
        })
    }

    #[tokio::test]
    async fn test_node_registration() {
        let orchestrator = EdgeOrchestrator::new("local-node".to_string());
        
        let node = create_test_node("node-1", "us-west-2", 10);
        orchestrator.register_node(node).await;

        let nodes = orchestrator.nodes.read().await;
        assert_eq!(nodes.len(), 1);
        assert!(nodes.contains_key("node-1"));
    }

    #[tokio::test]
    async fn test_node_placement() {
        let orchestrator = EdgeOrchestrator::new("local-node".to_string());
        
        // Register nodes in different regions
        let mut node1 = create_test_node("node-1", "us-west-2", 10);
        node1.available_memory_mb = 4096;
        node1.cpu_utilization = 0.3;
        
        let mut node2 = create_test_node("node-2", "us-east-1", 10);
        node2.available_memory_mb = 8192;
        node2.cpu_utilization = 0.5;

        orchestrator.register_node(node1).await;
        orchestrator.register_node(node2).await;

        // Find best node without region preference
        let best = orchestrator.find_best_node(None).await;
        assert!(best.is_some());
        
        // Find best node with region preference
        let best_west = orchestrator.find_best_node(Some("us-west-2")).await;
        assert!(best_west.is_some());
        assert_eq!(best_west.unwrap().config.node_id, "node-1");
    }

    #[test]
    fn test_placement_score() {
        let mut node = create_test_node("test", "us-west-2", 10);
        
        // Empty node should have good score
        node.current_vms = 0;
        node.available_memory_mb = 8192;
        node.cpu_utilization = 0.1;
        node.avg_latency_ms = 10.0;
        
        let score = node.placement_score();
        assert!(score > 0.5);

        // Full node should have zero score
        node.current_vms = 10;
        assert_eq!(node.placement_score(), 0.0);
    }

    #[tokio::test]
    async fn test_orchestrator_stats() {
        let orchestrator = EdgeOrchestrator::new("local-node".to_string());
        
        orchestrator.register_node(create_test_node("node-1", "us-west-2", 10)).await;
        orchestrator.register_node(create_test_node("node-2", "us-east-1", 20)).await;

        let stats = orchestrator.get_stats().await;
        
        assert_eq!(stats.total_nodes, 2);
        assert_eq!(stats.healthy_nodes, 2);
        assert_eq!(stats.total_vms, 0);
        assert_eq!(stats.total_capacity, 30);
        assert_eq!(stats.regions_count, 2);
    }
}
