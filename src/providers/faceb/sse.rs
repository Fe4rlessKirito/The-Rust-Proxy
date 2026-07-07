use anyhow::{anyhow, Result};
use futures::stream;
use futures::StreamExt;

use crate::providers::openai_compat::parse_sse_delta;

pub(super) fn stream_faceb_sse_response(
    response: reqwest::Response,
) -> impl futures::Stream<Item = Result<String>> {
    stream::unfold(
        (response.bytes_stream(), String::new(), false),
        |(mut bytes, mut buffer, mut done)| async move {
            loop {
                if done {
                    return None;
                }
                if let Some((line, rest)) = take_line(&buffer) {
                    buffer = rest;
                    if let Some(delta) = parse_sse_line(&line) {
                        return Some((Ok(delta), (bytes, buffer, done)));
                    }
                    continue;
                }
                match bytes.next().await {
                    Some(Ok(chunk)) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
                    Some(Err(err)) => {
                        done = true;
                        return Some((
                            Err(anyhow!("Faceb stream read failed: {}", err)),
                            (bytes, buffer, done),
                        ));
                    }
                    None => {
                        done = true;
                        if !buffer.trim().is_empty() {
                            if let Some(delta) = parse_sse_line(&buffer) {
                                return Some((Ok(delta), (bytes, String::new(), done)));
                            }
                        }
                        return None;
                    }
                }
            }
        },
    )
}

fn take_line(buffer: &str) -> Option<(String, String)> {
    let idx = buffer.find('\n')?;
    Some((
        buffer[..idx].trim_end_matches('\r').to_string(),
        buffer[idx + 1..].to_string(),
    ))
}

fn parse_sse_line(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("data:") {
        return None;
    }
    let payload = line.trim_start_matches("data:").trim();
    if payload == "[DONE]" || payload.is_empty() {
        return None;
    }
    parse_sse_delta(payload)
}
