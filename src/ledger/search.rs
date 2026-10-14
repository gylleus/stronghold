//! Read-only Ledger search. HTTP and file I/O belong on the pane's worker thread.

use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{Entry, Shelf};

const ENDPOINT: &str = "http://127.0.0.1:8643/mcp";
const PROTOCOL: &str = "2025-03-26";
const LIMIT: usize = 30;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Auto,
    Meaning,
    Text,
}

impl Mode {
    pub fn search_type(self, query: &str) -> &'static str {
        let id = query.split_once('-').is_some_and(|(prefix, suffix)| {
            !prefix.is_empty()
                && prefix.chars().all(|c| c.is_ascii_alphabetic())
                && !suffix.is_empty()
                && suffix.chars().all(|c| c.is_ascii_alphanumeric())
                && (suffix.chars().any(|c| c.is_ascii_digit())
                    || (prefix.len() <= 4 && suffix.len() <= 4))
        });
        match self {
            Self::Text => "text",
            Self::Auto if id => "text",
            _ => "vector",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub entry: Entry,
    pub snippet: String,
}

#[derive(Debug, Default)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// A server failure means these results came from local text search.
    pub fallback: Option<String>,
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn search_request(query: &str, mode: Mode) -> Value {
    request(
        2,
        "tools/call",
        json!({
            "name": "search_notes",
            "arguments": {
                "query": query, "search_type": mode.search_type(query),
                "page_size": LIMIT, "output_format": "json", "project": "ledger"
            }
        }),
    )
}

/// Reads through SSE notifications until the matching response arrives. Does not wait for
/// an SSE connection to close after receiving the result. Plain JSON is accepted as well.
fn response(reader: impl BufRead, sse: bool, id: u64) -> Result<Value> {
    fn result(value: Value, id: u64) -> Result<Option<Value>> {
        if value.get("id") != Some(&json!(id)) {
            return Ok(None);
        }
        if let Some(error) = value.get("error") {
            bail!("Ledger MCP: {error}");
        }
        Ok(Some(
            value.get("result").context("Missing MCP result")?.clone(),
        ))
    }
    if !sse {
        return result(serde_json::from_reader(reader)?, id)?
            .context("MCP response id did not match");
    }
    let mut data = String::new();
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            if !data.is_empty() {
                if let Some(value) = result(serde_json::from_str(&data)?, id)? {
                    return Ok(value);
                }
                data.clear();
            }
        } else if let Some(part) = line.strip_prefix("data:") {
            data.push_str(part.strip_prefix(' ').unwrap_or(part));
            data.push('\n');
        }
        ensure!(
            data.len() <= 8 * 1024 * 1024,
            "Ledger response is too large"
        );
    }
    if !data.is_empty()
        && let Some(value) = result(serde_json::from_str(&data)?, id)?
    {
        return Ok(value);
    }
    bail!("MCP stream ended without a response")
}

fn tool_result(result: Value) -> Result<Value> {
    ensure!(result["isError"] != true, "Ledger search failed: {result}");
    if let Some(structured) = result.get("structuredContent") {
        return Ok(structured.get("result").unwrap_or(structured).clone());
    }
    let text = result["content"]
        .as_array()
        .context("Missing tool content")?
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&text).context("Invalid Ledger search results")
}

fn remote(query: &str, mode: Mode, endpoint: &str) -> Result<Vec<Hit>> {
    let agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .new_agent();
    let mut init = agent
        .post(endpoint)
        .header("Accept", "application/json, text/event-stream")
        .send_json(request(
            1,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL, "capabilities": {},
                "clientInfo": {"name": "stronghold-ledger", "version": env!("CARGO_PKG_VERSION")}
            }),
        ))?;
    let session = init
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|s| s.to_str().ok())
        .map(str::to_owned);
    let sse = is_sse(&init);
    let initialized = response(BufReader::new(init.body_mut().as_reader()), sse, 1)?;
    let protocol = initialized["protocolVersion"]
        .as_str()
        .context("Missing MCP protocol version")?;
    let post = || {
        let mut req = agent
            .post(endpoint)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", protocol);
        if let Some(session) = &session {
            req = req.header("Mcp-Session-Id", session);
        }
        req
    };
    let outcome = (|| {
        post().send_json(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        let mut reply = post().send_json(search_request(query, mode))?;
        let sse = is_sse(&reply);
        let result = response(BufReader::new(reply.body_mut().as_reader()), sse, 2)?;
        parse_hits(&tool_result(result)?)
    })();
    // Release the MCP session; this deletes no notes.
    if let Some(session) = &session {
        let _ = agent
            .delete(endpoint)
            .header("Mcp-Session-Id", session)
            .header("MCP-Protocol-Version", protocol)
            .config()
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .call();
    }
    outcome
}

