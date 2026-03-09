//! ISA CLI - Command Line Interface for Instant-State Applications
//!
//! # Usage
//!
//! ```bash
//! isa-cli snapshot --label "my-state" --ttl 24h
//! isa-cli resume --state-id <id> --region us-west-2
//! isa-cli fork --state-id <id> --label "forked"
//! isa-cli status
//! ```

use clap::{Parser, Subcommand};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_HOST: &str = "http://localhost:3000";

#[derive(Parser)]
#[command(name = "isa-cli")]
#[command(author = "ISA Team")]
#[command(version = "0.1.0")]
#[command(about = "Instant-State Applications CLI", long_about = None)]
struct Cli {
    /// API host URL
    #[arg(short, long, default_value = DEFAULT_HOST)]
    host: String,

    /// Request timeout in seconds
    #[arg(short, long, default_value = "30")]
    timeout: u64,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new state snapshot
    Snapshot {
        /// Label for the snapshot
        #[arg(short, long)]
        label: String,

        /// Time-to-live (e.g., "24h", "7d", "3600s")
        #[arg(short, long, default_value = "24h")]
        ttl: String,
    },

    /// Resume a state from snapshot
    Resume {
        /// State ID to resume
        #[arg(short, long)]
        state_id: String,

        /// Resume mode (collaborative, readonly, debug)
        #[arg(short, long, default_value = "collaborative")]
        mode: String,

        /// Target region
        #[arg(short, long, default_value = "nearest")]
        region: String,
    },

    /// Fork an existing state
    Fork {
        /// State ID to fork
        #[arg(short, long)]
        state_id: String,

        /// Label for the forked state
        #[arg(short, long)]
        label: Option<String>,
    },

    /// Get system status
    Status,

    /// List all states
    List {
        /// Filter by label pattern
        #[arg(short, long)]
        filter: Option<String>,
    },

    /// Delete a state
    Delete {
        /// State ID to delete
        #[arg(short, long)]
        state_id: String,

        /// Skip confirmation prompt
        #[arg(short, long, default_value = "false")]
        force: bool,
    },
}

#[derive(Debug, Serialize)]
struct SnapshotRequest {
    label: String,
    ttl: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SnapshotResponse {
    state_id: String,
}

#[derive(Debug, Serialize)]
struct ResumeRequest {
    state_id: String,
    mode: Option<String>,
    region: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResumeResponse {
    accepted: bool,
    target_region: String,
    mode: String,
}

#[derive(Debug, Serialize)]
struct ForkRequest {
    state_id: String,
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ForkResponse {
    state_id: String,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    status: String,
    stored_states: usize,
    log_entries: usize,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let client = Client::builder()
        .timeout(Duration::from_secs(cli.timeout))
        .build()?;

    let result = match cli.command {
        Commands::Snapshot { label, ttl } => {
            cmd_snapshot(&client, &cli.host, label, ttl).await
        }
        Commands::Resume { state_id, mode, region } => {
            cmd_resume(&client, &cli.host, state_id, mode, region).await
        }
        Commands::Fork { state_id, label } => {
            cmd_fork(&client, &cli.host, state_id, label).await
        }
        Commands::Status => cmd_status(&client, &cli.host).await,
        Commands::List { filter } => cmd_list(&client, &cli.host, filter).await,
        Commands::Delete { state_id, force } => {
            cmd_delete(&client, &cli.host, state_id, force).await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    Ok(())
}

async fn cmd_snapshot(
    client: &Client,
    host: &str,
    label: String,
    ttl: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let req = SnapshotRequest {
        label,
        ttl: Some(ttl),
    };

    let resp = client
        .post(format!("{}/v1/snapshot", host))
        .json(&req)
        .send()
        .await?;

    if resp.status().is_success() {
        let body: SnapshotResponse = resp.json().await?;
        println!("✓ Snapshot created");
        println!("  State ID: {}", body.state_id);
    } else {
        let error: ErrorResponse = resp.json().await?;
        return Err(format!("Failed to create snapshot: {}", error.error).into());
    }

    Ok(())
}

async fn cmd_resume(
    client: &Client,
    host: &str,
    state_id: String,
    mode: String,
    region: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let req = ResumeRequest {
        state_id,
        mode: Some(mode),
        region: Some(region),
    };

    let resp = client
        .post(format!("{}/v1/resume", host))
        .json(&req)
        .send()
        .await?;

    if resp.status().is_success() {
        let body: ResumeResponse = resp.json().await?;
        println!("✓ State resumed");
        println!("  Region: {}", body.target_region);
        println!("  Mode: {}", body.mode);
    } else {
        let error: ErrorResponse = resp.json().await?;
        return Err(format!("Failed to resume state: {}", error.error).into());
    }

    Ok(())
}

async fn cmd_fork(
    client: &Client,
    host: &str,
    state_id: String,
    label: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let req = ForkRequest { state_id, label };

    let resp = client
        .post(format!("{}/v1/fork", host))
        .json(&req)
        .send()
        .await?;

    if resp.status().is_success() {
        let body: ForkResponse = resp.json().await?;
        println!("✓ State forked");
        println!("  New State ID: {}", body.state_id);
    } else {
        let error: ErrorResponse = resp.json().await?;
        return Err(format!("Failed to fork state: {}", error.error).into());
    }

    Ok(())
}

async fn cmd_status(
    client: &Client,
    host: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let resp = client
        .post(format!("{}/v1/status", host))
        .send()
        .await?;

    if resp.status().is_success() {
        let body: StatusResponse = resp.json().await?;
        println!("ISA Workspace Status");
        println!("  Status: {}", body.status);
        println!("  Stored States: {}", body.stored_states);
        println!("  Log Entries: {}", body.log_entries);
    } else {
        let error: ErrorResponse = resp.json().await?;
        return Err(format!("Failed to get status: {}", error.error).into());
    }

    Ok(())
}

async fn cmd_list(
    client: &Client,
    host: &str,
    filter: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Get status which includes state count
    let resp = client
        .get(format!("{}/v1/status", host))
        .send()
        .await?;

    if resp.status().is_success() {
        let body: serde_json::Value = resp.json().await?;
        println!("ISA Workspace States");
        println!("  Status: {}", body.get("status").unwrap_or(&serde_json::json!("unknown")));
        println!("  Stored States: {}", body.get("stored_states").unwrap_or(&serde_json::json!(0)));
        
        if let Some(pattern) = filter {
            println!("  Filter: {}", pattern);
        }
    } else {
        let error: ErrorResponse = resp.json().await?;
        return Err(format!("Failed to list states: {}", error.error).into());
    }

    Ok(())
}

async fn cmd_delete(
    client: &Client,
    host: &str,
    state_id: String,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !force {
        print!("Delete state {}? [y/N] ", state_id);
        use std::io::{self, Write};
        io::stdout().flush()?;
        
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled");
            return Ok(());
        }
    }

    // Use POST to /v1/delete with state_id in body
    let resp = client
        .post(format!("{}/v1/delete", host))
        .json(&serde_json::json!({ "state_id": state_id }))
        .send()
        .await?;

    if resp.status().is_success() {
        println!("✓ State deleted successfully");
    } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("State not found");
    } else if resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
        // Fallback message if endpoint doesn't exist
        println!("Note: Delete endpoint not available on this server");
        println!("Delete state manually from the state store");
    } else {
        let error: ErrorResponse = resp.json().await?;
        return Err(format!("Failed to delete state: {}", error.error).into());
    }

    Ok(())
}
