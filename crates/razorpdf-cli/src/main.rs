//! `razorpdf` CLI: parse, inspect, and serve MCP tools.

mod commands;
mod mcp_server;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "razorpdf",
    version,
    about = "Ultra-fast local-first document parser"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Parse a PDF into Markdown (and optional grounded JSON).
    Parse(ParseArgs),
    /// Print per-page layout stats and triage decisions.
    Inspect(InspectArgs),
    /// Start the MCP JSON-RPC server over stdin/stdout (Phase 5).
    Mcp,
}

#[derive(clap::Args)]
struct ParseArgs {
    /// Input PDF path.
    input: String,
    /// Markdown output path (stdout when omitted).
    #[arg(short, long)]
    output: Option<String>,
    /// Grounded JSON output path.
    #[arg(long)]
    json: Option<String>,
    /// VLM endpoint URL (fast path needs none).
    #[arg(long)]
    vlm_url: Option<String>,
    /// Resolve VLM regions with the deterministic offline mock provider.
    #[arg(long)]
    mock_vlm: bool,
}

#[derive(clap::Args)]
struct InspectArgs {
    /// Input PDF path.
    input: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    match Cli::parse().command {
        Commands::Parse(a) => {
            commands::parse(
                &a.input,
                a.output.as_deref(),
                a.json.as_deref(),
                a.vlm_url.as_deref(),
                a.mock_vlm,
            )
            .await
        }
        Commands::Inspect(a) => commands::inspect(&a.input),
        Commands::Mcp => mcp_server::run().await,
    }
}
