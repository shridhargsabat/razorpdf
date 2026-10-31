//! Optional `pdfium-render` backend behind the `pdfium` feature flag.
//!
//! Phase 1: delegates to the pure-Rust fallback so call sites stay stable.
//! Direct pdfium text/vector extraction hooks in here later.

use anyhow::Result;

use crate::models::RawPageElements;

#[cfg(feature = "pdfium")]
pub fn extract_document(pdf: &[u8], source_path: &str) -> Result<Vec<RawPageElements>> {
    tracing::debug!("pdfium backend requested; delegating to lopdf fallback (Phase 1)");
    crate::extractor::lopdf_fallback::extract_document(pdf, source_path)
}

#[cfg(not(feature = "pdfium"))]
pub fn extract_document(_pdf: &[u8], _source_path: &str) -> Result<Vec<RawPageElements>> {
    anyhow::bail!("pdfium backend not enabled; rebuild with --features pdfium")
}