fn is_sse(response: &ureq::http::Response<ureq::Body>) -> bool {
    response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
}

fn relative_note(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "md")
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn parse_hits(value: &Value) -> Result<Vec<Hit>> {
    let rows = value["results"]
        .as_array()
        .context("Missing search results array")?;
    let mut hits = Vec::new();
    for row in rows {
        let path = PathBuf::from(
            row["file_path"]
                .as_str()
                .context("Missing result file path")?,
        );
        ensure!(relative_note(&path), "Invalid Ledger file path");
        if hits.iter().any(|hit: &Hit| hit.entry.path == path) {
            continue;
        }
        let modified_ms = row["updated_at"]
            .as_str()
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            .map_or(0, |t| t.as_millisecond());
        hits.push(Hit {
            entry: Entry {
                title: row["title"]
                    .as_str()
                    .context("Missing result title")?
                    .to_owned(),
                folder: path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_string_lossy()
                    .into_owned(),
                kind: row["metadata"]["note_type"].as_str().map(str::to_owned),
                path,
                modified_ms,
            },
            snippet: snippet(
                row["matched_chunk"]
                    .as_str()
                    .or_else(|| row["content"].as_str())
                    .unwrap_or(""),
                "",
            ),
        });
    }
    Ok(hits)
}

/// Confinement also catches symlinks escaping the Ledger. Titles need not match file names.
pub fn note_path(root: &Path, path: &Path) -> Result<PathBuf> {
    ensure!(relative_note(path), "Invalid Ledger file path");
    let root = root
        .canonicalize()
        .context("Ledger folder is unavailable")?;
    let path = root
        .join(path)
        .canonicalize()
        .context("Entry file is unavailable")?;
    ensure!(path.starts_with(root), "Entry is outside the Ledger");
    Ok(path)
}

pub fn read(root: &Path, entry: &Entry) -> Result<String> {
    let text = std::fs::read_to_string(note_path(root, &entry.path)?)?;
    Ok(body(&text).to_owned())
}

fn body(text: &str) -> &str {
    if let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    {
        let mut offset = text.len() - rest.len();
        for line in rest.split_inclusive('\n') {
            offset += line.len();
            if line.trim() == "---" {
                return text[offset..].trim_start();
            }
        }
    }
    text
}

fn snippet(text: &str, query: &str) -> String {
    let text = body(text);
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let line = text.lines().find(|line| {
        !terms.is_empty() && terms.iter().any(|term| line.to_lowercase().contains(term))
    });
    let flat = line
        .unwrap_or(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out: String = flat.chars().take(240).collect();
    if flat.chars().count() > 240 {
        out.push('…');
    }
    out
}

pub fn newest(root: &Path, shelf: &Shelf) -> Results {
    Results {
        hits: shelf
            .newest
            .iter()
            .map(|entry| Hit {
                entry: entry.clone(),
                snippet: read(root, entry)
                    .map(|text| snippet(&text, ""))
                    .unwrap_or_default(),
            })
            .collect(),
        fallback: None,
    }
}

/// Case-insensitive literal terms, all required; no regex, shell, index or writes.
pub fn fallback(root: &Path, query: &str) -> Result<Vec<Hit>> {
    std::fs::read_dir(root).context("Ledger folder is unavailable")?;
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut files = Vec::new();
    super::collect(root, &mut files);
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut hits = Vec::new();
    for (path, modified_ms) in files {
        let entry = super::entry(root, &path, modified_ms);
        let Ok(text) = read(root, &entry) else {
            continue;
        };
        let haystack = format!("{} {} {text}", entry.title, entry.folder).to_lowercase();
        if terms.iter().all(|term| haystack.contains(term)) {
            hits.push(Hit {
                entry,
                snippet: snippet(&text, query),
            });
            if hits.len() == LIMIT {
                break;
            }
        }
    }
    Ok(hits)
}

pub fn search(root: &Path, query: &str, mode: Mode) -> Result<Results> {
    search_at(root, query, mode, ENDPOINT)
}

fn search_at(root: &Path, query: &str, mode: Mode, endpoint: &str) -> Result<Results> {
    match remote(query, mode, endpoint) {
        Ok(hits) => Ok(Results {
            hits,
            fallback: None,
        }),
        Err(error) => Ok(Results {
            hits: fallback(root, query)?,
            fallback: Some(error.to_string()),
        }),
    }
}

#[cfg(test)]
mod tests;
