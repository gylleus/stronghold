//! A bounded session that maintains the Ledger.

pub mod lookup;
pub mod mcp;
pub mod report;
pub mod session;
pub mod snapshot;

pub const DEFAULT_BUDGET: usize = 40;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    DryRun,
    Live,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interval {
    #[default]
    Daily,
    Weekly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub model: String,
    pub mode: Mode,
    pub enabled: bool,
    pub interval: Interval,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "sonnet".into(),
            mode: Mode::DryRun,
            enabled: false,
            interval: Interval::Daily,
        }
    }
}

impl Settings {
    pub fn period_ms(&self) -> Option<i64> {
        self.enabled.then_some(match self.interval {
            Interval::Daily => 86_400_000,
            Interval::Weekly => 7 * 86_400_000,
        })
    }
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("Missing parent"))?,
    )?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

pub(crate) fn home() -> anyhow::Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_owned())
        .ok_or_else(|| anyhow::anyhow!("Home directory is unavailable"))
}

pub fn run_cli(args: &[String]) -> Option<anyhow::Result<()>> {
    (args.first()?.as_str() == "lorekeeper").then(|| {
        let mut settings = crate::whip::Settings::path()
            .map(|p| crate::whip::Settings::load_from(&p).settings.lorekeeper)
            .unwrap_or_default();
        let mut mode = None;
        let mut budget = DEFAULT_BUDGET;
        let mut rest = args[1..].iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--dry-run" | "--live" => {
                    anyhow::ensure!(mode.is_none(), "Choose only one of --dry-run and --live");
                    mode = Some(if arg == "--live" {
                        Mode::Live
                    } else {
                        Mode::DryRun
                    });
                }
                "--budget" => {
                    budget = rest
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Missing budget"))?
                        .parse()?
                }
                _ => anyhow::bail!(
                    "usage: stronghold lorekeeper [--dry-run|--live] [--budget 1..200]"
                ),
            }
        }
        if let Some(mode) = mode {
            settings.mode = mode;
        }
        let saved = session::launch(&session::Paths::from_env()?, &settings, budget)?;
        println!(
            "Lorekeeper: {}\nSnapshot: {}",
            saved.handle.as_deref().unwrap_or("starting"),
            saved.snapshot.display()
        );
        Ok(())
    })
}
