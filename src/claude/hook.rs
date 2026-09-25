//! The `SessionStart` hook that records exact terminal-to-session mappings.
//!
//! `hooks/myrstack-session-hook.sh` appends one JSON line per Claude session
//! start to `session-map.jsonl` in the Myrstack data dir; [`load_hook_mappings`]
//! reads it back. `myrstack install-hook` / `uninstall-hook` copy the script
//! and add or remove its entry in Claude's `settings.json`.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::sessions::HookMappings;

/// The hook script, installed into the data dir by `install-hook`.
pub const HOOK_SCRIPT: &str = include_str!("../../hooks/myrstack-session-hook.sh");
/// File name of the installed script; also how our settings entry is recognised.
pub const HOOK_SCRIPT_NAME: &str = "myrstack-session-hook.sh";
/// File the hook appends to, inside the data dir.
pub const SESSION_MAP_FILE: &str = "session-map.jsonl";

const HOOK_EVENT: &str = "SessionStart";

/// `$MYRSTACK_DATA_DIR`, else `<platform data dir>/Myrstack`
/// (`~/Library/Application Support/Myrstack` on macOS, as in the script).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MYRSTACK_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    directories::BaseDirs::new()
        .map(|b| b.data_dir().to_path_buf())
        .unwrap_or_default()
        .join(crate::APP_NAME)
}

/// Where the hook writes its mappings.
pub fn default_session_map_path() -> PathBuf {
    data_dir().join(SESSION_MAP_FILE)
}

/// `$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`.
pub fn default_settings_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("settings.json");
    }
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_default()
        .join(".claude")
        .join("settings.json")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookRecord {
    pane_key: Option<String>,
    tab_id: Option<String>,
    session_id: String,
    #[serde(default)]
    at: i64,
}

/// Reads the hook's jsonl file. The newest record per pane key and per tab id
/// wins (by `at`, later lines winning ties); malformed lines are skipped and a
/// missing file yields empty mappings.
pub fn load_hook_mappings(path: &Path) -> HookMappings {
    let Ok(text) = fs::read_to_string(path) else {
        return HookMappings::default();
    };
    parse_hook_mappings(&text)
}

fn parse_hook_mappings(text: &str) -> HookMappings {
    let mut panes: HashMap<String, (i64, String)> = HashMap::new();
    let mut tabs: HashMap<String, (i64, String)> = HashMap::new();
    let record = |map: &mut HashMap<String, (i64, String)>, key: String, at: i64, id: &str| {
        let entry = map.entry(key).or_insert((at, id.to_owned()));
        if at >= entry.0 {
            *entry = (at, id.to_owned());
        }
    };
    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<HookRecord>(line) else {
            continue;
        };
        if rec.session_id.is_empty() {
            continue;
        }
        if let Some(key) = rec.pane_key.filter(|k| !k.is_empty()) {
            record(&mut panes, key, rec.at, &rec.session_id);
        }
        if let Some(tab) = rec.tab_id.filter(|t| !t.is_empty()) {
            record(&mut tabs, tab, rec.at, &rec.session_id);
        }
    }
    HookMappings {
        by_pane_key: panes.into_iter().map(|(k, (_, id))| (k, id)).collect(),
        by_tab_id: tabs.into_iter().map(|(k, (_, id))| (k, id)).collect(),
    }
}

/// Shell command for the settings entry; quoted because the data dir usually
/// contains a space.
pub fn hook_command(script: &Path) -> String {
    let s = script.to_string_lossy();
    if s.contains(['"', '\\', '$', '`']) {
        format!("'{}'", s.replace('\'', r"'\''"))
    } else {
        format!("\"{s}\"")
    }
}

fn is_our_hook(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(HOOK_SCRIPT_NAME))
}

fn settings_object(settings: &mut Value) -> Result<&mut Map<String, Value>> {
    if settings.is_null() {
        *settings = Value::Object(Map::new());
    }
    settings
        .as_object_mut()
        .context("settings JSON is not an object")
}

