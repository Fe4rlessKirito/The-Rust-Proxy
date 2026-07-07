pub(crate) const STREAM_CHUNK_CHARS: usize = 32;

/// Parse `<thinking>...</thinking>` and `<response>...</response>` from reply.
pub(crate) fn parse_thinking(reply: &str) -> (Option<String>, String) {
    let thinking_re = regex::Regex::new(r"(?s)<thinking>(.*?)</thinking>").unwrap();
    let response_re = regex::Regex::new(r"(?s)<response>(.*?)</response>").unwrap();
    let thinking = thinking_re
        .captures(reply)
        .map(|cap| cap[1].trim().to_string());
    let response = response_re
        .captures(reply)
        .map(|cap| cap[1].trim().to_string())
        .unwrap_or_else(|| reply.trim().to_string());
    (thinking, response)
}

pub(crate) fn split_stream_text(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if current.chars().count() >= STREAM_CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}
