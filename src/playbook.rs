//! The clan playbook: the Markdown rules coordinators, attendants, workers and the Envoy run
//! by. Myrstack serves them (MCP prompts, resources and tools; `myrstack playbook`) straight from disk on
//! every request, stamped with the git version they came from. See `playbook/README.md`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

pub mod fetches;
pub mod fill;
pub mod source;

/// Overrides where the playbook files are read from.
pub const ENV_DIR: &str = "MYRSTACK_PLAYBOOK_DIR";

/// The repo's `playbook/` dir, as of the build.
pub const BAKED_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/playbook");

pub const PLAYBOOK: &str = "AGENT-FLEET-PLAYBOOK.md";
pub const PROTOCOL: &str = "_AGENT_PROTOCOL.md";
pub const ATTENDANT_BRIEF: &str = "ATTENDANT-BRIEF-TEMPLATE.md";
pub const KICKOFF: &str = "KICKOFF-TEMPLATE.md";
pub const README: &str = "README.md";
pub const ENVOY: &str = "ENVOY-PLAYBOOK.md";

/// The known files, in listing order.
pub const FILES: [&str; 6] = [PLAYBOOK, PROTOCOL, ATTENDANT_BRIEF, KICKOFF, ENVOY, README];

/// The line a prompt puts before the docs.
pub const STANDING_RULES: &str = "These are your standing rules for this session; follow them.";

/// Who is asking for their rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Coordinator,
    Attendant,
    Worker,
    Envoy,
}

/// One piece of a role's docs: a whole file, or some of its `## ` sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub file: &'static str,
    pub sections: Option<&'static [&'static str]>,
}

impl Role {
    pub const ALL: [Role; 4] = [
        Role::Coordinator,
        Role::Attendant,
        Role::Worker,
        Role::Envoy,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Role::Coordinator => "coordinator",
            Role::Attendant => "attendant",
            Role::Worker => "worker",
            Role::Envoy => "envoy",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL
            .into_iter()
            .find(|r| r.name().eq_ignore_ascii_case(s.trim()))
    }

    pub fn title(self) -> &'static str {
        match self {
            Role::Coordinator => "Coordinator playbook",
            Role::Attendant => "Attendant brief",
            Role::Worker => "Worker protocol",
            Role::Envoy => "Envoy playbook",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Role::Coordinator => {
                "Standing rules for a clan coordinator: the clan playbook plus the worker \
                 protocol it enforces."
            }
            Role::Attendant => {
                "Standing rules for an attendant: the attendant brief template plus the \
                 playbook's Coordinator duties and Token economy sections."
            }
            Role::Worker => "Standing rules for a dispatched worker: the agent protocol.",
            Role::Envoy => {
                "Standing rules for the Envoy, Karl's Slack diplomat: the Envoy playbook only \
                 (it is not a worker)."
            }
        }
    }

    pub fn parts(self) -> &'static [Part] {
        const WHOLE_PLAYBOOK: Part = Part {
            file: PLAYBOOK,
            sections: None,
        };
        const WHOLE_PROTOCOL: Part = Part {
            file: PROTOCOL,
            sections: None,
        };
        match self {
            Role::Coordinator => &[WHOLE_PLAYBOOK, WHOLE_PROTOCOL],
            Role::Attendant => &[
                Part {
                    file: ATTENDANT_BRIEF,
                    sections: None,
                },
                Part {
                    file: PLAYBOOK,
                    sections: Some(&["Coordinator duties", "Token economy"]),
                },
            ],
            Role::Worker => &[WHOLE_PROTOCOL],
            Role::Envoy => &[Part {
                file: ENVOY,
                sections: None,
            }],
        }
    }

    pub fn files(self) -> Vec<&'static str> {
        self.parts().iter().map(|p| p.file).collect()
    }
}

/// The Envoy playbook as the session's appended system prompt, so it is part of the cached
/// prefix and survives a context reset.
pub fn envoy_system_prompt(playbook: &Playbook) -> Result<String> {
    let served = playbook.get(&Target::Role(Role::Envoy), None)?;
    Ok(format!(
        "# Envoy standing rules\n\n{}\n\n{}",
        served.version.header(),
        served.body
    ))
}

