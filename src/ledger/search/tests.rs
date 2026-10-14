use super::*;
use std::io::Cursor;

#[test]
fn requests_use_meaning_for_questions_and_text_for_ids() {
    for (query, expected) in [
        ("Where does the clan keep memories?", "vector"),
        ("VROOM-1299", "text"),
        ("myr-bas", "text"),
        ("basic-memory setup", "vector"),
        ("basic-memory", "vector"),
    ] {
        let request = search_request(query, Mode::Auto);
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["name"], "search_notes");
        assert_eq!(request["params"]["arguments"]["query"], query);
        assert_eq!(request["params"]["arguments"]["search_type"], expected);
        assert_eq!(request["params"]["arguments"]["output_format"], "json");
        assert_eq!(request["params"]["arguments"]["project"], "ledger");
    }
    assert_eq!(Mode::Meaning.search_type("VROOM-1299"), "vector");
    assert_eq!(Mode::Text.search_type("any question"), "text");
}

#[test]
fn parses_json_and_sse_with_notifications_multiline_data_and_crlf() {
    let value = json!({"jsonrpc":"2.0", "id":2, "result":{"content":[]}});
    let plain = response(Cursor::new(value.to_string()), false, 2).unwrap();
    let stream = concat!(
        ": keepalive\r\n\r\n",
        "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\r\n\r\n",
        "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\r\n",
        "data: \"result\":{\"content\":[]}}\r\n\r\n",
        "data: broken trailing event\n\n"
    );
    assert_eq!(response(Cursor::new(stream), true, 2).unwrap(), plain);
    assert!(response(Cursor::new(value.to_string()), false, 9).is_err());
    assert!(response(Cursor::new("data: nope\n\n"), true, 2).is_err());
    assert!(response(Cursor::new("event: message\n\n"), true, 2).is_err());
    let error = json!({"id":2,"error":{"code":-32000,"message":"offline"}});
    assert!(
        response(Cursor::new(error.to_string()), false, 2)
            .unwrap_err()
            .to_string()
            .contains("offline")
    );
}

fn row() -> Value {
    json!({
        "title":"A different title", "type":"entity", "file_path":"repo/actual.md",
        "updated_at":"2026-10-14T09:30:00+02:00", "metadata":{"note_type":"finding"},
        "content":"Full entry", "matched_chunk":"Relevant snippet"
    })
}

#[test]
fn tool_envelopes_and_results_preserve_paths_and_note_types() {
    let payload = json!({"results":[row(), row()]});
    for wrapper in [
        json!({"content":[{"type":"text","text":payload.to_string()}]}),
        json!({"structuredContent":{"result":payload}}),
        json!({"structuredContent":payload}),
    ] {
        let hits = parse_hits(&tool_result(wrapper).unwrap()).unwrap();
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.entry.title, "A different title");
        assert_eq!(hit.entry.path, Path::new("repo/actual.md"));
        assert_eq!(hit.entry.folder, "repo");
        assert_eq!(hit.entry.kind.as_deref(), Some("finding"));
        assert_eq!(
            hit.entry.modified_ms,
            "2026-10-14T07:30:00Z"
                .parse::<jiff::Timestamp>()
                .unwrap()
                .as_millisecond()
        );
        assert_eq!(hit.snippet, "Relevant snippet");
    }
    assert!(tool_result(json!({"isError":true,"content":[]})).is_err());
    assert!(tool_result(json!({"content":[{"text":"not json"}]})).is_err());
    assert!(parse_hits(&json!({"error":"broken"})).is_err());
    assert!(parse_hits(&json!({"results":[]})).unwrap().is_empty());
}

#[test]
fn rejects_paths_outside_the_ledger() {
    for path in [
        "../secret.md",
        "/tmp/secret.md",
        "repo/../../secret.md",
        "repo/tool.sh",
    ] {
        let mut row = row();
        row["file_path"] = json!(path);
        assert!(parse_hits(&json!({"results":[row]})).is_err(), "{path}");
    }
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("outside.md"), "outside").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            elsewhere.path().join("outside.md"),
            root.path().join("escape.md"),
        )
        .unwrap();
        std::os::unix::fs::symlink(root.path(), root.path().join("cycle")).unwrap();
        assert!(note_path(root.path(), Path::new("escape.md")).is_err());
        assert!(fallback(root.path(), "outside").unwrap().is_empty());
    }
}

