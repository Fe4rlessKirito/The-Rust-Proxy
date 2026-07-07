//! Provider routing and shared completion interface.

pub mod faceb;
pub mod freemodel;
pub mod groq;
mod openai_compat;
pub mod registry;
pub mod sakana;
pub mod types;
pub mod use_ai;

pub use registry::{
    complete_completion, pool_stats, proxy_url_for_model, requires_use_ai_account,
    stream_completion,
};
pub use types::{CompletionProvider, CompletionRequest, ProviderPoolStats};