/// The first prompt of the Envoy session.
pub fn envoy_launch_prompt(dry_run: bool) -> String {
    let mode = if dry_run {
        "Dry-run is ON: drafts and relays are recorded and shown, nothing leaves."
    } else {
        "Dry-run is OFF: drafts Karl approves are posted and relays are delivered."
    };
    format!(
        "You are the Envoy. Your standing rules are the Envoy playbook in your system prompt; \
         follow them. {mode} Then wait: Myrstack types each new event into this terminal as \
         an `Envoy event:` line. Don't poll, and don't load the playbook again: it is already \
         there."
    )
}

/// What to serve: a role's docs or one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Role(Role),
    File(String),
}

impl Target {
    /// A role name, else a file name.
    pub fn parse(s: &str) -> Target {
        Role::parse(s).map_or_else(|| Target::File(s.trim().to_owned()), Target::Role)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Role(r) => f.write_str(r.name()),
            Target::File(name) => f.write_str(name),
        }
    }
}

/// Which playbook text a response carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Short commit of the last change to the playbook dir; `None` outside git or before the
    /// first commit.
    pub commit: Option<String>,
    /// Uncommitted changes in the playbook dir.
    pub dirty: bool,
    pub in_git: bool,
    /// Short hash of the served text.
    pub hash: String,
}

impl Version {
    pub fn to_json(&self) -> Value {
        json!({
            "commit": self.commit,
            "dirty": self.dirty,
            "in_git": self.in_git,
            "hash": self.hash,
        })
    }

    /// `Playbook version: 1b0b685 (clean) · hash 3f9a1c2`.
    pub fn header(&self) -> String {
        format!("Playbook version: {self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = if self.dirty { "dirty" } else { "clean" };
        match (&self.commit, self.in_git) {
            (Some(commit), _) => write!(f, "{commit} ({state}) · hash {}", self.hash),
            (None, true) => write!(f, "uncommitted ({state}) · hash {}", self.hash),
            (None, false) => write!(f, "hash {} (not in git)", self.hash),
        }
    }
}

/// Served text with its provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct Served {
    pub target: Target,
    pub section: Option<String>,
    /// The files the text came from.
    pub files: Vec<String>,
    /// The text without the version header.
    pub body: String,
    pub version: Version,
}

impl Served {
    /// The body under a version header.
    pub fn text(&self) -> String {
        let what = match &self.section {
            Some(s) => format!("{} · section \"{s}\"", self.target),
            None => self.target.to_string(),
        };
        format!(
            "{}\nServing: {what} (from {})\n\n{}",
            self.version.header(),
            self.files.join(", "),
            self.body
        )
    }

    pub fn to_json(&self) -> Value {
        json!({
            "target": self.target.to_string(),
            "section": self.section,
            "files": self.files,
            "version": self.version.to_json(),
            // Claude Code shows the model `structuredContent` instead of the text block, so
            // the rules themselves must be here too.
            "text": self.text(),
        })
    }
}

/// A `## ` section of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    /// From the heading line to just before the next `#`/`##` heading.
    pub text: String,
}

/// The `## ` sections of a Markdown text, ignoring headings inside code fences.
pub fn sections(markdown: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut open = false;
    let mut fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
        }
        let heading = !fence && (line.starts_with("# ") || line.starts_with("## "));
        if heading {
            open = false;
            if let Some(title) = line.strip_prefix("## ") {
                out.push(Section {
                    title: title.trim().to_owned(),
                    text: String::new(),
                });
                open = true;
            }
        }
        if open && let Some(section) = out.last_mut() {
            section.text.push_str(line);
            section.text.push('\n');
        }
    }
    for s in &mut out {
        s.text.truncate(s.text.trim_end().len());
    }
    out
}

/// The sections matching `query`: exact case-insensitive title matches if any, else prefix
/// matches.
fn matching<'a>(all: &'a [(String, Section)], query: &str) -> Vec<&'a (String, Section)> {
    let q = query.trim().to_lowercase();
    let exact: Vec<_> = all
        .iter()
        .filter(|(_, s)| s.title.to_lowercase() == q)
        .collect();
    if !exact.is_empty() || q.is_empty() {
        return exact;
    }
    all.iter()
        .filter(|(_, s)| s.title.to_lowercase().starts_with(&q))
        .collect()
}

/// A playbook dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playbook {
    dir: PathBuf,
}

