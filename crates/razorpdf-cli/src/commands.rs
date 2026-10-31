//! CLI command implementations (hybrid assembler live; MCP lands in Phase 5).

use std::sync::Arc;

use anyhow::{Context, Result};
use razorpdf_core::{
    assembler, extractor, geometry, triage,
    vlm::{HttpVlmProvider, MockVlmProvider, NoVlmProvider},
    ParserConfig,
};

/// Hybrid parse: assemble via fast path + VLM fallback, then emit.
pub async fn parse(
    input: &str,
    output: Option<&str>,
    json: Option<&str>,
    vlm_url: Option<&str>,
    mock_vlm: bool,
) -> Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("read {input}"))?;
    let mut config = ParserConfig::default();
    if let Some(url) = vlm_url {
        config.vlm.url = Some(url.to_string());
    }

    if mock_vlm {
        let doc =
            assembler::assemble_document(&bytes, input, &config, Arc::new(MockVlmProvider::new()))
                .await?;
        return emit(output, json, &doc);
    }

    if config.vlm.url.is_some() || std::env::var("RAZORPDF_VLM_URL").is_ok() {
        let http = HttpVlmProvider::new(config.vlm.clone())?;
        tracing::info!(endpoint = http.endpoint(), "VLM fallback enabled");
        let doc = assembler::assemble_document(&bytes, input, &config, Arc::new(http)).await?;
        return emit(output, json, &doc);
    }

    let doc = assembler::assemble_document(&bytes, input, &config, Arc::new(NoVlmProvider)).await?;
    if doc.vlm_fallback_regions > 0 {
        eprintln!(
            "warning: {} region(s) need VLM but none is configured; raw-text fallback used \
             (pass --vlm-url or --mock-vlm)",
            doc.vlm_fallback_regions
        );
    }
    emit(output, json, &doc)
}

fn emit(
    output: Option<&str>,
    json: Option<&str>,
    doc: &razorpdf_core::ParsedDocument,
) -> Result<()> {
    if let Some(path) = output {
        std::fs::write(path, &doc.markdown).with_context(|| format!("write {path}"))?;
    } else {
        print!("{}", doc.markdown);
    }
    if let Some(path) = json {
        std::fs::write(path, serde_json::to_string_pretty(&doc)?)
            .with_context(|| format!("write {path}"))?;
    }
    Ok(())
}

/// Print per-page layout stats and triage decisions in a terminal table.
pub fn inspect(input: &str) -> Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("read {input}"))?;
    let config = ParserConfig::default();
    let pages = extractor::extract_document(&bytes, input)?;
    println!(
        "{:>4} {:>6} {:>8} {:>6} {:>6} {:>8} {:>6} {:>7} ver",
        "page", "spans", "vectors", "lines", "tables", "med_font", "S(R)", "regions"
    );
    for page in &pages {
        let lines = geometry::merge_spans_to_lines(&page.spans);
        let tables = geometry::detect_vector_tables(&page.vectors, &page.spans);
        let med = geometry::median_font_size(&lines);
        let triage = triage::score_page(page, &config);
        let verdict = if triage.fallback_regions.is_empty() {
            "fast"
        } else {
            "vlm"
        };
        println!(
            "{:>4} {:>6} {:>8} {:>6} {:>6} {:>8.1} {:>5.2} {:>7} {verdict}",
            page.page_index,
            page.spans.len(),
            page.vectors.len(),
            lines.len(),
            tables.len(),
            med,
            triage.score,
            triage.fallback_regions.len(),
        );
        for (r, region) in triage.fallback_regions.iter().enumerate() {
            println!(
                "  region {r}: {:?} S={:.2} bbox=[{:.0},{:.0},{:.0},{:.0}]",
                region.reason,
                region.score,
                region.content_bbox.x0,
                region.content_bbox.y0,
                region.content_bbox.x1,
                region.content_bbox.y1,
            );
        }
    }
    Ok(())
}