#[test]
fn fallback_searches_literal_terms_and_reads_real_filenames_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("repo")).unwrap();
    let content = "---\ntitle: A different title\ntype: decision\n---\n\n# Notes\nFixed VROOM-1299 with café [literal].\n";
    let file = root.join("repo/actual.md");
    std::fs::write(&file, content).unwrap();
    std::fs::write(root.join("other.md"), "Unrelated text").unwrap();
    std::fs::write(root.join(".hidden.md"), "VROOM-1299").unwrap();
    std::fs::write(root.join("ignored.txt"), "VROOM-1299").unwrap();
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    let hits = fallback(root, "vroom-1299 CAFÉ").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.title, "A different title");
    assert_eq!(hits[0].entry.kind.as_deref(), Some("decision"));
    assert!(hits[0].snippet.contains("VROOM-1299"));
    assert!(!hits[0].snippet.contains("title:"));
    assert!(read(root, &hits[0].entry).unwrap().starts_with("# Notes"));
    assert_eq!(fallback(root, "different repo").unwrap().len(), 1);
    assert_eq!(fallback(root, "[literal]").unwrap().len(), 1);
    assert!(fallback(root, "nonexistent").unwrap().is_empty());
    assert!(fallback(&root.join("missing"), "x").is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), content);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
    let shelf = super::super::scan(root);
    let recent = newest(root, &shelf);
    assert_eq!(recent.hits.len(), 2);
    assert!(
        recent
            .hits
            .iter()
            .any(|h| h.entry.path == Path::new("repo/actual.md"))
    );
}

#[test]
fn unavailable_server_uses_local_search_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.md"), "fallback needle").unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = socket.local_addr().unwrap();
    drop(socket);
    let results = search_at(
        dir.path(),
        "needle",
        Mode::Auto,
        &format!("http://{addr}/mcp"),
    )
    .unwrap();
    assert!(results.fallback.is_some());
    assert_eq!(results.hits.len(), 1);
}

#[test]
fn snippets_are_unicode_safe_and_strip_crlf_frontmatter() {
    let text = format!("---\r\ntitle: Hidden\r\n---\r\n\r\n{}", "å".repeat(300));
    let snippet = snippet(&text, "");
    assert_eq!(snippet.chars().count(), 241);
    assert!(snippet.ends_with('…'));
    assert!(!snippet.contains("Hidden"));
}

#[test]
#[ignore = "requires the local Ledger MCP service on port 8643"]
fn real_ledger_search() {
    let query = "Where does the clan keep shared knowledge?";
    let hits = remote(query, Mode::Meaning, ENDPOINT).unwrap();
    println!("Query: {query}\n{} results", hits.len());
    assert!(!hits.is_empty());
    for hit in hits.iter().take(5) {
        println!(
            "{} | {} | {}\n  {}",
            hit.entry.title,
            hit.entry.folder,
            hit.entry.kind.as_deref().unwrap_or("entry"),
            hit.snippet
        );
    }
    let root = super::super::default_dir().unwrap();
    let markdown = read(&root, &hits[0].entry).unwrap();
    assert!(!markdown.is_empty());
    println!("Read top entry: {} bytes", markdown.len());
}

#[test]
fn transport_initializes_and_keeps_the_session_for_read_only_search() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for step in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                assert!(!line.is_empty());
                headers.push_str(&line);
            }
            let lower = headers.to_lowercase();
            let len = lower
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .map(|s| s.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            if step > 0 {
                assert!(lower.contains("mcp-session-id: ledger-test"));
                assert!(lower.contains("mcp-protocol-version: 2025-03-26"));
            }
            let (status, extra, content) = match step {
                0 => {
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(request["method"], "initialize");
                    (
                        "200 OK",
                        "Mcp-Session-Id: ledger-test\r\nContent-Type: application/json\r\n",
                        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":PROTOCOL}})
                            .to_string(),
                    )
                }
                1 => {
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(request["method"], "notifications/initialized");
                    assert!(request.get("id").is_none());
                    ("202 Accepted", "", String::new())
                }
                2 => {
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(request, search_request("clan memory", Mode::Meaning));
                    let payload = json!({"results":[row()]});
                    let response = json!({"jsonrpc":"2.0","id":2,"result":{
                        "content":[{"type":"text","text":payload.to_string()}]
                    }});
                    (
                        "200 OK",
                        "Content-Type: text/event-stream\r\n",
                        format!("event: message\ndata: {response}\n\n"),
                    )
                }
                _ => {
                    assert!(headers.starts_with("DELETE /mcp "));
                    ("200 OK", "", String::new())
                }
            };
            write!(stream, "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{content}", content.len()).unwrap();
        }
    });
    let outcome = remote("clan memory", Mode::Meaning, &format!("http://{addr}/mcp"));
    server.join().unwrap();
    assert_eq!(outcome.unwrap()[0].entry.title, "A different title");
}
