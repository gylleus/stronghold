//! Read-only live worktree plan. Set STRONGHOLD_DATA_DIR to a scratch directory before use:
//! `STRONGHOLD_DATA_DIR=/tmp/stronghold-plan cargo run --example worktree_plan`.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use stronghold::orca::OrcaClient;
use stronghold::worktrees;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("STRONGHOLD_DATA_DIR").is_none_or(|dir| dir.is_empty()) {
        return Err(
            "Set STRONGHOLD_DATA_DIR to a scratch directory before running this example".into(),
        );
    }
    let now_ms = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let client = OrcaClient::new().without_sender_identity();
    let plan = worktrees::evaluate(&client, &HashMap::new(), now_ms)?;
    println!("{}", plan.text());
    println!(
        "Counts: evaluated={} / removable={} / kept={} / unlisted={} / orphans={}",
        plan.entries.len(),
        plan.removable().count(),
        plan.kept().count(),
        plan.entries.iter().filter(|entry| entry.unlisted).count(),
        plan.orphans.len(),
    );
    Ok(())
}
