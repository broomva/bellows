//! `bellows` — command-line interface.
//!
//! v0.1 wires three subcommands:
//! - `bellows version` — print version + license + repo
//! - `bellows doctor`  — diagnostics: Rust toolchain, runtime hooks, sandbox availability
//! - `bellows new <name>` — scaffold a new agent workspace (lands v0.2)
//!
//! `bellows build` and `bellows run` (the codegen + cargo orchestration that
//! produces `dist/<name>` artifacts) ship in v0.2 once `bellows-build` is
//! reified. The CLI is structured so that adding a subcommand is local —
//! see `commands/` for the dispatch table.

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "bellows",
    version,
    about = "Bellows — the Rust agent-harness framework.",
    long_about = None,
)]
struct Cli {
    /// Increase log verbosity. Repeat for more (-v, -vv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print version and build metadata.
    Version,
    /// Diagnose the local environment for Bellows readiness.
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::new(level);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .compact()
        .init();

    match cli.cmd {
        Cmd::Version => cmd_version(),
        Cmd::Doctor => cmd_doctor(),
    }
}

fn cmd_version() -> Result<()> {
    println!("bellows {}", env!("CARGO_PKG_VERSION"));
    println!("license: Apache-2.0");
    println!("repo:    https://github.com/broomva/bellows");
    Ok(())
}

fn cmd_doctor() -> Result<()> {
    println!("bellows doctor");
    println!("  rustc:   {}", rustc_version());
    println!("  default sandbox: bellows-sandbox-local (subprocess)");
    println!("  status: bellows-cli is v0.1 — `build`/`run` arrive in v0.2");
    Ok(())
}

fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string())
}
