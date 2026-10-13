//! What is kept from the app's first name, Myrstack (it became Stronghold on 2026-10-13), so an
//! existing install and a running clan carry over: `MYRSTACK_*` environment variables are still
//! read, and the old data folder is moved to the new place once.

use std::ffi::OsString;
use std::io;
use std::path::Path;

/// The app's name before the rename; also the name of its old data folder.
pub const OLD_APP_NAME: &str = "Myrstack";
/// Prefix of the app's environment variables.
pub const ENV_PREFIX: &str = "STRONGHOLD_";
/// Their prefix before the rename.
pub const OLD_ENV_PREFIX: &str = "MYRSTACK_";

/// The spelling of app variable `key` (a `STRONGHOLD_*` name) before the rename.
fn old_env_key(key: &str) -> Option<String> {
    key.strip_prefix(ENV_PREFIX)
        .map(|rest| format!("{OLD_ENV_PREFIX}{rest}"))
}

/// App variable `key` (a `STRONGHOLD_*` name), or its `MYRSTACK_*` spelling when only that is
/// set.
pub fn env_os(key: &str) -> Option<OsString> {
    std::env::var_os(key).or_else(|| old_env_key(key).and_then(std::env::var_os))
}

/// [`env_os`] for a value that must be UTF-8.
pub fn env(key: &str) -> Option<String> {
    env_os(key).and_then(|value| value.into_string().ok())
}

/// Moves the old data folder to the new one when only the old one exists. Run once at startup,
/// before anything reads the data dir. Does nothing when a `*_DATA_DIR` variable picks the
/// folder.
pub fn carry_over_data_dir() {
    if env_os("STRONGHOLD_DATA_DIR").is_some_and(|dir| !dir.is_empty()) {
        return;
    }
    let Some(base) = directories::BaseDirs::new() else {
        return;
    };
    let old = base.data_dir().join(OLD_APP_NAME);
    let new = base.data_dir().join(crate::APP_NAME);
    if let Err(e) = move_data_dir(&old, &new) {
        eprintln!(
            "stronghold: could not move {} to {}: {e}",
            old.display(),
            new.display()
        );
    }
}

/// Renames `old` to `new` and leaves a link at `old`, so the session hook installed under the
/// old path and processes started before the rename keep finding their files. Returns whether
/// it moved anything: not when `new` exists or `old` is not a real folder.
fn move_data_dir(old: &Path, new: &Path) -> io::Result<bool> {
    let is_real_dir = |path: &Path| path.symlink_metadata().is_ok_and(|m| m.is_dir());
    if new.symlink_metadata().is_ok() || !is_real_dir(old) {
        return Ok(false);
    }
    if let Err(e) = std::fs::rename(old, new) {
        // Another process starting at the same moment may have moved it first.
        return if is_real_dir(new) { Ok(false) } else { Err(e) };
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(new, old)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_env_key_swaps_the_prefix() {
        assert_eq!(
            old_env_key("STRONGHOLD_DATA_DIR").as_deref(),
            Some("MYRSTACK_DATA_DIR")
        );
        assert_eq!(old_env_key("PATH"), None);
    }

    #[test]
    fn env_falls_back_to_the_old_spelling() {
        // SAFETY: these variables are unique to this test, so no other thread touches them.
        unsafe { std::env::set_var("MYRSTACK_LEGACY_TEST_ONLY_OLD", "old") };
        assert_eq!(
            env("STRONGHOLD_LEGACY_TEST_ONLY_OLD").as_deref(),
            Some("old")
        );
        unsafe { std::env::set_var("STRONGHOLD_LEGACY_TEST_ONLY_OLD", "new") };
        assert_eq!(
            env("STRONGHOLD_LEGACY_TEST_ONLY_OLD").as_deref(),
            Some("new")
        );
        assert_eq!(env("STRONGHOLD_LEGACY_TEST_ONLY_UNSET"), None);
    }

    #[cfg(unix)]
    #[test]
    fn moves_the_old_folder_and_links_the_old_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("Myrstack"), tmp.path().join("Stronghold"));
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("settings.json"), "{}").unwrap();

        assert!(move_data_dir(&old, &new).unwrap());
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            "{}"
        );
        // The old path still reaches the files, and a second run changes nothing.
        assert!(old.symlink_metadata().unwrap().is_symlink());
        assert!(old.join("settings.json").exists());
        assert!(!move_data_dir(&old, &new).unwrap());
    }

    #[test]
    fn leaves_an_existing_new_folder_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("Myrstack"), tmp.path().join("Stronghold"));
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&new).unwrap();
        assert!(!move_data_dir(&old, &new).unwrap());
        assert!(old.symlink_metadata().unwrap().is_dir());

        let missing = tmp.path().join("Nothing");
        assert!(!move_data_dir(&missing, &tmp.path().join("Fresh")).unwrap());
    }
}
