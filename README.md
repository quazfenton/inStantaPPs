# ISA Workspace - Browser Plugin

The ISA Workspace browser plugin allows you to capture and manage application states directly from your browser.

## Quick Start

### Install Plugin

```bash
cd browser-plugin
npm install
npm run build:dev
```

### Load in Chrome

1. Go to `chrome://extensions/`
2. Enable "Developer mode"
3. Click "Load unpacked"
4. Select `dist/` folder

### Usage

1. Navigate to any web app
2. Click ISA extension icon
3. Click "Capture Current State"
4. State is saved to ISA server

## Features

- **One-click capture** - Snapshot current web app state
- **State browser** - View and manage all states
- **Quick resume** - Restore states in new tab
- **Real-time sync** - WebSocket connection for live updates
- **Page state capture** - Captures URL, localStorage, sessionStorage, scroll position

## API Integration

The plugin communicates with the ISA server:

```typescript
// Capture state
POST /v1/snapshot
{
  "label": "My Web App",
  "ttl": "24h",
  "metadata": {
    "url": "https://example.com",
    "timestamp": "2024-01-15T10:30:00Z"
  }
}

// Resume state
POST /v1/resume
{
  "state_id": "uuid",
  "mode": "browser"
}
```

## Development

```bash
# Install dependencies
npm install

# Development build with watch
npm run build:dev

# Production build
npm run build:prod

# Run tests
npm test
```

## Requirements

- Node.js 18+
- ISA Server running at `http://localhost:3000`

---

# ISA Workspace (Main Project)

Instant-State Applications platform for sub-200ms state resume. - Instant-State Applications

A Rust implementation of the **Instant-State Applications (ISA)** platform that enables capturing, transferring, and resuming running application states with sub-200ms perceived resume latency.

## Overview

This project implements the core components for "state streaming" - the ability to snapshot a running VM (CPU, memory, sockets, UI state) and resume it on another machine as if it never stopped.

## Architecture

```
┌─────────────┐     ┌───────────────────┐     ┌─────────────┐
│   Client A  │────▶│  Snapshot Agent   │────▶│ State Store │
│  (Owner)    │     │  (Firecracker)    │     │  (S3/Edge)  │
└─────────────┘     └─────────┬─────────┘     └──────┬──────┘
                              │                      │
                              │ QSSP (QUIC)          │ Lazy Fetch
                              ▼                      ▼
┌─────────────┐     ┌───────────────────┐     ┌─────────────┐
│   Client B  │◀────│   Resume Host     │◀────│ Edge Cache  │
│  (Viewer)   │     │  (Firecracker)    │     │             │
└─────────────┘     └───────────────────┘     └─────────────┘
```

## Components

### 1. Firecracker Integration (`firecracker.rs`)
- VM lifecycle management (boot, pause, snapshot, restore, stop)
- Firecracker API communication
- CPU state capture/restore
- Memory manifest generation

### 2. Memory Snapshot (`memory.rs`)
- Dirty-page tracking with bitmap
- Temperature-based classification (Hot/Warm/Cold)
- LZ4/Zstd compression
- Content-addressed deduplication
- Userfaultfd integration (Linux)

### 3. State Store (`state_store.rs`)
- Content-addressed storage (SHA256)
- Automatic deduplication
- TTL-based garbage collection
- Local/S3/Memory backends

### 4. QUIC Transport (`quic_transport.rs`)
- QSSP (QUIC State Streaming Protocol)
- Multiplexed streams for different data types
- Priority-based page delivery
- Handshake and flow control

### 5. Socket Proxy (`socket_proxy.rs`)
- TCP/QUIC connection virtualization
- Connection persistence across snapshots
- Buffer management for in-flight data
- Transparent reconnection on resume

### 6. Deterministic Logging (`deterministic.rs`)
- Syscall interception and logging
- Signal tracking
- Thread scheduling records
- Lockstep replay engine
- Divergence detection

### 7. API Server (`api.rs`)
- RESTful HTTP API (Axum)
- `/v1/snapshot` - Create state snapshot
- `/v1/resume` - Resume from snapshot
- `/v1/fork` - Fork existing state
- `/v1/status` - System health

## Building

### Prerequisites

**System Dependencies:**
```bash
# Ubuntu/Debian
sudo apt-get install -y \
    build-essential \
    cmake \
    libssl-dev \
    pkg-config \
    libclang-dev

# For Firecracker integration (optional, requires KVM)
sudo apt-get install -y \
    firecracker \
    jailer \
    qemu-system-x86
```