/// Adds our `SessionStart` entry running `command`. Returns whether anything
/// changed: an existing entry is left alone, or has its command updated when
/// the script moved. Other keys and hooks are untouched.
pub fn install_hook_entry(settings: &mut Value, command: &str) -> Result<bool> {
    let root = settings_object(settings)?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("settings \"hooks\" is not an object")?;
    let groups = hooks
        .entry(HOOK_EVENT)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .context("settings \"hooks.SessionStart\" is not an array")?;

    let mut found = false;
    let mut changed = false;
    for group in groups.iter_mut() {
        let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        for hook in list.iter_mut().filter(|h| is_our_hook(h)) {
            found = true;
            if hook.get("command").and_then(Value::as_str) != Some(command) {
                hook["command"] = Value::String(command.to_owned());
                changed = true;
            }
        }
    }
    if !found {
        groups.push(json!({ "hooks": [ { "type": "command", "command": command } ] }));
        changed = true;
    }
    Ok(changed)
}

/// Removes every hook of ours from `SessionStart`, dropping groups, the
/// `SessionStart` key and the `hooks` key only when our removal emptied them.
/// Returns whether anything changed.
pub fn uninstall_hook_entry(settings: &mut Value) -> Result<bool> {
    let Some(root) = settings.as_object_mut() else {
        return Ok(false);
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    let Some(groups) = hooks.get_mut(HOOK_EVENT).and_then(Value::as_array_mut) else {
        return Ok(false);
    };

    let mut changed = false;
    groups.retain_mut(|group| {
        let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        let before = list.len();
        list.retain(|h| !is_our_hook(h));
        if list.len() == before {
            return true;
        }
        changed = true;
        !list.is_empty()
    });
    if changed && groups.is_empty() {
        hooks.shift_remove(HOOK_EVENT);
        if hooks.is_empty() {
            root.shift_remove("hooks");
        }
    }
    Ok(changed)
}

fn read_settings(path: &Path) -> Result<Option<Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Some(Value::Object(Map::new())));
    }
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("parsing {}", path.display()))
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Writes `contents` to `path` via a temp file in the same dir and a rename.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().context("path has no file name")?;
    let tmp = dir.join(format!(
        ".{}.myrstack-tmp-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        if let Ok(meta) = fs::metadata(path) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.with_context(|| format!("writing {}", path.display()))
}

/// Backs up `path` to `<path>.myrstack-bak-<unix ms>` (never overwriting an
/// earlier backup) and writes `settings`.
fn save_settings(path: &Path, settings: &Value) -> Result<Option<PathBuf>> {
    let backup = if path.exists() {
        let stamp = unix_millis();
        let backup = (0..)
            .map(|n| {
                let mut name = path.as_os_str().to_owned();
                name.push(format!(".myrstack-bak-{stamp}"));
                if n > 0 {
                    name.push(format!("-{n}"));
                }
                PathBuf::from(name)
            })
            .find(|p| !p.exists())
            .context("no free backup name")?;
        fs::copy(path, &backup).with_context(|| format!("backing up to {}", backup.display()))?;
        Some(backup)
    } else {
        None
    };
    let mut text = serde_json::to_string_pretty(settings)?;
    text.push('\n');
    write_atomic(path, text.as_bytes())?;
    Ok(backup)
}

/// Options shared by `install-hook` and `uninstall-hook`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCliOptions {
    pub settings: PathBuf,
    pub dry_run: bool,
}

fn parse_options(args: &[String]) -> Result<HookCliOptions> {
    let mut settings = None;
    let mut dry_run = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--settings" => {
                settings = Some(PathBuf::from(it.next().context("--settings needs a path")?));
            }
            other => match other.strip_prefix("--settings=") {
                Some(path) => settings = Some(PathBuf::from(path)),
                None => bail!("unknown argument {other:?}"),
            },
        }
    }
    Ok(HookCliOptions {
        settings: settings.unwrap_or_else(default_settings_path),
        dry_run,
    })
}

