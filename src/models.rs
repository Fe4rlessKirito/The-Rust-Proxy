//! Model definitions and resolution.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Full model catalog (same as Python config).
pub const MODELS: &[(&str, &str)] = &[
    // OpenAI
    ("gpt-5-5", "OpenAI GPT-5.5"),
    ("gpt-5-4", "OpenAI GPT-5.4"),
    ("gpt-5-3", "OpenAI GPT-5.3"),
    ("gpt-5-1", "OpenAI GPT-5.1"),
    ("gpt-5", "OpenAI GPT-5"),
    ("gpt-5-mini", "OpenAI GPT-5 Mini"),
    ("gpt-4o", "OpenAI GPT-4o"),
    ("gpt-4o-mini", "OpenAI GPT-4o Mini"),
    // Anthropic
    ("claude-opus-4-8", "Claude Opus 4.8"),
    ("claude-opus-4-7", "Claude Opus 4.7"),
    ("claude-opus-4-6", "Claude Opus 4.6"),
    ("claude-opus-4-5", "Claude Opus 4.5"),
    ("claude-opus-4-1", "Claude Opus 4.1"),
    ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
    // Google
    ("gemini-3-1-pro", "Gemini 3.1 Pro"),
    ("gemini-3-pro", "Gemini 3 Pro"),
    ("gemini-3-flash", "Gemini 3 Flash"),
    ("gemini-2.5-flash", "Gemini 2.5 Flash"),
    // DeepSeek
    ("deepseek-v4-pro", "DeepSeek V4 Pro"),
    ("deepseek-v4-flash", "DeepSeek V4 Flash"),
    ("deepseek-r1", "DeepSeek R1"),
    // Others
    ("grok-4", "Grok 4"),
    ("qwen-3-max", "Qwen 3 Max"),
    ("qwen-3-5-397b", "Qwen 3.5"),
    ("kimi-k2-6", "Kimi K2.6"),
    ("deepinfra-kimi-k2", "Kimi K2"),
    ("llama-3-3-70b-versatile", "Llama 3.3"),
];

/// Aliases for convenience.
pub const ALIASES: &[(&str, &str)] = &[
    ("default", "gpt-5-4"),
    ("fast", "gpt-5-mini"),
    ("smart", "claude-opus-4-8"),
];

static ALIAS_MAP: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| ALIASES.iter().cloned().collect());
static MODEL_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| MODELS.iter().map(|(slug, _)| *slug).collect());

/// Resolve a model name (slug or alias) to a real slug.
pub fn resolve_model(name: &str) -> String {
    if MODEL_SET.contains(name) {
        name.to_string()
    } else if let Some(&alias) = ALIAS_MAP.get(name) {
        alias.to_string()
    } else {
        "gpt-5-4".to_string()
    }
}

/// Get the full model list for the /v1/models endpoint.
pub fn model_list() -> Vec<serde_json::Value> {
    MODELS
        .iter()
        .map(|(slug, label)| {
            serde_json::json!({
                "id": slug,
                "object": "model",
                "owned_by": "leech",
                "label": label,
            })
        })
        .collect()
}
