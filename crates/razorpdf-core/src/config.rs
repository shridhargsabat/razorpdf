//! Parser & VLM configuration.

use serde::{Deserialize, Serialize};

use crate::vlm::cropper::CanvasSource;

/// Top-level parser configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParserConfig {
    /// XY-cut histogram bin resolution in points.
    pub xy_bin_pt: f32,
    /// Y-cut threshold multiplier over median line height.
    pub xy_tau_y_mult: f32,
    /// X-cut threshold multiplier over median font size.
    pub xy_tau_x_mult: f32,
    /// Vertical overlap ratio required to merge spans into one line.
    pub line_merge_overlap: f32,
    /// Heading threshold multiplier over median body font size.
    pub heading_size_mult: f32,
    /// Triage decision threshold: S(R) >= threshold => VLM fallback.
    pub triage_threshold: f32,
    /// Padding applied to VLM fallback region bboxes (points).
    pub vlm_region_pad_pt: f32,
    /// Stitch tables/paragraphs split across page boundaries.
    pub enable_cross_page_stitching: bool,
    /// Max tokens per RAG chunk; oversized tables split with repeated
    /// headers (counted by `count_tokens`, exact under `accurate-tokens`).
    pub max_chunk_tokens: usize,
    /// VLM client configuration.
    pub vlm: VlmConfig,
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            xy_bin_pt: 2.0,
            xy_tau_y_mult: 1.2,
            xy_tau_x_mult: 2.5,
            line_merge_overlap: 0.55,
            heading_size_mult: 1.15,
            triage_threshold: 0.45,
            vlm_region_pad_pt: 8.0,
            enable_cross_page_stitching: true,
            max_chunk_tokens: 512,
            vlm: VlmConfig::default(),
        }
    }
}

/// VLM endpoint configuration (any OpenAI-compatible Vision endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VlmConfig {
    /// Endpoint URL, e.g. `http://localhost:11434/v1/chat/completions`.
    /// Falls back to `RAZORPDF_VLM_URL` / `OPENAI_BASE_URL` env vars.
    pub url: Option<String>,
    /// API key. Falls back to `OPENAI_API_KEY` env var.
    pub api_key: Option<String>,
    /// Model name for the chat-completions request.
    pub model: String,
    /// Max concurrent sub-page VLM requests.
    pub max_concurrency: usize,
    /// Render DPI for sub-page crops.
    pub render_dpi: u32,
    /// WebP quality for sub-page crops (0-100).
    pub webp_quality: u8,
    /// Per-request HTTP timeout (seconds).
    pub timeout_secs: u64,
    /// Max completion tokens per region request.
    pub max_tokens: u32,
    /// Page-canvas source for VLM crops (synthetic default; `auto` uses
    /// `pdftoppm` when present on PATH).
    pub canvas_source: CanvasSource,
}

/// `OPENAI_BASE_URL` holds a base (…/v1); expand to the chat route.
#[must_use]
pub fn openai_base_url_endpoint() -> Option<String> {
    std::env::var("OPENAI_BASE_URL")
        .ok()
        .map(|b| format!("{}/chat/completions", b.trim_end_matches('/')))
}

impl Default for VlmConfig {
    fn default() -> Self {
        Self {
            url: std::env::var("RAZORPDF_VLM_URL")
                .ok()
                .or_else(openai_base_url_endpoint),
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            model: std::env::var("RAZORPDF_VLM_MODEL")
                .unwrap_or_else(|_| "qwen2-vl-7b-instruct".to_string()),
            max_concurrency: 8,
            render_dpi: 150,
            webp_quality: 80,
            timeout_secs: 180,
            max_tokens: 2000,
            canvas_source: CanvasSource::Synthetic,
        }
    }
}
