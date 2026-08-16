use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use forge_accounting::{
    AccountingCoverage, enrich_codex_evidence, read_enrichment_jsonl, write_enrichment_jsonl,
};

#[derive(Debug, Parser)]
#[command(name = "forge-accounting")]
#[command(about = "Offline accounting enrichment for preserved Forge evidence")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Enrich one preserved Codex run without modifying its export or ledger.
    EnrichCodex {
        #[arg(long)]
        environment: PathBuf,
        #[arg(long)]
        export: PathBuf,
        #[arg(long)]
        agent_log: PathBuf,
        /// Matching Codex rollout JSONL. Without it, actual model identity may
        /// remain unknown unless the Forge export explicitly pinned a model.
        #[arg(long)]
        session_log: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
    /// Summarize field coverage for one or more enrichment JSONL artifacts.
    Coverage {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::EnrichCodex {
            environment,
            export,
            agent_log,
            session_log,
            output,
        } => {
            let record =
                enrich_codex_evidence(&environment, &export, &agent_log, session_log.as_deref())
                    .context("could not enrich Codex accounting evidence")?;
            write_enrichment_jsonl(&output, &record)
                .context("could not write accounting enrichment")?;
            println!("wrote {}", output.display());
        }
        Command::Coverage { inputs } => {
            let mut records = Vec::new();
            for input in inputs {
                records.extend(
                    read_enrichment_jsonl(&input)
                        .with_context(|| format!("could not read {}", input.display()))?,
                );
            }
            let coverage = AccountingCoverage::from_records(&records);
            println!("Codex accounting coverage");
            row("runs", coverage.runs);
            row("model known", coverage.model_known);
            row(
                "input/output tokens known",
                coverage.input_output_tokens_known,
            );
            row("cached input known", coverage.cached_input_known);
            row("provider credits known", coverage.provider_credits_known);
            row("derived credits", coverage.derived_credits_known);
            row(
                "credit-equivalent USD",
                coverage.credit_equivalent_usd_known,
            );
            row(
                "known billed USD",
                coverage.provider_reported_cost_usd_known,
            );
        }
    }
    Ok(())
}

fn row(label: &str, value: u64) {
    println!("  {label:<30} {value:>6}");
}
