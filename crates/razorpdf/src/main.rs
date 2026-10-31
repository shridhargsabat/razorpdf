//! `razorpdf` CLI: parse, inspect, bench, and serve MCP tools.

mod commands;
mod mcp_server;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "razorpdf",
    version,
    about = "Ultra-fast local-first document parser"
)]
struct Cli {
    /// Start the MCP stdio server (same as the `mcp` subcommand).
    #[arg(long)]
    mcp: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Parse a PDF into Markdown (and optional grounded JSON).
    Parse(ParseArgs),
    /// Print per-page layout stats and triage decisions.
    Inspect(InspectArgs),
    /// Benchmark fast-path throughput on a synthetic multi-page document.
    Bench(BenchArgs),
    /// Start the MCP JSON-RPC server over stdin/stdout.
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
    /// VLM model id (overrides RAZORPDF_VLM_MODEL / default qwen2-vl-7b-instruct).
    #[arg(long)]
    vlm_model: Option<String>,
    /// Resolve VLM regions with the deterministic offline mock provider.
    #[arg(long)]
    mock_vlm: bool,
    /// Page-canvas source for VLM crops: synthetic (default) or auto
    /// (`pdftoppm` when present on PATH, else synthetic).
    #[arg(long, value_parser = ["synthetic", "auto"])]
    canvas_source: Option<String>,
}

#[derive(clap::Args)]
struct InspectArgs {
    /// Input PDF path.
    input: String,
}

#[derive(clap::Args)]
struct BenchArgs {
    /// Synthetic document pages (two-col/single/bordered mix). Default 400:
    /// a ~30 ms window where engine throughput dominates host noise (a
    /// 100-page/8 ms sample is mostly scheduling jitter on shared hosts).
    #[arg(long, default_value_t = 400)]
    pages: usize,
    /// Emit machine-readable JSON instead of the human table.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    if cli.mcp {
        return mcp_server::run().await;
    }
    match cli.command {
        Some(Commands::Parse(a)) => {
            commands::parse(
                &a.input,
                a.output.as_deref(),
                a.json.as_deref(),
                a.vlm_url.as_deref(),
                a.vlm_model.as_deref(),
                a.mock_vlm,
                a.canvas_source.as_deref(),
            )
            .await
        }
        Some(Commands::Inspect(a)) => commands::inspect(&a.input),
        Some(Commands::Bench(a)) => commands::bench(a.pages, a.json).await,
        Some(Commands::Mcp) => mcp_server::run().await,
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}