/// Copies the script into `data_dir` and adds its entry to the settings file.
pub fn install(opts: &HookCliOptions, data_dir: &Path) -> Result<()> {
    let script = data_dir.join(HOOK_SCRIPT_NAME);
    let command = hook_command(&script);
    let mut settings = read_settings(&opts.settings)?.unwrap_or(Value::Object(Map::new()));
    let changed = install_hook_entry(&mut settings, &command)?;

    if opts.dry_run {
        println!("[dry run] would write hook script to {}", script.display());
        if changed {
            println!("[dry run] would update {}:", opts.settings.display());
            println!("{}", serde_json::to_string_pretty(&settings)?);
        } else {
            println!("[dry run] {} already has the hook", opts.settings.display());
        }
        return Ok(());
    }

    fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    write_atomic(&script, HOOK_SCRIPT.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod {}", script.display()))?;
    }
    println!("Installed hook script at {}", script.display());
    if changed {
        if let Some(backup) = save_settings(&opts.settings, &settings)? {
            println!("Backed up settings to {}", backup.display());
        }
        println!("Added SessionStart hook to {}", opts.settings.display());
    } else {
        println!("{} already has the hook", opts.settings.display());
    }
    Ok(())
}

/// Removes our entry from the settings file; the script is left in place.
pub fn uninstall(opts: &HookCliOptions) -> Result<()> {
    let Some(mut settings) = read_settings(&opts.settings)? else {
        println!("{} does not exist; nothing to do", opts.settings.display());
        return Ok(());
    };
    if !uninstall_hook_entry(&mut settings)? {
        println!("{} has no Myrstack hook", opts.settings.display());
        return Ok(());
    }
    if opts.dry_run {
        println!("[dry run] would update {}:", opts.settings.display());
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }
    if let Some(backup) = save_settings(&opts.settings, &settings)? {
        println!("Backed up settings to {}", backup.display());
    }
    println!("Removed SessionStart hook from {}", opts.settings.display());
    Ok(())
}

