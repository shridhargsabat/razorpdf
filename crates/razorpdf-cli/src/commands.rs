//! CLI command implementations (hybrid assembler live; MCP lands in Phase 5).

use std::sync::Arc;

use anyhow::{Context, Result};
use razorpdf_core::{
    assembler, extractor, geometry, test_fixtures, triage,
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
    canvas_source: Option<&str>,
) -> Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("read {input}"))?;
    let mut config = ParserConfig::default();
    if let Some(url) = vlm_url {
        config.vlm.url = Some(url.to_string());
    }
    if canvas_source == Some("auto") {
        config.vlm.canvas_source = razorpdf_core::vlm::cropper::CanvasSource::Auto;
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
        let med = geometry::median_font_size(&lines);
        let tables = geometry::detect_all_tables(
            &page.vectors,
            &page.spans,
            &lines,
            geometry::body_font_size(&lines),
            &page.filled_rects,
        );
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

/// Benchmark fast-path throughput on a synthetic N-page document.
///
/// One untimed warmup run, then a timed full assembly (extract → triage →
/// blocks → chunks) with no VLM provider, so the number is pure geometric
/// throughput.
pub async fn bench(pages: usize, as_json: bool) -> Result<()> {
    if pages == 0 {
        anyhow::bail!("--pages must be at least 1");
    }
    let config = ParserConfig::default();
    let build_started = std::time::Instant::now();
    let pdf = test_fixtures::mixed_paper_pdf(pages);
    let build_ms = build_started.elapsed().as_secs_f64() * 1000.0;

    let vlm = Arc::new(NoVlmProvider);
    // Warmup (rayon pool, allocator) — untimed.
    assembler::assemble_document(&pdf, "bench.pdf", &config, Arc::clone(&vlm)).await?;

    let extract_started = std::time::Instant::now();
    let extracted = extractor::extract_document(&pdf, "bench.pdf")?;
    let extract_ms = extract_started.elapsed().as_secs_f64() * 1000.0;
    let spans: usize = extracted.iter().map(|p| p.spans.len()).sum();

    let started = std::time::Instant::now();
    let doc = assembler::assemble_document(&pdf, "bench.pdf", &config, vlm).await?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let pages_per_sec = pages as f64 / elapsed_ms * 1000.0;
    let ms_per_page = elapsed_ms / pages as f64;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "pages": pages,
                "pdf_bytes": pdf.len(),
                "build_ms": build_ms,
                "extract_ms": extract_ms,
                "elapsed_ms": elapsed_ms,
                "pages_per_sec": pages_per_sec,
                "ms_per_page": ms_per_page,
                "spans": spans,
                "chunks": doc.chunks.len(),
                "fast_path_pages": doc.fast_path_pages,
                "vlm_fallback_regions": doc.vlm_fallback_regions,
            }))?
        );
    } else {
        println!("razorpdf bench: {pages}-page mixed paper (fast path, no VLM)");
        println!("  pdf bytes      : {}", pdf.len());
        println!("  fixture build  : {build_ms:.1} ms");
        println!("  extraction     : {extract_ms:.1} ms");
        println!("  full assembly  : {elapsed_ms:.1} ms");
        println!("  throughput     : {pages_per_sec:.1} pages/sec ({ms_per_page:.2} ms/page)");
        println!("  spans / chunks : {spans} / {}", doc.chunks.len());
        println!(
            "  fast pages     : {}  (vlm regions: {})",
            doc.fast_path_pages, doc.vlm_fallback_regions
        );
    }
    Ok(())
}
