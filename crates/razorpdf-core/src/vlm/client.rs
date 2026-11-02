//! VLM provider trait: deterministic Mock, explicit Null, and
//! OpenAI-compatible HTTP (OpenAI, Gemini's OpenAI endpoint, Ollama,
//! OpenRouter, vLLM, ...) for sub-page region completion.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::config::VlmConfig;
use crate::models::{BBox, TriageReason};

/// Strict region-conversion system prompt (spec §4).
pub const SYSTEM_PROMPT: &str = "Convert the provided document region image into clean Markdown. \
    If it is a table, output a valid Markdown table (or HTML <table> \
    if cells use rowspan/colspan). If it is a diagram or chart, output \
    a concise Mermaid.js block followed by a bulleted summary of data \
    points. Output ONLY the raw Markdown without code fences or commentary.";

/// One sub-page completion request.
#[derive(Debug, Clone)]
pub struct VlmRequest {
    /// Cropped region as a `data:image/webp;base64,...` URI.
    pub image_data_uri: String,
    /// Source page index.
    pub page_index: usize,
    /// Tight content bbox (points, top-left origin).
    pub region: BBox,
    /// Why triage routed this region to VLM.
    pub reason: TriageReason,
}

/// Completion result.
#[derive(Debug, Clone)]
pub struct VlmResponse {
    /// Clean Markdown for the region (fences stripped).
    pub markdown: String,
    /// Provider/model label that produced it.
    pub model: String,
}

/// Vision provider for sub-page regions.
#[async_trait]
pub trait VlmProvider: Send + Sync {
    /// Human-readable provider/model label.
    fn name(&self) -> String;

    /// Complete one cropped region into Markdown.
    async fn complete_region(&self, req: &VlmRequest) -> Result<VlmResponse>;
}

/// Deterministic offline provider for tests and demos.
pub struct MockVlmProvider {
    model_label: String,
}

impl MockVlmProvider {
    /// Build a mock provider with the default label.
    #[must_use]
    pub fn new() -> Self {
        Self {
            model_label: "mock-region".to_string(),
        }
    }
}

impl Default for MockVlmProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VlmProvider for MockVlmProvider {
    fn name(&self) -> String {
        format!("mock:{}", self.model_label)
    }

    async fn complete_region(&self, req: &VlmRequest) -> Result<VlmResponse> {
        Ok(VlmResponse {
            markdown: mock_markdown(req),
            model: self.name(),
        })
    }
}

/// Deterministic canned Markdown per trigger reason.
fn mock_markdown(req: &VlmRequest) -> String {
    match req.reason {
        TriageReason::BorderlessIrregularTable | TriageReason::OverlappingSpans => {
            format!(
                "| mock_col_a | mock_col_b |\n| --- | --- |\n| p{}:{:.0},{:.0} | {:?} |\n",
                req.page_index, req.region.x0, req.region.y0, req.reason
            )
        }
        TriageReason::ScannedImageRegion => {
            format!(
                "Mock OCR transcription of page {} region \
                ({:.0},{:.0})-({:.0},{:.0}).\n",
                req.page_index, req.region.x0, req.region.y0, req.region.x1, req.region.y1
            )
        }
        TriageReason::ComplexMathOrDiagram => {
            "```mermaid\ngraph TD\n  A[mock diagram] --> B[mock point]\n```\n\n- mock data point\n"
                .to_string()
        }
        TriageReason::CorruptedCMapOrMojibake => {
            format!(
                "Mock re-transcription of garbled region on page {} \
                ({:.0},{:.0})-({:.0},{:.0}).\n",
                req.page_index, req.region.x0, req.region.y0, req.region.x1, req.region.y1
            )
        }
        TriageReason::MathGlyphCluster => "$$x^2 + y^2 = z^2$$ (mock equation)\n".to_string(),
    }
}

/// Explicit no-VLM provider: every completion fails fast so the assembler
/// leaves regions unresolved (raw-text fallback) instead of calling a model.
pub struct NoVlmProvider;

#[async_trait]
impl VlmProvider for NoVlmProvider {
    fn name(&self) -> String {
        "none".to_string()
    }

    async fn complete_region(&self, _req: &VlmRequest) -> Result<VlmResponse> {
        anyhow::bail!("no VLM configured (use --vlm-url or --mock-vlm)")
    }
}

/// OpenAI-compatible HTTP provider (`/chat/completions` with image_url).
pub struct HttpVlmProvider {
    config: VlmConfig,
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
}

impl HttpVlmProvider {
    /// Build from explicit config (URL falls back to `RAZORPDF_VLM_URL`,
    /// then `OPENAI_BASE_URL` with `/chat/completions` appended; key to
    /// `OPENAI_API_KEY`).
    pub fn new(config: VlmConfig) -> Result<Self> {
        let endpoint = config
            .url
            .clone()
            .or_else(|| std::env::var("RAZORPDF_VLM_URL").ok())
            .or_else(crate::config::openai_base_url_endpoint)
            .context("no VLM endpoint: set RAZORPDF_VLM_URL or pass --vlm-url")?;
        let api_key = config
            .api_key
            .clone()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());
        Ok(Self {
            config,
            http: reqwest::Client::new(),
            endpoint,
            api_key,
        })
    }

    /// OpenAI chat-completions endpoint.
    #[must_use]
    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::from_parts(
            "https://api.openai.com/v1/chat/completions",
            Some(api_key.into()),
            model.into(),
        )
    }

    /// Gemini via its OpenAI-compatibility endpoint.
    #[must_use]
    pub fn gemini(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::from_parts(
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            Some(api_key.into()),
            model.into(),
        )
    }

    /// Local Ollama (`http://localhost:11434/v1/chat/completions`).
    #[must_use]
    pub fn ollama(model: impl Into<String>) -> Self {
        Self::from_parts(
            "http://localhost:11434/v1/chat/completions",
            None,
            model.into(),
        )
    }

    fn from_parts(endpoint: &str, api_key: Option<String>, model: String) -> Self {
        let config = VlmConfig {
            model,
            ..VlmConfig::default()
        };
        Self {
            config,
            http: reqwest::Client::new(),
            endpoint: endpoint.to_string(),
            api_key,
        }
    }

    /// Effective endpoint URL.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[async_trait]
