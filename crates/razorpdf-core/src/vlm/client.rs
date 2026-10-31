//! Async VLM router for OpenAI-compatible Vision endpoints.
//!
//! Concurrent sub-page completion lands in Phase 4; the client shell and
//! strict system prompt are live now.

use crate::config::VlmConfig;

/// Async client for sub-page region completion.
pub struct VlmClient {
    config: VlmConfig,
    http: reqwest::Client,
}

impl VlmClient {
    /// Build a client from explicit configuration.
    #[must_use]
    pub fn new(config: VlmConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Effective endpoint URL (explicit config or env fallback).
    #[must_use]
    pub fn endpoint(&self) -> Option<&str> {
        self.config.url.as_deref()
    }

    /// Strict region-conversion system prompt (spec §4).
    #[must_use]
    pub fn system_prompt() -> &'static str {
        "Convert the provided document region image into clean Markdown. \
         If it is a table, output a valid Markdown table (or HTML <table> \
         if cells use rowspan/colspan). If it is a diagram or chart, output \
         a concise Mermaid.js block followed by a bulleted summary of data \
         points. Output ONLY the raw Markdown without code fences or commentary."
    }

    /// Complete one cropped WebP region into Markdown (Phase 4).
    pub async fn complete_region(&self, _webp: &[u8]) -> anyhow::Result<String> {
        let _ = &self.http;
        anyhow::bail!("VLM completion ships in Phase 4")
    }
}
