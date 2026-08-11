//! `forge agent list` — what Forge knows about, and what it can actually run.

use anyhow::Result;
use forge_agent::AgentRegistry;
use forge_core::agent::AdapterStatus;

use crate::output;

pub fn list() -> Result<()> {
    let registry = AgentRegistry::builtin();

    let rows: Vec<Vec<String>> = registry
        .descriptors()
        .iter()
        .map(|descriptor| {
            let availability = registry.availability(descriptor);
            vec![
                descriptor.agent_id.to_string(),
                descriptor.display_name.clone(),
                descriptor.harness.clone(),
                match descriptor.adapter_status {
                    AdapterStatus::Implemented => "ready".to_string(),
                    AdapterStatus::Planned => "not implemented".to_string(),
                },
                match (&availability.executable_path, &availability.executable) {
                    (Some(path), _) => format!("found ({})", path.display()),
                    (None, Some(exe)) => format!("`{exe}` not on PATH"),
                    (None, None) => "n/a".to_string(),
                },
            ]
        })
        .collect();

    println!(
        "{}",
        output::table(&["agent", "name", "harness", "adapter", "cli"], &rows)
    );

    let runnable = registry
        .descriptors()
        .iter()
        .filter(|d| registry.availability(d).is_runnable())
        .count();

    if runnable == 0 {
        println!(
            "\nNo adapters are implemented yet. Forge currently provides the agent\n\
             interface and the isolation, evaluation, and ledger machinery beneath it;\n\
             `forge run` arrives with the first adapter."
        );
    }

    Ok(())
}
