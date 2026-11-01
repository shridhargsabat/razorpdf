//! Hybrid document assembler: fast geometric path plus concurrent sub-page
//! VLM fallback, stitched into Markdown and grounded RAG chunks.
//!
//! Per-document flow: extract (rayon page-parallel) → triage → render VLM
//! region crops → bounded-concurrency completion → blocks → emit.
//! Individual VLM failures never fail the document: the region keeps its
//! raw-text fallback.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use rayon::prelude::*;
use tokio::sync::Semaphore;

use crate::config::ParserConfig;
use crate::emitter;
use crate::extractor;
use crate::geometry;
use crate::models::{BlockKind, LayoutBlock, ParsedDocument, RawPageElements};
use crate::triage::{self, PageTriage};
use crate::vlm::{cropper, VlmProvider, VlmRequest};

/// Assemble a full document from PDF bytes.
///
/// Region coverage at/above which a deterministic table suppresses a VLM
/// region (the geometry already extracted the content: skip the VLM call).
const TABLE_REGION_PRECEDENCE: f32 = 0.7;

/// `vlm` is shared across all region completions (use [`NoVlmProvider`](crate::vlm::NoVlmProvider)
/// for pure fast-path runs — regions stay unresolved with raw-text fallback).
pub async fn assemble_document<P>(
    pdf: &[u8],
    source_path: &str,
    config: &ParserConfig,
    vlm: Arc<P>,
) -> Result<ParsedDocument>
where
    P: VlmProvider + 'static,
{
    let started = std::time::Instant::now();
    let pages = extractor::extract_document(pdf, source_path)?;

    // 1. Triage every page (parallel over pages; order-preserving).
    // Deterministic tables take precedence over VLM regions they cover:
    // geometry already won, so the region is dropped before any VLM call.
    // Lines merge once per page and are reused by triage, precedence, and
    // block layout below.
    let triaged_lines: Vec<(PageTriage, Vec<crate::geometry::lines::TextLine>)> = pages
        .par_iter()
        .map(|p| -> anyhow::Result<_> {
            let lines = geometry::merge_spans_to_lines_with_threshold(
                &p.spans,
                config.line_merge_overlap,
            )?;
            let mut verdict = triage::score_page_with_lines(p, &lines, config);
            if !verdict.fallback_regions.is_empty() {
                let tables = geometry::detect_all_tables(
                    &p.vectors,
                    &p.spans,
                    &lines,
                    geometry::body_font_size(&lines),
                    &p.filled_rects,
                );
                verdict.fallback_regions.retain(|region| {
                    let area = region.content_bbox.area().max(1.0);
                    !tables.iter().any(|t| {
                        t.bbox.intersection_area(&region.content_bbox) / area
                            >= TABLE_REGION_PRECEDENCE
                    })
                });
            }
            Ok((verdict, lines))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut triaged: Vec<PageTriage> = Vec::with_capacity(pages.len());
    let mut all_lines: Vec<Vec<crate::geometry::lines::TextLine>> = Vec::with_capacity(pages.len());
    for (verdict, lines) in triaged_lines {
        triaged.push(verdict);
        all_lines.push(lines);
    }
    let total_pages = pages.len();
    let fast_path_pages = triaged
        .iter()
        .filter(|t| t.fallback_regions.is_empty())
        .count();
    let vlm_fallback_regions: usize = triaged.iter().map(|t| t.fallback_regions.len()).sum();

    // 2. Render crops for regions (synchronous raster work).
    let jobs = render_jobs(pdf, &pages, &triaged, config);
    tracing::debug!(
        pages = total_pages,
        vlm_jobs = jobs.len(),
        "assembler workload"
    );

    // 3. Bounded-concurrency VLM completion; failures degrade per-region.
    let resolved = complete_jobs(jobs, vlm, config.vlm.max_concurrency.max(1)).await;

    // 4. Blocks per page, Markdown per page (parallel), chunks across doc.
    let mut all_blocks: Vec<LayoutBlock> = Vec::new();
    let mut markdown_pages: Vec<String> = Vec::new();
    let page_outputs: Vec<(Vec<LayoutBlock>, String)> = pages
        .par_iter()
        .zip(triaged.par_iter())
        .zip(all_lines.par_iter())
        .map(|((page, tri), lines)| {
            let blocks = page_to_blocks(page, lines, tri, &resolved, config);
            let markdown = emitter::blocks_to_markdown(&blocks);
            (blocks, markdown)
        })
        .collect();
    for (blocks, markdown) in page_outputs {
        markdown_pages.push(markdown);
        all_blocks.extend(blocks);
    }
    let chunks = emitter::synthesize_chunks(&all_blocks);

    Ok(ParsedDocument {
        source_path: source_path.to_string(),
        total_pages,
        fast_path_pages,
        vlm_fallback_regions,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        markdown: markdown_pages.join("\n"),
        chunks,
    })
}

/// One VLM completion job with fully owned inputs.
struct RegionJob {
    page_index: usize,
    region_index: usize,
    request: VlmRequest,
}

/// Render one crop + data URI per fallback region.
fn render_jobs(
    pdf: &[u8],
    pages: &[RawPageElements],
    triaged: &[PageTriage],
    config: &ParserConfig,
) -> Vec<RegionJob> {
    let mut jobs = Vec::new();
    for (page, tri) in pages.iter().zip(triaged.iter()) {
        if tri.fallback_regions.is_empty() {
            continue;
        }
        let canvas =
            cropper::render_page_canvas(pdf, page, config.vlm.render_dpi, config.vlm.canvas_source);
        for (region_index, region) in tri.fallback_regions.iter().enumerate() {
            let uri = match cropper::crop_region_to_data_uri(
                &canvas,
                &region.bbox,
                page.width,
                page.height,
                config.vlm.webp_quality,
            ) {
                Ok(uri) => uri,
                Err(error) => {
                    // One degenerate region (e.g. fully off-page vectors
                    // cropping 0-wide) must not fail the document: degrade
                    // to raw text, exactly like a VLM completion failure.
                    tracing::warn!(
                        page = page.page_index,
                        region = region_index,
                        error = %error,
                        "region crop failed; keeping raw-text fallback"
                    );
                    continue;
                }
            };
            jobs.push(RegionJob {
                page_index: page.page_index,
                region_index,
                request: VlmRequest {
                    image_data_uri: uri,
                    page_index: page.page_index,
                    region: region.content_bbox,
                    reason: region.reason.clone(),
                },
            });
        }
    }
    jobs
}

/// Complete jobs with at most `max_concurrency` in flight.
/// Returns `(page, region) → resolved Markdown` for successes.
async fn complete_jobs<P>(
    jobs: Vec<RegionJob>,
    vlm: Arc<P>,
    max_concurrency: usize,
) -> BTreeMap<(usize, usize), String>
where
    P: VlmProvider + 'static,
{
    let semaphore = Arc::new(Semaphore::new(max_concurrency));
    let mut set = tokio::task::JoinSet::new();
    for job in jobs {
        let permit_source = Arc::clone(&semaphore);
        let provider = Arc::clone(&vlm);
        set.spawn(async move {
            let _permit = permit_source.acquire_owned().await;
            let outcome = provider.complete_region(&job.request).await;
            (job.page_index, job.region_index, outcome)
        });
    }
    let mut resolved = BTreeMap::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((page, region, Ok(response))) => {
                resolved.insert((page, region), response.markdown);
            }
            Ok((page, region, Err(error))) => {
                tracing::warn!(
                    page,
                    region,
                    error = %error,
                    "VLM region failed; keeping raw-text fallback"
                );
            }
            Err(error) => {
                tracing::warn!(error = %error, "VLM task join failed");
            }
        }
    }
    resolved
}