impl Playbook {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `MYRSTACK_PLAYBOOK_DIR`, else the repo's `playbook/`, else `~/Documents/agents`.
    pub fn locate() -> Result<Self> {
        let env = std::env::var_os(ENV_DIR).map(PathBuf::from);
        let home = directories::BaseDirs::new().map(|b| b.home_dir().join("Documents/agents"));
        resolve_dir(env, Path::new(BAKED_DIR), home).map(Self::at)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The known files that exist, then any other `.md` files, by name.
    pub fn files(&self) -> Vec<String> {
        let mut out: Vec<String> = FILES
            .iter()
            .filter(|f| self.dir.join(f).is_file())
            .map(|f| f.to_string())
            .collect();
        let mut extra: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".md") && !FILES.contains(&n.as_str()))
            .filter(|n| self.dir.join(n).is_file())
            .collect();
        extra.sort();
        out.extend(extra);
        out
    }

    /// The file `name` refers to: exact, or case-insensitive with or without `.md` or the
    /// leading underscore.
    pub fn resolve_file(&self, name: &str) -> Result<String> {
        let files = self.files();
        let norm = |s: &str| {
            let s = s.trim().to_lowercase();
            let s = s.strip_suffix(".md").unwrap_or(&s).to_owned();
            s.trim_start_matches('_').to_owned()
        };
        let wanted = norm(name);
        files
            .iter()
            .find(|f| *f == name)
            .or_else(|| files.iter().find(|f| norm(f) == wanted))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "no playbook file \"{name}\" in {}. Files: {}. Roles: {}.",
                    self.dir.display(),
                    files.join(", "),
                    role_names()
                )
            })
    }

    pub fn read(&self, file: &str) -> Result<String> {
        let path = self.dir.join(file);
        std::fs::read_to_string(&path)
            .with_context(|| format!("playbook file {file} not found in {}", self.dir.display()))
    }

    /// The docs for `target`, or just its `section`.
    pub fn get(&self, target: &Target, section: Option<&str>) -> Result<Served> {
        let (body, section, files) = self.assemble(target, section)?;
        let version = self.version(&body);
        Ok(Served {
            target: target.clone(),
            section,
            files,
            body,
            version,
        })
    }

    /// The hash [`Playbook::get`] would stamp on `target`/`section` now, without running git.
    pub fn current_hash(&self, target: &Target, section: Option<&str>) -> Result<String> {
        Ok(short_hash(&self.assemble(target, section)?.0))
    }

    /// The served body, the matched section title and the files it came from.
    fn assemble(
        &self,
        target: &Target,
        section: Option<&str>,
    ) -> Result<(String, Option<String>, Vec<String>)> {
        let parts: Vec<(String, Option<&[&str]>)> = match target {
            Target::Role(role) => role
                .parts()
                .iter()
                .map(|p| (p.file.to_owned(), p.sections))
                .collect(),
            Target::File(name) => vec![(self.resolve_file(name)?, None)],
        };
        let mut texts = Vec::new();
        for (file, _) in &parts {
            texts.push((file.clone(), self.read(file)?));
        }
        let (body, section, files) = match section {
            Some(query) => {
                let all: Vec<(String, Section)> = texts
                    .iter()
                    .flat_map(|(f, t)| sections(t).into_iter().map(move |s| (f.clone(), s)))
                    .collect();
                let hits = matching(&all, query);
                if hits.is_empty() {
                    bail!(
                        "no section \"{query}\" in {target}. Sections:\n{}",
                        titles_text(&all)
                    );
                }
                let title = hits[0].1.title.clone();
                let body = if hits.len() == 1 {
                    hits[0].1.text.clone()
                } else {
                    hits.iter()
                        .map(|(f, s)| format!("<!-- {f} -->\n{}", s.text))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                };
                let mut files: Vec<String> = hits.iter().map(|(f, _)| f.clone()).collect();
                files.dedup();
                (body, Some(title), files)
            }
            None => {
                let pieces: Vec<String> = parts
                    .iter()
                    .zip(&texts)
                    .map(|((_, wanted), (file, text))| match wanted {
                        None => text.trim_end().to_owned(),
                        Some(titles) => {
                            let found = sections(text);
                            let picked: Vec<String> = titles
                                .iter()
                                .filter_map(|t| {
                                    found.iter().find(|s| s.title.eq_ignore_ascii_case(t))
                                })
                                .map(|s| s.text.clone())
                                .collect();
                            format!(
                                "# From {file}: {}\n\n{}",
                                titles.join(", "),
                                picked.join("\n\n")
                            )
                        }
                    })
                    .collect();
                let body = if pieces.len() == 1 {
                    pieces.into_iter().next().unwrap_or_default()
                } else {
                    texts
                        .iter()
                        .zip(pieces)
                        .map(|((file, _), piece)| format!("<!-- {file} -->\n{piece}"))
                        .collect::<Vec<_>>()
                        .join("\n\n---\n\n")
                };
                let files = texts.iter().map(|(f, _)| f.clone()).collect();
                (body, None, files)
            }
        };
        Ok((body, section, files))
    }

    /// The version stamp for `served` text from this dir.
    pub fn version(&self, served: &str) -> Version {
        let git_dir = self.git_dir();
        let in_git =
            git(&git_dir, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true");
        let commit = in_git
            .then(|| git(&git_dir, &["log", "-1", "--format=%h", "--", "."]))
            .flatten();
        let dirty = in_git
            && git(&git_dir, &["status", "--porcelain", "--", "."]).is_some_and(|s| !s.is_empty());
        Version {
            commit: commit.filter(|c| !c.is_empty()),
            dirty,
            in_git,
            hash: short_hash(served),
        }
    }

    /// Where git runs: the real dir of the playbook file, so a dir of symlinks into the repo
    /// (`~/Documents/agents`) still gets the repo's version.
    fn git_dir(&self) -> PathBuf {
        FILES
            .iter()
            .find_map(|f| std::fs::canonicalize(self.dir.join(f)).ok())
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.dir.clone())
    }

    /// Files, roles and section titles, for `list_playbook` and `myrstack playbook --list`.
    pub fn listing(&self) -> Result<Listing> {
        let mut files = Vec::new();
        let mut all_text = String::new();
        for name in self.files() {
            let text = self.read(&name)?;
            all_text.push_str(&text);
            files.push(FileEntry {
                bytes: text.len(),
                sections: sections(&text).into_iter().map(|s| s.title).collect(),
                name,
            });
        }
        if files.is_empty() {
            bail!("no playbook files in {}", self.dir.display());
        }
        Ok(Listing {
            dir: self.dir.clone(),
            version: self.version(&all_text),
            files,
        })
    }
}

