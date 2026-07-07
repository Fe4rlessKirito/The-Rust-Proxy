use serde_json::{json, Value};

use super::format::split_stream_text;

const TOOL_STREAM_GUARD_CHARS: usize = 96;
const TOOL_PROMPT: &str = r#"You may be given tools.

When tools are available and the task requires reading, searching, creating, editing, patching, or inspecting files, respond with one or more tool calls.

Rules:
- Output tool calls using the supported format below.
- You may include one short user-visible status line before the thinking/tool-call sequence only when it adds meaningful progress, an assumption, or a blocker.
- Do not narrate routine reads, searches, edits, or obvious next steps.
- Do not say you lack tool access.
- Do not describe limitations.
- Do not wrap the tool call in markdown fences.
- Do not include any prose after a tool call.
- The tool call must be valid JSON.
- Escape backslashes in Windows paths.
- Escape quotes and newlines correctly in JSON strings.

Use this exact tool-call format:

<tool_use>
{"name":"tool_name","input":{"key":"value"}}
</tool_use>

If you need to communicate before continuing to tool calls, use this supported pattern:

Short user-visible status line.
<thinking>brief private reasoning about the next tool step</thinking>
<tool_use>
{"name":"tool_name","input":{"key":"value"}}
</tool_use>

After a tool result is provided, either output one or more next tool calls in the same format or answer the user normally if no more tools are needed."#;

pub fn tools_prompt(tools: &[Value], tool_choice: Option<&Value>) -> String {
    let mut prompt = String::from(TOOL_PROMPT);
    prompt.push_str("\n\nAvailable tools:\n");
    prompt.push_str(&serde_json::to_string_pretty(tools).unwrap_or_else(|_| "[]".to_string()));
    if let Some(choice) = tool_choice {
        prompt.push_str("\n\nTool choice:\n");
        prompt.push_str(&choice.to_string());
    }
    prompt
}

pub fn looks_like_tool_prompt(value: &Value) -> bool {
    let text = value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
        .to_lowercase();

    text.contains("available tools")
        || text.contains("<tool_use>")
        || text.contains("tool_choice")
        || text.contains("tool call")
        || text.contains("function_call")
}

pub fn mark_trusted_tool_prompt(message: &mut Value) {
    if let Some(obj) = message.as_object_mut() {
        let metadata = obj.entry("metadata").or_insert_with(|| json!({}));
        if let Some(metadata_obj) = metadata.as_object_mut() {
            metadata_obj.insert("leech_proxy_tool_prompt".to_string(), Value::Bool(true));
        }
    }
}

pub fn has_trusted_tool_prompt(messages: &[Value]) -> bool {
    messages.iter().any(|message| {
        message
            .get("metadata")
            .and_then(|m| m.get("leech_proxy_tool_prompt"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    })
}

fn strip_runtime_tags(reply: &str) -> String {
    let mut cleaned = reply.to_string();
    for tag in [
        "system_reminder",
        "system-reminder",
        "system",
        "reminder",
        "context",
    ] {
        let pattern = format!(r"(?is)<{tag}[^>]*>.*?</{tag}>");
        cleaned = regex::Regex::new(&pattern)
            .unwrap()
            .replace_all(&cleaned, "")
            .to_string();
    }
    cleaned.trim().to_string()
}

fn extract_first_json_object(text: &str) -> Option<Value> {
    let mut start_idx = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;

    for (idx, ch) in text.char_indices() {
        if start_idx.is_none() {
            if ch == '{' {
                start_idx = Some(idx);
                depth = 1;
                in_string = false;
                escape = false;
            }
            continue;
        }

        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let start = start_idx?;
                    let candidate = &text[start..=idx];
                    if let Ok(value) = serde_json::from_str::<Value>(candidate) {
                        return Some(value);
                    }
                    start_idx = None;
                }
            }
            _ => {}
        }
    }

    None
}

fn extract_fenced_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))?;
    let body = stripped.trim();
    let body = body.strip_suffix("```")?.trim();
    serde_json::from_str::<Value>(body)
        .ok()
        .or_else(|| extract_first_json_object(body))
}

fn tool_value_to_call(value: &Value) -> Option<(String, Value)> {
    let name = value.get("name")?.as_str()?.to_string();
    let input = value.get("input").cloned().unwrap_or_else(|| json!({}));
    Some((name, input))
}

fn parse_all_tagged_json(reply: &str, tag: &str) -> Vec<Value> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let mut out = Vec::new();
    let mut cursor = 0usize;

    while let Some(start_rel) = reply[cursor..].find(&open) {
        let start = cursor + start_rel + open.len();
        let Some(end_rel) = reply[start..].find(&close) else {
            break;
        };
        let end = start + end_rel;
        let body = reply[start..end].trim();
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            out.push(value);
        }
        cursor = end + close.len();
    }

    out
}

pub fn parse_tool_uses(reply: &str) -> Vec<(String, Value)> {
    let cleaned = strip_runtime_tags(reply);
    let mut calls = Vec::new();

    for value in parse_all_tagged_json(&cleaned, "tool_use") {
        if let Some(call) = tool_value_to_call(&value) {
            calls.push(call);
        }
    }

    if !calls.is_empty() {
        return calls;
    }

    if let Ok(value) = serde_json::from_str::<Value>(cleaned.trim()) {
        if let Some(call) = tool_value_to_call(&value) {
            calls.push(call);
        }
    }

    if calls.is_empty() {
        if let Some(value) = extract_fenced_json(&cleaned) {
            if let Some(call) = tool_value_to_call(&value) {
                calls.push(call);
            }
        }
    }

    if calls.is_empty() {
        if let Some(value) = extract_first_json_object(&cleaned) {
            if let Some(call) = tool_value_to_call(&value) {
                calls.push(call);
            }
        }
    }

    calls
}