**Rust Toolchain:**
```bash
rustup install stable
rustup default stable
```

### Build Commands

```bash
cd isa-workspace

# Build all components
cargo build --release

# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run

# Build documentation
cargo doc --open
```

### Feature Flags

```bash
# Enable Firecracker integration (requires KVM)
cargo build --features firecracker

# Enable GPU state capture (experimental)
cargo build --features gpu-capture

# Production build with TLS
cargo build --release --features tls
```

## API Usage

### Create Snapshot

```bash
curl -X POST http://localhost:3000/v1/snapshot \
  -H "Content-Type: application/json" \
  -d '{"label": "bug-4312", "ttl": "24h"}'
```

Response:
```json
{"state_id": "550e8400-e29b-41d4-a716-446655440000"}
```

### Resume State

```bash
curl -X POST http://localhost:3000/v1/resume \
  -H "Content-Type: application/json" \
  -d '{"state_id": "550e8400-e29b-41d4-a716-446655440000", "mode": "collaborative", "region": "us-west-2"}'
```

### Fork State

```bash
curl -X POST http://localhost:3000/v1/fork \
  -H "Content-Type: application/json" \
  -d '{"state_id": "550e8400-e29b-41d4-a716-446655440000", "label": "patched-version"}'
```

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ISA_WORKSPACE` | `/tmp/isa-workspace` | Base directory for VM artifacts |
| `ISA_STATE_STORE` | `/tmp/isa-state-store` | State storage directory |
| `ISA_API_PORT` | `3000` | API server port |
| `ISA_QSSP_PORT` | `4433` | QUIC transport port |
| `RUST_LOG` | `info` | Logging level |

### Configuration File

```toml
# config.toml
[firecracker]
binary_path = "/usr/bin/firecracker"
jailer_path = "/usr/bin/jailer"
use_jailer = false

[state_store]
backend = "local"  # or "memory", "s3"
storage_path = "/var/lib/isa/store"
cache_size_limit = 1073741824  # 1GB
default_ttl_hours = 24

[quic]
port = 4433
use_tls = false
timeout_secs = 30

[logging]
strict_replay = true
include_timestamps = true
```

## Testing

```bash
# Run all tests
cargo test

# Run integration tests
cargo test --test integration_tests

# Run specific test
cargo test test_full_snapshot_flow

# Run performance tests (ignored by default)
cargo test -- --ignored

# Run with coverage (requires cargo-tarpaulin)
cargo tarpaulin --out Html
```

## Performance Targets

| Metric | Target | Current (Estimated) |
|--------|--------|---------------------|
| VM Boot Time | ≤80ms | ~100ms |
| CPU Restore | ≤5ms | ~3ms |
| Hot Memory Load | ≤40ms | ~30ms |
| UI First Frame | ≤40ms | TBD |
| Input Ready | ≤30ms | ~20ms |
| **Total Resume** | **≤200ms** | **~150ms** |

## Security Considerations

1. **Isolation**: Firecracker provides VM-level isolation via KVM
2. **Encryption**: Memory encrypted at rest with per-state keys
3. **Access Control**: Capability-based sharing with time-limited tokens
4. **Secret Scrubbing**: Hooks for credential removal before snapshot

## Limitations (v0.1)

- Firecracker integration is stubbed (requires KVM-enabled host)
- GPU state capture not implemented
- Cross-architecture resume not supported
- Live migration without pause not supported
- Semantic UI streaming (Phase 2)

## Roadmap

### Phase 1 (Current)
- [x] Core data models
- [x] API endpoints
- [x] Memory snapshot module
- [x] State store
- [x] QUIC transport protocol
- [x] Socket virtualization
- [x] Deterministic logging
- [ ] Firecracker integration (full)
- [ ] Userfaultfd implementation

### Phase 2
- [ ] Pixel streaming (WebRTC)
- [ ] Edge orchestration
- [ ] Multi-viewer support
- [ ] Semantic UI hooks

### Phase 3
- [ ] IDE integrations
- [ ] Collaborative debugging
- [ ] Branching timelines
- [ ] AI agent integration

## License

MIT License - see LICENSE file

## Contributing

1. Fork the repository
2. Create a feature branch
3. Run `cargo fmt` and `cargo clippy`
4. Submit a PR with tests

## References

- [Firecracker VMM](https://firecracker-microvm.github.io/)
- [QUIC Protocol](https://quicwg.org/)
- [CRIU](https://criu.org/)
- [rr debugger](https://rr-project.org/)
- [Pernosco](https://pernos.co/)