/// Handles `install-hook` / `uninstall-hook` if `args` (argv without the
/// program name) starts with one; returns `None` otherwise.
pub fn run_cli(args: &[String]) -> Option<Result<()>> {
    let (cmd, rest) = args.split_first()?;
    match cmd.as_str() {
        "install-hook" => Some(parse_options(rest).and_then(|o| install(&o, &data_dir()))),
        "uninstall-hook" => Some(parse_options(rest).and_then(|o| uninstall(&o))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    const CMD: &str = "\"/data/Myrstack/myrstack-session-hook.sh\"";

    fn line(pane: &str, tab: &str, session: &str, at: i64) -> String {
        format!(
            r#"{{"paneKey":"{pane}","tabId":"{tab}","worktreeId":"w","sessionId":"{session}","transcriptPath":"/t","cwd":"/c","source":"startup","at":{at}}}"#
        )
    }

    #[test]
    fn newest_record_per_pane_and_tab_wins() {
        let text = [
            line("t1:a", "t1", "s1", 100),
            "not json".into(),
            line("t1:b", "t1", "s2", 200),
            r#"{"paneKey":"t9:x"}"#.into(),
            String::new(),
            line("t1:a", "t1", "s3", 300),
            line("t2:a", "t2", "old", 50),
            line("t2:a", "t2", "same-at-later-line", 50),
            line("t3:a", "t3", "new", 500),
            line("t3:a", "t3", "stale", 400),
        ]
        .join("\n");
        let m = parse_hook_mappings(&text);
        assert_eq!(m.by_pane_key["t1:a"], "s3");
        assert_eq!(m.by_pane_key["t1:b"], "s2");
        assert_eq!(m.by_pane_key["t2:a"], "same-at-later-line");
        assert_eq!(m.by_pane_key["t3:a"], "new");
        assert!(!m.by_pane_key.contains_key("t9:x"));
        assert_eq!(m.by_tab_id["t1"], "s3");
        assert_eq!(m.by_tab_id["t3"], "new");
        assert_eq!(m.by_pane_key.len(), 4);
    }

    #[test]
    fn load_reads_file_and_tolerates_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SESSION_MAP_FILE);
        assert!(load_hook_mappings(&path).is_empty());
        fs::write(&path, format!("{}\n{{broken\n", line("t:l", "t", "s", 1))).unwrap();
        let m = load_hook_mappings(&path);
        assert_eq!(m.by_pane_key["t:l"], "s");
        assert_eq!(m.by_tab_id["t"], "s");
    }

    #[test]
    fn hook_command_quotes_paths() {
        assert_eq!(
            hook_command(Path::new("/a/Application Support/x.sh")),
            "\"/a/Application Support/x.sh\""
        );
        assert_eq!(hook_command(Path::new("/a/$b/x.sh")), "'/a/$b/x.sh'");
        assert_eq!(hook_command(Path::new("/a/it's$/x")), r"'/a/it'\''s$/x'");
    }

    #[test]
    fn install_into_empty_settings() {
        let mut v = Value::Null;
        assert!(install_hook_entry(&mut v, CMD).unwrap());
        assert_eq!(
            v,
            json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": CMD}]}]}})
        );
        let mut v = json!({});
        assert!(install_hook_entry(&mut v, CMD).unwrap());
        assert_eq!(v["hooks"]["SessionStart"][0]["hooks"][0]["command"], CMD);
    }

    #[test]
    fn install_keeps_other_keys_and_hooks_in_order() {
        let mut v = json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "x"}]}],
                "SessionStart": [{"hooks": [{"type": "command", "command": "orca-hook.sh"}]}]
            },
            "permissions": {"allow": ["Bash(ls)"]}
        });
        let original = v.clone();
        assert!(install_hook_entry(&mut v, CMD).unwrap());
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["model", "hooks", "permissions"]);
        assert_eq!(v["hooks"]["PreToolUse"], original["hooks"]["PreToolUse"]);
        let start = v["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2);
        assert_eq!(start[0], original["hooks"]["SessionStart"][0]);
        assert_eq!(start[1]["hooks"][0]["command"], CMD);
    }

    #[test]
    fn install_is_idempotent_and_updates_moved_script() {
        let mut v = json!({});
        install_hook_entry(&mut v, CMD).unwrap();
        let once = v.clone();
        assert!(!install_hook_entry(&mut v, CMD).unwrap());
        assert_eq!(v, once);

        let moved = "\"/elsewhere/myrstack-session-hook.sh\"";
        assert!(install_hook_entry(&mut v, moved).unwrap());
        let start = v["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 1);
        assert_eq!(start[0]["hooks"][0]["command"], moved);
    }

    #[test]
    fn install_rejects_unexpected_shapes() {
        assert!(install_hook_entry(&mut json!([]), CMD).is_err());
        assert!(install_hook_entry(&mut json!({"hooks": []}), CMD).is_err());
        assert!(install_hook_entry(&mut json!({"hooks": {"SessionStart": {}}}), CMD).is_err());
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let other = json!({"hooks": [{"type": "command", "command": "orca-hook.sh"}]});
        let mut v = json!({
            "hooks": {
                "PreToolUse": [{"hooks": [{"type": "command", "command": "x"}]}],
                "SessionStart": [
                    other.clone(),
                    {"matcher": "startup", "hooks": [
                        {"type": "command", "command": "keep-me"},
                        {"type": "command", "command": CMD}
                    ]}
                ]
            }
        });
        install_hook_entry(&mut v, CMD).unwrap();
        assert!(uninstall_hook_entry(&mut v).unwrap());
        let start = v["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2);
        assert_eq!(start[0], other);
        assert_eq!(
            start[1],
            json!({"matcher": "startup", "hooks": [{"type": "command", "command": "keep-me"}]})
        );
        assert!(!uninstall_hook_entry(&mut v).unwrap());
    }

    #[test]
    fn uninstall_cleans_up_what_install_created() {
        let mut v = json!({"model": "opus", "theme": "dark"});
        install_hook_entry(&mut v, CMD).unwrap();
        v["zzz"] = json!(1);
        assert!(uninstall_hook_entry(&mut v).unwrap());
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["model", "theme", "zzz"]);

        let mut v = json!({"hooks": {"SessionStart": []}});
        assert!(!uninstall_hook_entry(&mut v).unwrap());
        assert_eq!(v, json!({"hooks": {"SessionStart": []}}));
        assert!(!uninstall_hook_entry(&mut json!(null)).unwrap());
    }

    fn opts(settings: &Path, dry_run: bool) -> HookCliOptions {
        HookCliOptions {
            settings: settings.to_path_buf(),
            dry_run,
        }
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".myrstack-bak-"))
            .collect()
    }

    #[test]
    fn install_and_uninstall_files() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data dir");
        let settings = dir.path().join("claude").join("settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let original = "{\n  \"zeta\": 1,\n  \"alpha\": {\"b\": 2, \"a\": 1}\n}\n";
        fs::write(&settings, original).unwrap();

        install(&opts(&settings, true), &data).unwrap();
        assert_eq!(fs::read_to_string(&settings).unwrap(), original);
        assert!(!data.exists());

        install(&opts(&settings, false), &data).unwrap();
        let script = data.join(HOOK_SCRIPT_NAME);
        assert_eq!(fs::read_to_string(&script).unwrap(), HOOK_SCRIPT);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&script).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        let text = fs::read_to_string(&settings).unwrap();
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap());
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            hook_command(&script)
        );
        let baks = backups(settings.parent().unwrap());
        assert_eq!(baks.len(), 1);
        assert_eq!(fs::read_to_string(&baks[0]).unwrap(), original);

        install(&opts(&settings, false), &data).unwrap();
        assert_eq!(fs::read_to_string(&settings).unwrap(), text);

        uninstall(&opts(&settings, true)).unwrap();
        assert_eq!(fs::read_to_string(&settings).unwrap(), text);
        uninstall(&opts(&settings, false)).unwrap();
        assert_eq!(backups(settings.parent().unwrap()).len(), 2);
        let v: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v, serde_json::from_str::<Value>(original).unwrap());
    }

    #[test]
    fn install_creates_missing_settings_and_handles_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("new").join("settings.json");
        install(&opts(&settings, false), &dir.path().join("d")).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(v["hooks"]["SessionStart"].is_array());

        let empty = dir.path().join("empty.json");
        fs::write(&empty, "  \n").unwrap();
        install(&opts(&empty, false), &dir.path().join("d")).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&empty).unwrap()).unwrap();
        assert!(v["hooks"]["SessionStart"].is_array());

        let bad = dir.path().join("bad.json");
        fs::write(&bad, "{not json").unwrap();
        assert!(install(&opts(&bad, false), &dir.path().join("d")).is_err());
        assert_eq!(fs::read_to_string(&bad).unwrap(), "{not json");
    }

    #[test]
    fn parses_cli_options() {
        let args = |s: &[&str]| s.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let o = parse_options(&args(&["--settings", "/x.json", "--dry-run"])).unwrap();
        assert_eq!(o, opts(Path::new("/x.json"), true));
        let o = parse_options(&args(&["--settings=/y.json"])).unwrap();
        assert_eq!(o, opts(Path::new("/y.json"), false));
        assert!(parse_options(&args(&["--settings"])).is_err());
        assert!(parse_options(&args(&["--bogus"])).is_err());
        assert!(run_cli(&args(&["something-else"])).is_none());
        assert!(run_cli(&[]).is_none());
    }

    fn run_script(data: &Path, env: &[(&str, &str)], stdin: &str) -> std::process::Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("hooks")
            .join(HOOK_SCRIPT_NAME);
        let mut cmd = Command::new("sh");
        cmd.arg(script)
            .env("MYRSTACK_DATA_DIR", data)
            .env_remove("ORCA_PANE_KEY")
            .env_remove("ORCA_TAB_ID")
            .env_remove("ORCA_WORKTREE_ID")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    const PAYLOAD: &str = r#"{"session_id":"sess-1","transcript_path":"/Users/x/.claude/projects/-w/sess-1.jsonl","cwd":"/Users/x/a \"q\" b","hook_event_name":"SessionStart","source":"clear"}"#;

    #[test]
    fn script_appends_parseable_line_silently() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data dir");
        let env = [
            ("ORCA_PANE_KEY", "tab-1:leaf-1"),
            ("ORCA_TAB_ID", "tab-1"),
            ("ORCA_WORKTREE_ID", "wt\"1"),
        ];
        let out = run_script(&data, &env, PAYLOAD);
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        assert!(out.stderr.is_empty());

        let text = fs::read_to_string(data.join(SESSION_MAP_FILE)).unwrap();
        let v: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["paneKey"], "tab-1:leaf-1");
        assert_eq!(v["tabId"], "tab-1");
        assert_eq!(v["worktreeId"], "wt\"1");
        assert_eq!(v["sessionId"], "sess-1");
        assert_eq!(
            v["transcriptPath"],
            "/Users/x/.claude/projects/-w/sess-1.jsonl"
        );
        assert_eq!(v["cwd"], "/Users/x/a \"q\" b");
        assert_eq!(v["source"], "clear");
        assert!(v["at"].as_i64().unwrap() > 1_600_000_000_000);

        let second = PAYLOAD.replace("sess-1", "sess-2");
        assert!(run_script(&data, &env, &second).status.success());
        let m = load_hook_mappings(&data.join(SESSION_MAP_FILE));
        assert_eq!(m.by_pane_key["tab-1:leaf-1"], "sess-2");
        assert_eq!(m.by_tab_id["tab-1"], "sess-2");
    }

    #[test]
    fn script_is_a_noop_outside_orca_or_without_session() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_script(dir.path(), &[], PAYLOAD);
        assert!(out.status.success() && out.stdout.is_empty());
        let out = run_script(dir.path(), &[("ORCA_PANE_KEY", "t:l")], "garbage");
        assert!(out.status.success() && out.stdout.is_empty());
        let out = run_script(dir.path(), &[("ORCA_PANE_KEY", "t:l")], "");
        assert!(out.status.success() && out.stdout.is_empty());
        assert!(!dir.path().join(SESSION_MAP_FILE).exists());

        let unwritable = dir.path().join("file");
        fs::write(&unwritable, "").unwrap();
        let out = run_script(&unwritable, &[("ORCA_PANE_KEY", "t:l")], PAYLOAD);
        assert!(out.status.success() && out.stdout.is_empty());
    }

    #[test]
    fn script_caps_file_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SESSION_MAP_FILE);
        let filler = line("old:pane", "old", &"x".repeat(400), 1);
        let text: String = (0..3000).map(|_| format!("{filler}\n")).collect();
        assert!(text.len() > 1_048_576);
        fs::write(&path, text).unwrap();

        let out = run_script(dir.path(), &[("ORCA_PANE_KEY", "t:l")], PAYLOAD);
        assert!(out.status.success());
        let kept = fs::read_to_string(&path).unwrap();
        assert_eq!(kept.lines().count(), 2000);
        assert_eq!(load_hook_mappings(&path).by_pane_key["t:l"], "sess-1");
    }
}