fn parse_tool_use(reply: &str) -> Option<(String, Value)> {
    parse_tool_uses(reply).into_iter().next()
}

pub fn looks_like_tool_call(reply: &str) -> bool {
    let cleaned = strip_runtime_tags(reply);
    let lower = cleaned.to_lowercase();
    lower.contains("\"name\"")
        && (lower.contains("\"input\"")
            || lower.contains("\"filepath\"")
            || lower.contains("\"patchtext\"")
            || lower.contains("\"old_string\"")
            || lower.contains("\"new_string\""))
        || lower.contains("<tool_use>")
        || lower.contains("```json")
}

pub fn looks_like_tool_refusal(reply: &str) -> bool {
    let cleaned = strip_runtime_tags(reply);
    let lower = cleaned.to_lowercase();
    lower.contains("i can't inspect")
        || lower.contains("i cant inspect")
        || lower.contains("i can’t inspect")
        || lower.contains("i canâ€™t inspect")
        || lower.contains("i can't access")
        || lower.contains("i cant access")
        || lower.contains("i can’t access")
        || lower.contains("i don't have access")
        || lower.contains("i dont have access")
        || lower.contains("i do not have access")
        || lower.contains("available in this workspace")
        || lower.contains("from here unless")
}

pub fn is_tool_call_incomplete(reply: &str) -> bool {
    let trimmed = strip_runtime_tags(reply);
    (trimmed.contains("<\u{200B}tool_use>") && !trimmed.contains("<\u{200B}/tool_use>"))
        || (looks_like_tool_call(&trimmed) && parse_tool_use(&trimmed).is_none())
}

#[derive(Default)]
pub struct ToolModeStreamBuffer {
    reply: String,
    pending: String,
    normal_text: bool,
}

impl ToolModeStreamBuffer {
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.reply.push_str(text);
        if self.normal_text {
            return split_stream_text(text);
        }

        self.pending.push_str(text);
        let trimmed = self.pending.trim_start();
        if starts_like_tool_syntax(trimmed) {
            return Vec::new();
        }

        if self.pending.chars().count() <= TOOL_STREAM_GUARD_CHARS
            && maybe_tool_syntax_prefix(trimmed)
        {
            return Vec::new();
        }

        self.normal_text = true;
        let pending = std::mem::take(&mut self.pending);
        split_stream_text(&pending)
    }

    pub fn finish(self) -> (String, Vec<String>) {
        if self.normal_text || self.pending.is_empty() {
            (self.reply, Vec::new())
        } else {
            let pending = self.pending;
            (self.reply, split_stream_text(&pending))
        }
    }
}

fn starts_like_tool_syntax(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("<\u{200B}tool_use")
        || lower.starts_with("<tool_use")
        || lower.starts_with("```json")
        || lower.starts_with('{')
        || lower.starts_with('[')
}

fn maybe_tool_syntax_prefix(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let lower = text.to_lowercase();
    ["<\u{200B}tool_use", "<tool_use", "```json", "{", "["]
        .iter()
        .any(|tag| tag.starts_with(&lower))
        || lower.starts_with("<\u{200B}thinking")
        || lower.starts_with("<thinking")
}

fn normalize_openai_tool_schema(tool: &Value) -> Option<Value> {
    let function = tool.get("function")?;
    let name = function.get("name")?.as_str()?;
    Some(json!({
        "name": name,
        "description": function
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "input_schema": function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
    }))
}

pub fn normalize_tools_for_prompt(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let tool_type = tool.get("type").and_then(|v| v.as_str());
            if tool_type == Some("function") {
                normalize_openai_tool_schema(tool).unwrap_or_else(|| tool.clone())
            } else {
                tool.clone()
            }
        })
        .collect()
}

pub fn tool_choice_to_prompt_value(tool_choice: Option<&Value>) -> Option<Value> {
    match tool_choice {
        Some(Value::Object(map)) => {
            if map.get("type").and_then(|v| v.as_str()) == Some("function") {
                if let Some(name) = map
                    .get("function")
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                {
                    return Some(json!({ "type": "tool", "name": name }));
                }
            }
            Some(Value::Object(map.clone()))
        }
        Some(other) => Some(other.clone()),
        None => None,
    }
}

pub fn normalize_tool_messages(messages: &mut [Value]) {
    for msg in messages {
        if msg.get("role").and_then(|v| v.as_str()) == Some("tool") {
            let tool_call_id = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
                .or_else(|| msg.get("content").map(|v| v.to_string()))
                .unwrap_or_default();
            *msg = json!({
                "role": "user",
                "content": format!("Tool result for {}:\n{}", tool_call_id, content),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_marks_tool_like_system_prompt() {
        let mut message = json!({
            "role": "system",
            "content": "Available tools:\n<tool_use>{\"name\":\"read_file\",\"input\":{}}</tool_use>"
        });

        assert!(looks_like_tool_prompt(&message["content"]));
        mark_trusted_tool_prompt(&mut message);
        assert_eq!(
            message["metadata"]["leech_proxy_tool_prompt"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn tool_prompt_allows_status_before_thinking_and_tool_use() {
        let prompt = tools_prompt(
            &[json!({
                "name": "read_file",
                "input_schema": {"type": "object"}
            })],
            None,
        );

        assert!(prompt.contains("one short user-visible status line"));
        assert!(prompt.contains("Short user-visible status line."));
        assert!(prompt
            .contains("<thinking>brief private reasoning about the next tool step</thinking>"));
        assert!(prompt.contains("<tool_use>"));
        assert!(!prompt.contains("Do not include any text before or after the tool call"));
    }
}