impl VlmProvider for HttpVlmProvider {
    fn name(&self) -> String {
        format!("http:{}", self.config.model)
    }

    async fn complete_region(&self, req: &VlmRequest) -> Result<VlmResponse> {
        let body = serde_json::json!({
            "model": self.config.model,
            "temperature": 0.0,
            "max_tokens": self.config.max_tokens,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": [
                    {"type": "text", "text": format!("Convert this document region to Markdown. Triage reason: {:?}.", req.reason)},
                    {"type": "image_url", "image_url": {"url": req.image_data_uri}},
                ]},
            ],
        });
        let mut call = self
            .http
            .post(&self.endpoint)
            .timeout(Duration::from_secs(self.config.timeout_secs.max(1)))
            .json(&body);
        if let Some(key) = &self.api_key {
            call = call.bearer_auth(key);
        }
        let resp = call.send().await.context("VLM request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let clipped: String = text.chars().take(300).collect();
            anyhow::bail!("VLM HTTP {status}: {clipped}");
        }
        let value: serde_json::Value = resp.json().await.context("VLM JSON decode")?;
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .context("unexpected VLM response shape")?;
        Ok(VlmResponse {
            markdown: strip_code_fences(content),
            model: self.config.model.clone(),
        })
    }
}

/// Strip one wrapping Markdown code fence (with optional language tag).
#[must_use]
pub fn strip_code_fences(s: &str) -> String {
    let trimmed = s.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let mut lines = trimmed.lines();
    lines.next(); // opening fence (+ optional language)
    let mut rest: Vec<&str> = lines.collect();
    while rest.last().is_some_and(|l| l.trim().is_empty()) {
        rest.pop();
    }
    if rest
        .last()
        .is_some_and(|l| l.trim_start().starts_with("```"))
    {
        rest.pop();
    }
    rest.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(reason: TriageReason) -> VlmRequest {
        VlmRequest {
            image_data_uri: "data:image/webp;base64,AAA".to_string(),
            page_index: 2,
            region: BBox::new(56.0, 190.0, 472.0, 265.0),
            reason,
        }
    }

    #[tokio::test]
    async fn mock_table_is_deterministic() {
        let mock = MockVlmProvider::new();
        let out = mock
            .complete_region(&req(TriageReason::BorderlessIrregularTable))
            .await
            .expect("mock");
        assert_eq!(
            out.markdown,
            "| mock_col_a | mock_col_b |\n| --- | --- |\n| p2:56,190 | BorderlessIrregularTable |\n"
        );
        assert!(out.model.starts_with("mock:"));
    }

    #[tokio::test]
    async fn mock_diagram_is_mermaid() {
        let mock = MockVlmProvider::default();
        let out = mock
            .complete_region(&req(TriageReason::ComplexMathOrDiagram))
            .await
            .expect("mock");
        assert!(out.markdown.contains("```mermaid"));
    }

    #[tokio::test]
    async fn null_provider_fails_fast() {
        let err = NoVlmProvider
            .complete_region(&req(TriageReason::OverlappingSpans))
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("--vlm-url"));
    }

    #[test]
    fn fence_stripping() {
        assert_eq!(strip_code_fences("# Plain"), "# Plain");
        assert_eq!(strip_code_fences("```markdown\n# Hi\n```"), "# Hi");
        assert_eq!(strip_code_fences("```\n| a |\n```\n"), "| a |");
        assert_eq!(strip_code_fences("```\nunclosed"), "unclosed");
    }

    #[test]
    fn provider_constructors_preset_endpoints() {
        assert_eq!(
            HttpVlmProvider::ollama("qwen2-vl").endpoint(),
            "http://localhost:11434/v1/chat/completions"
        );
        assert!(HttpVlmProvider::gemini("k", "gemini-2.0-flash")
            .endpoint()
            .contains("generativelanguage"));
        assert!(HttpVlmProvider::openai("k", "gpt-4o-mini")
            .endpoint()
            .contains("api.openai.com"));
    }

    /// Proves the OpenAI wire format against a fake HTTP endpoint (offline).
    #[tokio::test]
    async fn http_provider_completes_against_mock_server() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 65_536];
            let n = sock.read(&mut buf).await.expect("read");
            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
            assert!(text.contains("\"model\""), "missing model field");
            assert!(
                text.contains("data:image/webp;base64,"),
                "missing image data URI"
            );
            let body = r#"{"choices":[{"message":{"content":"```markdown\n# Hi\n```"}}]}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            sock.write_all(head.as_bytes()).await.expect("head");
            sock.write_all(body.as_bytes()).await.expect("body");
        });

        let config = VlmConfig {
            url: Some(format!("http://{addr}/chat/completions")),
            model: "test-model".to_string(),
            ..VlmConfig::default()
        };
        let provider = HttpVlmProvider::new(config).expect("provider");
        let out = provider
            .complete_region(&req(TriageReason::ScannedImageRegion))
            .await
            .expect("complete");
        assert_eq!(out.markdown, "# Hi");
        assert_eq!(out.model, "test-model");
        server.await.expect("server");
    }
}