/// Build ordered layout blocks for one page.
///
/// Lines, vector tables, and VLM regions all enter XY-cut as first-class
/// items. Lines inside tables or VLM regions are dropped (their content
/// lives in the table/region block), as are tables swallowed by a region.
fn page_to_blocks(
    page: &RawPageElements,
    lines: &[crate::geometry::lines::TextLine],
    triage: &PageTriage,
    resolved: &BTreeMap<(usize, usize), String>,
    config: &ParserConfig,
) -> Vec<LayoutBlock> {
    let tables = geometry::detect_all_tables(
        &page.vectors,
        &page.spans,
        lines,
        geometry::body_font_size(lines),
        &page.filled_rects,
    );
    let regions = &triage.fallback_regions;
    if lines.is_empty() && tables.is_empty() && regions.is_empty() {
        return Vec::new();
    }

    #[derive(Clone, Copy)]
    enum Item {
        Line(usize),
        Table(usize),
        Region(usize),
    }
    let mut boxes = Vec::new();
    let mut items = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        boxes.push(line.bbox);
        items.push(Item::Line(i));
    }
    for (t, table) in tables.iter().enumerate() {
        boxes.push(table.bbox);
        items.push(Item::Table(t));
    }
    for (r, region) in regions.iter().enumerate() {
        // Order by content bbox: padding may touch neighbors and hide
        // the valleys the content boxes leave open.
        boxes.push(region.content_bbox);
        items.push(Item::Region(r));
    }
    let med_font = geometry::body_font_size(lines).max(1.0);
    let med_h = geometry::lines::median_line_height(lines).max(1.0);
    let order = geometry::xy_cut::reading_order_with_medians(&boxes, med_font, med_h, config);
    let median_body = geometry::body_font_size(lines);

    let inside_region =
        |x: f32, y: f32| regions.iter().any(|r| r.content_bbox.contains_point(x, y));
    let mut blocks = Vec::new();
    for &pos in &order {
        match items[pos] {
            Item::Region(r) => {
                let region = &regions[r];
                blocks.push(LayoutBlock {
                    id: format!("p{}-vlm{r}", page.page_index),
                    page_index: page.page_index,
                    bbox: region.content_bbox,
                    kind: BlockKind::VlmFallbackRegion {
                        reason: region.reason.clone(),
                        complexity_score: region.score,
                        resolved_markdown: resolved.get(&(page.page_index, r)).cloned(),
                    },
                    raw_text: region_raw_text(page, region.content_bbox),
                    reading_order: blocks.len(),
                });
            }
            Item::Table(t) => {
                let table = &tables[t];
                let (cx, cy) = table.bbox.centroid();
                if inside_region(cx, cy) {
                    continue; // VLM region owns this area
                }
                blocks.push(LayoutBlock {
                    id: format!("p{}-table{t}", page.page_index),
                    page_index: page.page_index,
                    bbox: table.bbox,
                    kind: BlockKind::DeterministicTable {
                        rows: table.rows.clone(),
                        has_header: table.has_header,
                    },
                    raw_text: table
                        .rows
                        .iter()
                        .map(|r| r.join(" | "))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    reading_order: blocks.len(),
                });
            }
            Item::Line(i) => {
                let line = &lines[i];
                let (cx, cy) = line.bbox.centroid();
                if inside_region(cx, cy) || tables.iter().any(|t| t.bbox.contains_point(cx, cy)) {
                    continue;
                }
                let kind = if geometry::classify_heading_with_mult(
                    line,
                    median_body,
                    config.heading_size_mult,
                ) {
                    BlockKind::Heading {
                        level: geometry::lines::heading_level(line, median_body),
                    }
                } else if line.is_monospace && line.text.len() > 40 {
                    BlockKind::CodeBlock
                } else {
                    BlockKind::Paragraph
                };
                blocks.push(LayoutBlock {
                    id: format!("p{}-line{i}", page.page_index),
                    page_index: page.page_index,
                    bbox: line.bbox,
                    kind,
                    raw_text: line.text.clone(),
                    reading_order: blocks.len(),
                });
            }
        }
    }
    blocks
}

