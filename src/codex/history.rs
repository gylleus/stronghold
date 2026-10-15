//! Codex rollout response items adapted to the shared conversation model. Event messages
//! repeat the same user/assistant text, so only response items contribute visible messages.

use serde_json::{Value, json};

use crate::chat::history::HistoryRecord;
use crate::chat::protocol::{ContentBlock, Incoming};

pub fn records(record: &Value) -> Vec<HistoryRecord> {
    let payload = &record["payload"];
    match record["type"].as_str() {
        Some("turn_context") => payload["model"]
            .as_str()
            .map(|model| assistant(payload, Some(model.to_owned()), vec![]))
            .into_iter()
            .collect(),
        Some("compacted") => vec![HistoryRecord::Notice(
            crate::chat::history::COMPACTED_NOTICE.to_owned(),
        )],
        Some("response_item") => response(payload),
        Some("event_msg") if payload["type"] == "turn_aborted" => {
            vec![HistoryRecord::Notice("Interrupted".into())]
        }
        _ => vec![],
    }
}

fn response(payload: &Value) -> Vec<HistoryRecord> {
    let string = |key| payload[key].as_str().unwrap_or_default().to_owned();
    match payload["type"].as_str() {
        Some("message") => {
            let text = content_text(&payload["content"]);
            if text.trim().is_empty() {
                return vec![];
            }
            match payload["role"].as_str() {
                Some("user") if !injected(&text) => vec![HistoryRecord::Prompt(text)],
                Some("assistant") if payload["channel"] != "analysis" => {
                    vec![assistant(payload, None, vec![ContentBlock::Text(text)])]
                }
                _ => vec![],
            }
        }
        Some("function_call" | "custom_tool_call") => {
            let id = string("call_id");
            if id.is_empty() {
                return vec![];
            }
            let input = if payload["type"] == "function_call" {
                serde_json::from_str(payload["arguments"].as_str().unwrap_or(""))
                    .unwrap_or_else(|_| json!({"arguments": string("arguments")}))
            } else {
                json!({"input": string("input")})
            };
            vec![assistant(
                payload,
                None,
                vec![ContentBlock::ToolUse {
                    id,
                    name: string("name"),
                    input,
                }],
            )]
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            let output = &payload["output"];
            let text = content_text(output);
            let is_error = payload["is_error"].as_bool() == Some(true)
                || output["is_error"].as_bool() == Some(true)
                || output
                    .pointer("/metadata/exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|c| c != 0)
                || text.lines().any(|line| {
                    line.strip_prefix("Process exited with code ")
                        .or_else(|| line.strip_prefix("Process exit code: "))
                        .and_then(|code| code.trim().parse::<i64>().ok())
                        .is_some_and(|code| code != 0)
                });
            vec![HistoryRecord::Message(Incoming::User {
                parent: None,
                content: vec![crate::chat::history::shrink_block(
                    ContentBlock::ToolResult {
                        tool_use_id: string("call_id"),
                        text,
                        is_error,
                    },
                )],
            })]
        }
        _ => vec![],
    }
}

fn assistant(payload: &Value, model: Option<String>, content: Vec<ContentBlock>) -> HistoryRecord {
    HistoryRecord::Message(Incoming::Assistant {
        parent: None,
        message_id: payload["id"].as_str().unwrap_or_default().into(),
        model,
        content: content
            .into_iter()
            .map(crate::chat::history::shrink_block)
            .collect(),
        usage: None,
    })
}

fn content_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block["type"].as_str() {
                Some("input_text" | "output_text" | "text") => block["text"].as_str(),
                Some("input_image" | "image") => Some("[image]"),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => value["output"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
        _ => String::new(),
    }
}

fn injected(text: &str) -> bool {
    let text = text.trim_start();
    [
        "# AGENTS.md instructions for ",
        "<environment_context>",
        "<permissions instructions>",
        "<system-reminder>",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::history::{HistoryReader, parse_record};
    use crate::chat::{ChatItem, Transcript};
    use std::io::Write;

    fn apply(t: &mut Transcript, record: Value) {
        for r in parse_record(&record) {
            t.apply_history(&r);
        }
    }

    #[test]
    fn skips_harness_reasoning_and_duplicate_events_but_keeps_images_and_interrupts() {
        let mut t = Transcript::default();
        for record in [
            json!({"type":"session_meta", "payload":{"base_instructions":"hidden"}}),
            json!({"type":"response_item", "payload":{"type":"message", "role":"developer", "content":[{"type":"input_text", "text":"hidden"}]}}),
            json!({"type":"response_item", "payload":{"type":"message", "role":"user", "content":[{"type":"input_text", "text":"# AGENTS.md instructions for /repo\nhidden"}]}}),
            json!({"type":"response_item", "payload":{"type":"reasoning", "summary":[{"text":"hidden"}]}}),
            json!({"type":"response_item", "payload":{"type":"message", "role":"assistant", "channel":"analysis", "content":[{"type":"output_text", "text":"hidden"}]}}),
            json!({"type":"event_msg", "payload":{"type":"agent_message", "message":"duplicate"}}),
            json!({"type":"event_msg", "payload":{"type":"user_message", "message":"duplicate"}}),
        ] {
            apply(&mut t, record);
        }
        assert!(t.items.is_empty());
        apply(
            &mut t,
            json!({"type":"response_item", "payload":{"type":"message", "role":"user", "content":[{"type":"input_text", "text":"Check this"}, {"type":"input_image", "image_url":"ignored"}]}}),
        );
        apply(
            &mut t,
            json!({"type":"event_msg", "payload":{"type":"turn_aborted"}}),
        );
        apply(
            &mut t,
            json!({"type":"compacted", "payload":{"message":"internal summary"}}),
        );
        assert_eq!(
            t.items,
            vec![
                ChatItem::User("Check this\n[image]".into()),
                ChatItem::Notice("Interrupted".into()),
                ChatItem::Notice(crate::chat::history::COMPACTED_NOTICE.into())
            ]
        );
    }

    #[test]
    fn failed_tools_and_malformed_arguments_stay_readable_and_bounded() {
        let mut t = Transcript::default();
        apply(
            &mut t,
            json!({"type":"response_item", "payload":{"type":"function_call", "call_id":"t", "name":"exec_command", "arguments":"{partial"}}),
        );
        apply(
            &mut t,
            json!({"type":"response_item", "payload":{"type":"function_call_output", "call_id":"t", "output":format!("Process exited with code 1\n{}", "ö".repeat(9000))}}),
        );
        let tool = t.tool_calls().next().unwrap();
        assert_eq!(tool.input.as_ref().unwrap()["arguments"], "{partial");
        let result = tool.result.as_ref().unwrap();
        assert!(result.is_error);
        assert_eq!(
            result.text.chars().count(),
            crate::chat::history::MAX_KEPT_CHARS + 1
        );
    }

    #[test]
    fn rollout_reader_applies_appended_results_partial_lines_and_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rollout.jsonl");
        std::fs::write(&path, concat!("{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"exec_command\",\"call_id\":\"t\",\"arguments\":\"{}\"}}\n", "broken json\n")).unwrap();
        let mut reader = HistoryReader::new(&path);
        let mut view = crate::ui::chat::TranscriptView::new(path.clone());
        assert!(view.apply(&reader.read_new().unwrap().unwrap()));
        assert_eq!(view.transcript.items.len(), 1);
        let line = json!({"type":"response_item", "payload":{"type":"function_call_output", "call_id":"t", "output":[{"type":"input_text", "text":"done"}]}}).to_string();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(file, "{}", &line[..20]).unwrap();
        view.apply(&reader.read_new().unwrap().unwrap());
        assert!(
            view.transcript
                .tool_calls()
                .next()
                .unwrap()
                .result
                .is_none()
        );
        writeln!(file, "{}", &line[20..]).unwrap();
        view.apply(&reader.read_new().unwrap().unwrap());
        assert_eq!(
            view.transcript
                .tool_calls()
                .next()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .text,
            "done"
        );
        assert!(reader.read_new().unwrap().is_none());
        std::fs::write(&path, "{\"type\":\"compacted\",\"payload\":{}}\n").unwrap();
        let reset = reader.read_new().unwrap().unwrap();
        assert!(reset.reset);
        view.apply(&reset);
        assert_eq!(
            view.transcript.items,
            vec![ChatItem::Notice(
                crate::chat::history::COMPACTED_NOTICE.into()
            )]
        );
    }
}