/// One file in a [`Listing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub bytes: usize,
    pub sections: Vec<String>,
}

/// What the playbook dir holds. The version's hash covers every file.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub dir: PathBuf,
    pub files: Vec<FileEntry>,
    pub version: Version,
}

impl Listing {
    pub fn text(&self) -> String {
        let mut out = format!(
            "{}\nDir: {}\n\nRoles:\n",
            self.version.header(),
            self.dir.display()
        );
        for role in Role::ALL {
            out.push_str(&format!(
                "- {}: {} — {}\n",
                role.name(),
                role.files().join(" + "),
                role.description()
            ));
        }
        out.push_str("\nFiles and their sections:\n");
        for f in &self.files {
            out.push_str(&format!(
                "- {} ({:.1} KB): {}\n",
                f.name,
                f.bytes as f64 / 1024.0,
                f.sections.join(" · ")
            ));
        }
        out
    }

    pub fn to_json(&self) -> Value {
        json!({
            "dir": self.dir.display().to_string(),
            "version": self.version.to_json(),
            "roles": Role::ALL.iter().map(|r| json!({
                "name": r.name(),
                "files": r.files(),
                "description": r.description(),
            })).collect::<Vec<_>>(),
            "files": self.files.iter().map(|f| json!({
                "name": f.name,
                "bytes": f.bytes,
                "sections": f.sections,
            })).collect::<Vec<_>>(),
        })
    }
}

/// The section titles of one target, under a version header, for `myrstack playbook X --list`.
pub fn target_sections_text(playbook: &Playbook, target: &Target) -> Result<String> {
    let served = playbook.get(target, None)?;
    let mut all = Vec::new();
    for file in &served.files {
        let text = playbook.read(file)?;
        all.extend(sections(&text).into_iter().map(|s| (file.clone(), s)));
    }
    Ok(format!(
        "{}\nSections of {target}:\n{}",
        served.version.header(),
        titles_text(&all)
    ))
}

