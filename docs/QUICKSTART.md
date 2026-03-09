# Quick Start Guide

## Getting Started with ISA Workspace

This guide will help you get up and running with the Instant-State Applications platform.

---

## Prerequisites

### System Requirements

- **OS:** Linux (Ubuntu 20.04+ recommended)
- **RAM:** 4GB minimum, 8GB recommended
- **Storage:** 1GB free space
- **Network:** Broadband connection

### Install Dependencies

```bash
# Update package list
sudo apt-get update

# Install build tools
sudo apt-get install -y \
    build-essential \
    cmake \
    pkg-config \
    libssl-dev \
    libclang-dev \
    git \
    curl

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Verify Rust installation
rustc --version
cargo --version
```

---

## Building from Source

### 1. Clone the Repository

```bash
cd /root/code/instanCe
```

### 2. Build the Project

```bash
cd isa-workspace

# Debug build (faster, larger binary)
cargo build

# Release build (slower, optimized binary)
cargo build --release
```

### 3. Run Tests

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture
```

---

## Running the Server

### Quick Start

```bash
# Start with default settings (port 3000)
cargo run

# Or use the binary directly (after build)
./target/debug/isa-server
```

### Custom Configuration

```bash
# Custom port
cargo run -- --port 8080

# With config file
cargo run -- --config config.toml

# With debug logging
RUST_LOG=debug cargo run
```

### Environment Variables

```bash
# Set API port
export ISA_API_PORT=8080

# Set workspace directory
export ISA_WORKSPACE=/tmp/isa

# Set log level
export RUST_LOG=info

# Start server
cargo run
```

---

## Using the CLI

### Build CLI

```bash
cargo build --bin isa-cli
```

### Basic Commands

```bash
# Create a snapshot
./target/debug/isa-cli snapshot --label "my-app" --ttl 24h

# Resume a snapshot
./target/debug/isa-cli resume --state-id <state_id>

# Fork a snapshot
./target/debug/isa-cli fork --state-id <state_id> --label "my-fork"

# Check status
./target/debug/isa-cli status
```

### CLI Help

```bash
./target/debug/isa-cli --help
./target/debug/isa-cli snapshot --help
```

---

## API Reference

### Create Snapshot

```bash
curl -X POST http://localhost:3000/v1/snapshot \
  -H "Content-Type: application/json" \
  -d '{
    "label": "my-app",
    "ttl": "24h"
  }'
```

Response:
```json
{"state_id": "550e8400-e29b-41d4-a716-446655440000"}
```

### Resume State

```bash
curl -X POST http://localhost:3000/v1/resume \
  -H "Content-Type: application/json" \
  -d '{
    "state_id": "550e8400-e29b-41d4-a716-446655440000",
    "mode": "collaborative",
    "region": "us-west-2"
  }'
```

### Fork State

```bash
curl -X POST http://localhost:3000/v1/fork \
  -H "Content-Type: application/json" \
  -d '{
    "state_id": "550e8400-e29b-41d4-a716-446655440000",
    "label": "patched-version"
  }'
```

### Health Check

```bash
curl http://localhost:3000/health
```

### Prometheus Metrics

```bash
curl http://localhost:3000/metrics
```

---

## Configuration

### Create Config File

```bash
# Copy example config
cp config.example.toml config.toml

# Edit configuration
nano config.toml
```

### Example Configuration

```toml
[server]
host = "0.0.0.0"
port = 3000
workers = 4

[firecracker]
binary_path = "/usr/bin/firecracker"
workspace_root = "/tmp/isa-workspace"
use_jailer = false

[state_store]
backend = "local"
storage_path = "/var/lib/isa/store"
cache_size_mb = 1024
default_ttl_hours = 24

[quic]
port = 4433
use_tls = false
timeout_secs = 30

[logging]
level = "info"
format = "full"
```

---

## Monitoring

### Check Server Health

```bash
curl http://localhost:3000/health | jq
```

### View Metrics

```bash
# Text format
curl http://localhost:3000/metrics

# With Prometheus
promtool check metrics <(curl -s http://localhost:3000/metrics)
```

### Grafana Dashboard

Import the provided dashboard JSON into Grafana for visualization.

---

## Troubleshooting

### Build Errors

```bash
# Clean and rebuild
cargo clean
cargo build

# Update dependencies
cargo update
```

### Port Already in Use

```bash
# Find process using port 3000
lsof -i :3000

# Kill the process
kill -9 <PID>

# Or use a different port
cargo run -- --port 8080
```

### Permission Denied

```bash
# For Firecracker (requires KVM)
sudo usermod -aG kvm $USER
sudo usermod -aG firecracker $USER

# Log out and back in
```

### Logs

```bash
# Enable debug logging
RUST_LOG=debug cargo run

# Log to file
RUST_LOG=info cargo run 2>&1 | tee isa.log
```

---

## Next Steps

1. **Read the Documentation:**
   - [README.md](README.md) - Overview and features
   - [PROJECT_OVERVIEW.md](PROJECT_OVERVIEW.md) - Complete architecture
   - [IMPLEMENTATION.md](IMPLEMENTATION.md) - Technical details

2. **Explore the API:**
   - Try all endpoints with curl or Postman
   - Set up Prometheus + Grafana for monitoring

3. **Production Setup:**
   - Configure Firecracker with KVM
   - Set up TLS certificates
   - Deploy to Kubernetes cluster

4. **Contribute:**
   - Fork the repository
   - Create a feature branch
   - Submit a PR

---

## Getting Help

- **Documentation:** See `docs/` directory
- **Issues:** GitHub Issues
- **Discussions:** GitHub Discussions

---

## Quick Reference Card

```bash
# Server
cargo run -- --port 3000
RUST_LOG=debug cargo run

# CLI
isa-cli snapshot --label "app" --ttl 24h
isa-cli resume --state-id <id>
isa-cli status

# API
curl http://localhost:3000/health
curl http://localhost:3000/metrics

# Tests
cargo test
cargo test -- --nocapture
```
