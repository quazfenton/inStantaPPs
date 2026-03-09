//! ISA API Server Binary
//!
//! Starts the HTTP API server for the Instant-State Applications platform.
//!
//! # Usage
//!
//! ```bash
//! cargo run -- --port 3000 --config config.toml
//! ```
//!
//! # Environment Variables
//!
//! - `ISA_API_PORT`: Port to bind (default: 3000)
//! - `ISA_API_HOST`: Host to bind (default: 0.0.0.0)
//! - `ISA_WORKSPACE`: Base directory for VM artifacts
//! - `ISA_STATE_STORE`: State storage directory
//! - `RUST_LOG`: Logging level (default: info)

use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::{self, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args = std::env::args().collect::<Vec<_>>();
    let mut config_path: Option<PathBuf> = None;
    let mut port: Option<u16> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                if i + 1 < args.len() {
                    config_path = Some(PathBuf::from(&args[i + 1]));
                    i += 1;
                }
            }
            "--port" | "-p" => {
                if i + 1 < args.len() {
                    port = args[i + 1].parse().ok();
                    i += 1;
                }
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }

    // Load configuration
    let config = isa_workspace::config::Config::load(config_path.as_deref());

    // Override port if specified
    let config = if let Some(p) = port {
        isa_workspace::config::Config {
            server: isa_workspace::config::ServerConfig {
                port: p,
                ..config.server
            },
            ..config
        }
    } else {
        config
    };

    // Initialize logging from config
    let log_level = &config.logging.level;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(format!("isa_workspace={}", log_level).parse()?)
                .add_directive("tokio=warn".parse()?)
                .add_directive("h2=warn".parse()?)
                .add_directive("quinn=warn".parse()?)
        )
        .init();

    let addr = config.api_address();

    info!("╔═══════════════════════════════════════════════════════════╗");
    info!("║     ISA Workspace - Instant-State Applications Platform   ║");
    info!("╠═══════════════════════════════════════════════════════════╣");
    info!("║  Version: {:<52} ║", isa_workspace::VERSION);
    info!("╚═══════════════════════════════════════════════════════════╝");
    info!("");
    info!("Configuration:");
    info!("  API Address: {}", addr);
    info!("  Workspace: {:?}", config.firecracker.workspace_root);
    info!("  State Store: {:?}", config.state_store.storage_path);
    info!("  QUIC Port: {}", config.quic.port);
    info!("  Log Level: {}", config.logging.level);
    info!("");
    info!("Endpoints:");
    info!("  POST /v1/snapshot - Create state snapshot");
    info!("  POST /v1/resume   - Resume from snapshot");
    info!("  POST /v1/fork     - Fork existing state");
    info!("  POST /v1/status   - System health check");
    info!("");
    info!("CLI Usage:");
    info!("  isa-cli snapshot --label \"my-state\" --ttl 24h");
    info!("  isa-cli resume --state-id <id> --region us-west-2");
    info!("  isa-cli status");
    info!("");

    isa_workspace::api::serve(&addr).await?;

    Ok(())
}

fn print_help() {
    println!("ISA Workspace - Instant-State Applications Server");
    println!();
    println!("USAGE:");
    println!("    isa-server [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -c, --config <FILE>    Configuration file path [default: config.toml]");
    println!("    -p, --port <PORT>      API server port [default: 3000]");
    println!("    -h, --help             Print help information");
    println!();
    println!("ENVIRONMENT VARIABLES:");
    println!("    ISA_API_PORT           API server port");
    println!("    ISA_API_HOST           API server host");
    println!("    ISA_WORKSPACE          Base directory for VM artifacts");
    println!("    ISA_STATE_STORE        State storage directory");
    println!("    RUST_LOG               Logging level (info, debug, warn, error)");
    println!();
    println!("EXAMPLES:");
    println!("    isa-server");
    println!("    isa-server --port 8080");
    println!("    isa-server --config /etc/isa/config.toml");
    println!("    RUST_LOG=debug isa-server");
}