/// The dir to serve from: `env` if set (it must hold the playbook), else `baked` if it holds
/// the playbook, else `home` if it does.
pub fn resolve_dir(env: Option<PathBuf>, baked: &Path, home: Option<PathBuf>) -> Result<PathBuf> {
    let has_files = |d: &Path| d.join(PLAYBOOK).is_file() || d.join(PROTOCOL).is_file();
    if let Some(dir) = env.filter(|d| !d.as_os_str().is_empty()) {
        if has_files(&dir) {
            return Ok(dir);
        }
        bail!(
            "{ENV_DIR}={} has no {PLAYBOOK} or {PROTOCOL}",
            dir.display()
        );
    }
    let mut tried = vec![baked.to_path_buf()];
    if has_files(baked) {
        return Ok(baked.to_path_buf());
    }
    if let Some(home) = home {
        if has_files(&home) {
            return Ok(home);
        }
        tried.push(home);
    }
    bail!(
        "no playbook found (looked for {PLAYBOOK} in {}); set {ENV_DIR} to the playbook dir",
        tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn titles_text(all: &[(String, Section)]) -> String {
    let mut out = String::new();
    let mut current: Option<&str> = None;
    for (file, s) in all {
        if current != Some(file.as_str()) {
            out.push_str(&format!("{file}:\n"));
            current = Some(file);
        }
        out.push_str(&format!("- {}\n", s.title));
    }
    out
}

fn role_names() -> String {
    Role::ALL.map(Role::name).join(", ")
}

/// Trimmed stdout of a successful git command in `dir`.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// 7 hex digits of the FNV-1a 64 hash: stable across builds and platforms.
pub fn short_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")[..7].to_owned()
}

// ---------------------------------------------------------------------------
// CLI

/// `myrstack playbook [role|file] [--section X] [--list]`; `None` for other subcommands.
pub fn run_cli(args: &[String]) -> Option<Result<()>> {
    let (cmd, rest) = args.split_first()?;
    (cmd == "playbook").then(|| {
        let text = cli_text(rest)?;
        println!("{}", text.trim_end());
        Ok(())
    })
}

const USAGE: &str = "usage: myrstack playbook [coordinator|attendant|worker|envoy|<file>] \
                     [--section <title>] [--list]\n       \
                     myrstack playbook attendant-brief <run_id>\n       \
                     myrstack playbook kickoff <task_id>";

/// What `myrstack playbook <args>` prints.
pub fn cli_text(args: &[String]) -> Result<String> {
    if let Some(template) = args.first().and_then(|a| source::Template::parse(a)) {
        return template_cli_text(template, &args[1..]);
    }
    let mut target = None;
    let mut section = None;
    let mut list = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--list" | "-l" => list = true,
            "--section" | "-s" => {
                section = Some(
                    it.next()
                        .ok_or_else(|| anyhow!("--section needs a title"))?,
                );
            }
            "--help" | "-h" => return Ok(USAGE.to_owned()),
            s if s.starts_with("--section=") => section = Some(arg),
            s if s.starts_with('-') => bail!("unknown option {s}\n{USAGE}"),
            _ if target.is_some() => bail!("one role or file at a time\n{USAGE}"),
            _ => target = Some(Target::parse(arg)),
        }
    }
    let section = section.map(|s| s.strip_prefix("--section=").unwrap_or(s));
    let playbook = Playbook::locate()?;
    match (target, list) {
        (None, true) => Ok(playbook.listing()?.text()),
        (Some(t), true) => target_sections_text(&playbook, &t),
        (target, false) => {
            let target = target.unwrap_or(Target::Role(Role::Coordinator));
            let served = playbook.get(&target, section)?;
            fetches::record(
                fetches::default_path().as_deref(),
                "cli",
                &served.target,
                served.section.as_deref(),
                &served.version,
            );
            Ok(served.text())
        }
    }
}

/// `myrstack playbook attendant-brief <run_id>` / `kickoff <task_id>`: the template filled from
/// the running app's data, else from one Orca poll.
fn template_cli_text(template: source::Template, args: &[String]) -> Result<String> {
    let id = match args {
        [id] if !id.starts_with('-') => id,
        [flag] if flag == "--help" || flag == "-h" => return Ok(USAGE.to_owned()),
        _ => bail!(
            "usage: myrstack playbook {} <{}>",
            template.name(),
            template.arg()
        ),
    };
    let playbook = Playbook::locate()?;
    let mut control = crate::mcp::SocketControl::new(crate::control::socket_path());
    let filled = source::render(&playbook, &mut control, template, id).map_err(|e| anyhow!(e))?;
    fetches::record(
        fetches::default_path().as_deref(),
        &format!("cli:{}", template.name()),
        &Target::File(template.file().to_owned()),
        None,
        &filled.context.version,
    );
    Ok(filled.text())
}

#[cfg(test)]
mod tests;