/// Raw-text fallback for a region: span texts in reading-ish order.
fn region_raw_text(page: &RawPageElements, content: crate::models::BBox) -> String {
    let mut hits: Vec<&crate::models::TextSpan> = page
        .spans
        .iter()
        .filter(|s| {
            let (cx, cy) = s.bbox.centroid();
            content.contains_point(cx, cy)
        })
        .collect();
    hits.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bbox
                    .x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    hits.iter()
        .map(|s| s.text.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures;
    use crate::vlm::{MockVlmProvider, NoVlmProvider};

    #[tokio::test]
    async fn clean_page_assembles_without_vlm_calls() {
        let pdf = test_fixtures::single_col_clean_pdf();
        let doc = assemble_document(
            &pdf,
            "single.pdf",
            &ParserConfig::default(),
            Arc::new(NoVlmProvider),
        )
        .await
        .expect("assemble");
        assert_eq!(doc.total_pages, 1);
        assert_eq!(doc.fast_path_pages, 1);
        assert_eq!(doc.vlm_fallback_regions, 0);
        assert!(doc.markdown.contains("# RazorPDF Benchmark Document"));
        assert_eq!(doc.chunks.len(), 17);
        assert!(doc.chunks.iter().all(|c| !c.parsed_by_vlm));
    }

    #[tokio::test]
    async fn messy_page_resolves_via_mock() {
        let pdf = test_fixtures::messy_overlap_table_pdf();
        let doc = assemble_document(
            &pdf,
            "messy.pdf",
            &ParserConfig::default(),
            Arc::new(MockVlmProvider::new()),
        )
        .await
        .expect("assemble");
        assert_eq!(doc.vlm_fallback_regions, 1);
        assert_eq!(doc.fast_path_pages, 0);
        assert!(doc.markdown.contains("mock_col_a"));
        assert!(doc.markdown.contains("Messy Invoice Scan"));
        let vlm_chunks: Vec<_> = doc.chunks.iter().filter(|c| c.parsed_by_vlm).collect();
        assert_eq!(vlm_chunks.len(), 1);
    }

    #[tokio::test]
    async fn null_vlm_degrades_to_raw_text() {
        let pdf = test_fixtures::messy_overlap_table_pdf();
        let doc = assemble_document(
            &pdf,
            "messy.pdf",
            &ParserConfig::default(),
            Arc::new(NoVlmProvider),
        )
        .await
        .expect("assemble");
        assert_eq!(doc.vlm_fallback_regions, 1);
        assert!(!doc.markdown.contains("mock_col_a"));
        assert!(doc.markdown.contains("Item0")); // raw island text preserved
        assert!(doc.chunks.iter().all(|c| !c.parsed_by_vlm));
    }
}
