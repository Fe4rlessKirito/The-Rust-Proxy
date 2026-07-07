use serde_json::Value;

pub fn extract_chat_content(data: &Value) -> Option<String> {
    data.get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")
        .and_then(extract_content_value)
}

pub fn parse_sse_delta(payload: &str) -> Option<String> {
    let obj = serde_json::from_str::<Value>(payload).ok()?;
    obj.get("choices")?
        .as_array()?
        .first()?
        .get("delta")?
        .get("content")
        .and_then(extract_content_value)
        .filter(|delta| !delta.is_empty())
}

fn extract_content_value(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join(""),
        ),
        other if !other.is_null() => Some(other.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_string_chat_content() {
        let data = json!({
            "choices": [{
                "message": {
                    "content": "hello"
                }
            }]
        });

        assert_eq!(extract_chat_content(&data), Some("hello".to_string()));
    }

    #[test]
    fn extracts_array_chat_content() {
        let data = json!({
            "choices": [{
                "message": {
                    "content": [{"text": "hel"}, {"text": "lo"}]
                }
            }]
        });

        assert_eq!(extract_chat_content(&data), Some("hello".to_string()));
    }

    #[test]
    fn parses_sse_delta_content() {
        let payload = r#"{"choices":[{"delta":{"content":"hello"}}]}"#;

        assert_eq!(parse_sse_delta(payload), Some("hello".to_string()));
    }
}
