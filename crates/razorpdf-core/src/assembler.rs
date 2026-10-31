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

    // 1. Triage every page (fast, synchronous).
    let triaged: Vec<PageTriage> = pages
        .iter()
        .map(|p| triage::score_page(p, config))
        .collect();
    let total_pages = pages.len();
    let fast_path_pages = triaged
        .iter()
        .filter(|t| t.fallback_regions.is_empty())
        .count();
    let vlm_fallback_regions: usize = triaged.iter().map(|t| t.fallback_regions.len()).sum();

    // 2. Render crops for regions (synchronous raster work).
    let jobs = render_jobs(&pages, &triaged, config)?;
    tracing::debug!(
        pages = total_pages,
        vlm_jobs = jobs.len(),
        "assembler workload"
    );

    // 3. Bounded-concurrency VLM completion; failures degrade per-region.
    let resolved = complete_jobs(jobs, vlm, config.vlm.max_concurrency.max(1)).await;

    // 4. Blocks per page, Markdown per page, chunks across the document.
    let mut all_blocks: Vec<LayoutBlock> = Vec::new();
    let mut markdown_pages: Vec<String> = Vec::new();
    for (page, tri) in pages.iter().zip(triaged.iter()) {
        let blocks = page_to_blocks(page, tri, &resolved, config);
        markdown_pages.push(emitter::blocks_to_markdown(&blocks));
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
    pages: &[RawPageElements],
    triaged: &[PageTriage],
    config: &ParserConfig,
) -> Result<Vec<RegionJob>> {
    let mut jobs = Vec::new();
    for (page, tri) in pages.iter().zip(triaged.iter()) {
        if tri.fallback_regions.is_empty() {
            continue;
        }
        let canvas = cropper::render_synthetic_canvas(page, config.vlm.render_dpi);
        for (region_index, region) in tri.fallback_regions.iter().enumerate() {
            let uri = cropper::crop_region_to_data_uri(
                &canvas,
                &region.bbox,
                page.width,
                page.height,
                config.vlm.webp_quality,
            )?;
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
    Ok(jobs)
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
    triage: &PageTriage,
    resolved: &BTreeMap<(usize, usize), String>,
    config: &ParserConfig,
) -> Vec<LayoutBlock> {
    let lines = geometry::merge_spans_to_lines(&page.spans);
    let tables = geometry::detect_vector_tables(&page.vectors, &page.spans);
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
    let med_font = geometry::median_font_size(&lines).max(1.0);
    let med_h = geometry::lines::median_line_height(&lines).max(1.0);
    let order = geometry::xy_cut::reading_order_with_medians(&boxes, med_font, med_h, config);
    let median_body = geometry::median_font_size(&lines);

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
                let kind = if geometry::classify_heading(line, median_body) {
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
