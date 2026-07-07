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
    ("claude-sonnet-5", "Claude Sonnet 5"),
    ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
    ("claude-sonnet-4-5", "Claude Sonnet 4.5"),
    ("claude-haiku-4-5", "Claude Haiku 4.5"),
    ("claude-haiku-4", "Claude Haiku 4"),
    // Sakana
    ("sakana-namazu", "Sakana Namazu"),
    ("sakana-fugu", "Sakana Fugu"),
    ("sakana-fugu-ultra", "Sakana Fugu Ultra"),
    // Faceb.ai
    ("faceb-openai/gpt-5", "Faceb GPT-5"),
    ("faceb-openai/gpt-5.1", "Faceb GPT-5.1"),
    ("faceb-openai/gpt-5.2", "Faceb GPT-5.2"),
    ("faceb-openai/gpt-5.3-chat", "Faceb GPT-5.3 Chat"),
    ("faceb-openai/gpt-5.4", "Faceb GPT-5.4"),
    ("faceb-openai/gpt-5.5", "Faceb GPT-5.5"),
    ("faceb-anthropic/claude-opus-4", "Faceb Claude Opus 4"),
    ("faceb-anthropic/claude-opus-4.1", "Faceb Claude Opus 4.1"),
    ("faceb-anthropic/claude-opus-4.5", "Faceb Claude Opus 4.5"),
    ("faceb-anthropic/claude-opus-4.6", "Faceb Claude Opus 4.6"),
    ("faceb-anthropic/claude-opus-4.7", "Faceb Claude Opus 4.7"),
    ("faceb-anthropic/claude-opus-4.8", "Faceb Claude Opus 4.8"),
    ("faceb-anthropic/claude-fable-5", "Faceb Claude Fable 5"),
    (
        "faceb-google/gemini-3.1-pro-preview",
        "Faceb Gemini 3.1 Pro",
    ),
    ("faceb-google/gemini-3.5-flash", "Faceb Gemini 3.5 Flash"),
    ("faceb-google/gemini-2.5-pro", "Faceb Gemini 2.5 Pro"),
    ("faceb-google/gemini-2.5-flash", "Faceb Gemini 2.5 Flash"),
    ("faceb-qwen/qwen3-max", "Faceb Qwen3 Max"),
    ("faceb-qwen/qwen3-coder", "Faceb Qwen3 Coder"),
    ("faceb-qwen/qwen3-coder-plus", "Faceb Qwen3 Coder Plus"),
    ("faceb-qwen/qwen3.5-397b-a17b", "Faceb Qwen3.5 397B"),
    ("faceb-mistralai/ministral-14b-2512", "Faceb Ministral 14B"),
    ("faceb-mistralai/ministral-8b-2512", "Faceb Ministral 8B"),
    ("faceb-mistralai/ministral-3b-2512", "Faceb Ministral 3B"),
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
    ("glm-5-2", "GLM 5.2"),
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
    ("smart", "claude-sonnet-4-6"),
];

static ALIAS_MAP: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| ALIASES.iter().cloned().collect());
static MODEL_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| MODELS.iter().map(|(slug, _)| *slug).collect());

/// Resolve a model name (slug or alias) to a real slug.
pub fn resolve_model(name: &str) -> String {
    if name.starts_with("faceb-") || MODEL_SET.contains(name) {
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
