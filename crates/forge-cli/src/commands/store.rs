//! Supported operational lifecycle for the SQLite experience ledger.

use std::path::PathBuf;

use anyhow::{Context, Result};
use forge_store::Store;

use crate::commands::run::resolve_repository;
use crate::output;

pub async fn backup(repo: Option<PathBuf>, output_path: PathBuf) -> Result<()> {
    let (_, layout, config) = resolve_repository(repo.as_deref())?;
    let source = layout.store_path(&config);
    let store = Store::open(&source)
        .await
        .with_context(|| format!("opening the ledger at {}", source.display()))?;
    let verification = store
        .backup_to(&output_path)
        .await
        .with_context(|| format!("backing up the ledger to {}", output_path.display()))?;
    println!(
        "Store backup complete\n\n{}",
        output::fields(&[
            ("Source", source.display().to_string()),
            ("Backup", output_path.display().to_string()),
            ("Migration", verification.migration_version.to_string()),
            ("Runs", verification.run_count.to_string()),
            ("Integrity", "ok".into()),
        ])
    );
    Ok(())
}

pub async fn verify(repo: Option<PathBuf>, path: Option<PathBuf>) -> Result<()> {
    let target = match path {
        Some(path) => path,
        None => {
            let (_, layout, config) = resolve_repository(repo.as_deref())?;
            layout.store_path(&config)
        }
    };
    let verification = Store::verify_file(&target)
        .await
        .with_context(|| format!("verifying store {}", target.display()))?;
    if !verification.is_usable() {
        anyhow::bail!("store verification failed: {verification:?}");
    }
    println!(
        "Store verified\n\n{}",
        output::fields(&[
            ("Path", target.display().to_string()),
            ("Integrity", "ok".into()),
            (
                "Foreign key violations",
                verification.foreign_key_violations.to_string(),
            ),
            ("Migration", verification.migration_version.to_string()),
            ("Runs", verification.run_count.to_string()),
        ])
    );
    Ok(())
}

pub async fn restore(repo: Option<PathBuf>, source: PathBuf, force: bool) -> Result<()> {
    let (_, layout, config) = resolve_repository(repo.as_deref())?;
    let destination = layout.store_path(&config);
    let verification = Store::restore_from(&source, &destination, force)
        .await
        .with_context(|| {
            format!(
                "restoring backup {} to {}",
                source.display(),
                destination.display()
            )
        })?;
    println!(
        "Store restore complete\n\n{}",
        output::fields(&[
            ("Backup", source.display().to_string()),
            ("Restored store", destination.display().to_string()),
            ("Migration", verification.migration_version.to_string()),
            ("Runs", verification.run_count.to_string()),
            ("Integrity", "ok".into()),
        ])
    );
    Ok(())
}
