//! The clan's shared folder outside the app's data dir: the Ledger, worktree archives and saved
//! notes live under it, and the playbook falls back to it.

use std::path::PathBuf;

/// Overrides [`agents_dir`].
pub const AGENTS_DIR_ENV: &str = "STRONGHOLD_AGENTS_DIR";

/// `$STRONGHOLD_AGENTS_DIR`, else `~/Documents/agents`.
pub fn agents_dir() -> Option<PathBuf> {
    agents_dir_from(std::env::var_os(AGENTS_DIR_ENV).map(PathBuf::from), home())
}

fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

fn agents_dir_from(env: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    env.filter(|d| !d.as_os_str().is_empty())
        .or_else(|| home.map(|h| h.join("Documents").join("agents")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_var_wins_over_the_home_default() {
        let home = Some(PathBuf::from("/h"));
        assert_eq!(
            agents_dir_from(Some("/x".into()), home.clone()),
            Some("/x".into())
        );
        assert_eq!(
            agents_dir_from(Some("".into()), home.clone()),
            Some("/h/Documents/agents".into())
        );
        assert_eq!(
            agents_dir_from(None, home),
            Some("/h/Documents/agents".into())
        );
        assert_eq!(agents_dir_from(None, None), None);
    }
}
