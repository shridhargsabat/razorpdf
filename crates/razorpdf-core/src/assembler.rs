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
use crate::models::PdfLinkAnnotation;
use crate::models::{
    BlockKind, FootnoteItem, FormFieldEntry, GroundedChunk, LayoutBlock, LineCandidate, PageBBox,
    ParsedDocument, RawPageElements, TriageReason,
};
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
            let lines =
                geometry::merge_spans_to_lines_with_threshold(&p.spans, config.line_merge_overlap)?;
            // Deterministic math first: rewritten lines carry `$...$` text
            // into tables, blocks, and triage-region checks below.
            let (lines, math_boxes) = geometry::math_ast::rewrite_math_lines(lines, &p.vectors);
            let mut verdict = triage::score_page_with_lines(p, &lines, config);
            // Math the reconstructor solved needs no VLM crop: drop
            // covered MathGlyphCluster regions (mirrors table precedence).
            if !verdict.fallback_regions.is_empty() && !math_boxes.is_empty() {
                verdict.fallback_regions.retain(|region| {
                    if !matches!(region.reason, TriageReason::MathGlyphCluster) {
                        return true;
                    }
                    let area = region.content_bbox.area().max(1.0);
                    !math_boxes.iter().any(|b| {
                        b.intersection_area(&region.content_bbox) / area >= TABLE_REGION_PRECEDENCE
                    })
                });
            }
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

    // 4. Blocks per page (parallel), cross-page stitching (sequential:
    //    boundary-local), then Markdown per page + chunks across doc.
    let mut page_blocks: Vec<Vec<LayoutBlock>> = pages
        .par_iter()
        .zip(triaged.par_iter())
        .zip(all_lines.par_iter())
        .map(|((page, tri), lines)| page_to_blocks(page, lines, tri, &resolved, config))
        .collect();
    // 4b. Running header/footer strip + footnote extraction (sequential:
    // cross-page repeats), before stitching so edge blocks never block
    // merges, and before Markdown/chunk emission.
    let page_sizes: Vec<(f32, f32)> = pages.iter().map(|p| (p.width, p.height)).collect();
    let (footnotes, footnote_stats) = geometry::process_footnotes(&mut page_blocks, &page_sizes);
    tracing::debug!(
        stripped = footnote_stats.stripped_running_blocks,
        footnotes = footnote_stats.footnotes_found,
        linked = footnote_stats.markers_linked,
        "footnote processing"
    );
    let stitch_stats = geometry::stitch_across_pages(&mut page_blocks, config);
    tracing::debug!(
        tables_stitched = stitch_stats.tables_stitched,
        paragraphs_merged = stitch_stats.paragraphs_merged,
        "cross-page stitching"
    );
    let mut all_blocks: Vec<LayoutBlock> = Vec::new();
    let mut markdown_pages: Vec<String> = Vec::new();
    for blocks in &page_blocks {
        markdown_pages.push(emitter::blocks_to_markdown(blocks));
        all_blocks.extend(blocks.iter().cloned());
    }
    let mut markdown = markdown_pages.join("\n");
    if !footnotes.is_empty() {
        markdown.push_str("\n\n");
        for note in &footnotes {
            markdown.push_str(&format!("[^{}]: {}\n", note.marker, note.text));
        }
    }
    let mut chunks = emitter::synthesize_chunks(&all_blocks, config.max_chunk_tokens);
    attach_chunk_footnotes(&mut chunks, &footnotes);
    apply_page_triage_scores(&mut chunks, &triaged);
    let mut form_fields: Vec<FormFieldEntry> = Vec::new();
    for page in &pages {
        form_fields.extend(page.form_fields.iter().cloned());
    }
    let mut text_lines: Vec<LineCandidate> = Vec::new();
    for (page, lines) in pages.iter().zip(all_lines.iter()) {
        text_lines.extend(lines.iter().map(|l| LineCandidate {
            text: l.text.clone(),
            page: page.page_index as u32,
            bbox: l.bbox,
        }));
    }

    Ok(ParsedDocument {
        source_path: source_path.to_string(),
        total_pages,
        fast_path_pages,
        vlm_fallback_regions,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        markdown,
        chunks,
        form_fields,
        footnotes,
        text_lines,
    })
}

/// Attach footnote definitions cited by each chunk's `[^marker]` refs.
/// Same-page definitions win; otherwise the first document-wide match.
fn attach_chunk_footnotes(chunks: &mut [GroundedChunk], footnotes: &[FootnoteItem]) {
    if footnotes.is_empty() {
        return;
    }
    for chunk in chunks.iter_mut() {
        for note in footnotes {
            let reference = format!("[^{}]", note.marker);
            if chunk.markdown.contains(&reference)
                && !chunk.footnotes.iter().any(|f| f.marker == note.marker)
                && (note.page == chunk.page_index as u32
                    || !footnotes
                        .iter()
                        .any(|f| f.marker == note.marker && f.page == chunk.page_index as u32))
            {
                chunk.footnotes.push(note.clone());
            }
        }
    }
}

/// Fill non-VLM chunks with their owning page's triage score (`vlm_region`
/// chunks keep the region complexity set at synthesis).
fn apply_page_triage_scores(chunks: &mut [GroundedChunk], triaged: &[PageTriage]) {
    for chunk in chunks.iter_mut() {
        if chunk.block_kind == "vlm_region" {
            continue;
        }
        if let Some(verdict) = triaged.get(chunk.page_index) {
            chunk.triage_score = verdict.score;
        }
    }
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
                    reason: region.reason,
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
                    page_spans: vec![PageBBox {
                        page: page.page_index as u32,
                        bbox: region.content_bbox,
                    }],
                    kind: BlockKind::VlmFallbackRegion {
                        reason: region.reason,
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
                    page_spans: vec![PageBBox {
                        page: page.page_index as u32,
                        bbox: table.bbox,
                    }],
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
                    page_spans: vec![PageBBox {
                        page: page.page_index as u32,
                        bbox: line.bbox,
                    }],
                    kind,
                    raw_text: linkify_line_text(line, &page.link_annotations),
                    reading_order: blocks.len(),
                });
            }
        }
    }
    // Form fields trail page content as checklist/bullet blocks (v1: no
    // layout integration; fields keep document order, not reading order).
    for (k, field) in page.form_fields.iter().enumerate() {
        blocks.push(LayoutBlock {
            id: format!("p{}-form{k}", page.page_index),
            page_index: page.page_index,
            bbox: field.bbox,
            page_spans: vec![PageBBox {
                page: page.page_index as u32,
                bbox: field.bbox,
            }],
            kind: BlockKind::Paragraph,
            raw_text: render_form_field(field),
            reading_order: blocks.len(),
        });
    }
    blocks
}

/// Escape link display text for Markdown anchors.
fn escape_link_text(text: &str) -> String {
    text.replace('[', "\\[").replace(']', "\\]")
}

/// Escape a URI for a Markdown anchor target.
fn escape_uri(uri: &str) -> String {
    uri.replace(' ', "%20").replace(')', "%29")
}

/// Wrap span texts covered by link annotations in `[text](uri)`.
///
/// Spans whose centroid falls inside a link bbox become anchors; other
/// spans pass through. Lines without any hit keep the merged line text
/// (gap spacing preserved); linkified lines join spans with spaces.
fn linkify_line_text(
    line: &crate::geometry::lines::TextLine,
    links: &[PdfLinkAnnotation],
) -> String {
    if links.is_empty() {
        return line.text.clone();
    }
    let mut parts = Vec::with_capacity(line.spans.len());
    let mut touched = false;
    for span in &line.spans {
        let (cx, cy) = span.bbox.centroid();
        let hit = links.iter().find(|l| l.bbox.contains_point(cx, cy));
        match hit {
            Some(link) => {
                touched = true;
                parts.push(format!(
                    "[{}]({})",
                    escape_link_text(span.text.trim()),
                    escape_uri(&link.uri)
                ));
            }
            None => parts.push(span.text.clone()),
        }
    }
    if touched {
        parts.join(" ")
    } else {
        line.text.clone()
    }
}

/// Render a form field as a Markdown checklist/bullet line:
/// `- [x] Name` / `- [ ] Name` for buttons, `- Name: Value` otherwise.
fn render_form_field(field: &FormFieldEntry) -> String {
    if field.field_type == "Btn" {
        let checked = !field.value.is_empty() && field.value != "Off";
        let mark = if checked { "x" } else { " " };
        if field.value.is_empty()
            || field.value == "Yes"
            || field.value == "Off"
            || field.value == "On"
        {
            format!("- [{mark}] {}", field.name)
        } else {
            format!("- [{mark}] {}: {}", field.name, field.value)
        }
    } else if field.value.is_empty() {
        format!("- {}", field.name)
    } else {
        format!("- {}: {}", field.name, field.value)
    }
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

    #[tokio::test]
    async fn links_emit_as_markdown_anchors() {
        let pdf = test_fixtures::annotated_form_pdf();
        let doc = assemble_document(
            &pdf,
            "annotated.pdf",
            &ParserConfig::default(),
            Arc::new(NoVlmProvider),
        )
        .await
        .expect("assemble");
        assert!(
            doc.markdown
                .contains("[Visit our site](https://example.com/docs)"),
            "markdown:\n{}",
            doc.markdown
        );
    }

    #[tokio::test]
    async fn form_fields_emit_as_checklist() {
        let pdf = test_fixtures::annotated_form_pdf();
        let doc = assemble_document(
            &pdf,
            "annotated.pdf",
            &ParserConfig::default(),
            Arc::new(NoVlmProvider),
        )
        .await
        .expect("assemble");
        assert!(
            doc.markdown.contains("- FullName: Ada Lovelace"),
            "markdown:\n{}",
            doc.markdown
        );
        assert!(
            doc.markdown.contains("- [x] Subscribe"),
            "markdown:\n{}",
            doc.markdown
        );
        assert!(
            doc.markdown.contains("- HiddenNote: secret"),
            "markdown:\n{}",
            doc.markdown
        );
    }

    #[test]
    fn unchecked_box_renders_empty() {
        let field = crate::models::FormFieldEntry {
            name: "Agree".to_string(),
            value: "Off".to_string(),
            field_type: "Btn".to_string(),
            bbox: crate::models::BBox::new(0.0, 0.0, 10.0, 10.0),
            page: 0,
        };
        assert_eq!(render_form_field(&field), "- [ ] Agree");
    }

    #[test]
    fn page_scores_fill_plain_chunks_not_vlm_ones() {
        let bbox = crate::models::BBox::new(0.0, 0.0, 10.0, 10.0);
        let mut chunks = vec![
            GroundedChunk {
                chunk_id: "p0-000".to_string(),
                page_index: 0,
                bbox,
                breadcrumb: Vec::new(),
                markdown: "Body.".to_string(),
                token_estimate: 1,
                parsed_by_vlm: false,
                page_spans: Vec::new(),
                footnotes: Vec::new(),
                block_kind: "paragraph".to_string(),
                triage_score: 0.0,
            },
            GroundedChunk {
                chunk_id: "p1-000".to_string(),
                page_index: 1,
                bbox,
                breadcrumb: Vec::new(),
                markdown: "| a |".to_string(),
                token_estimate: 1,
                parsed_by_vlm: false,
                page_spans: Vec::new(),
                footnotes: Vec::new(),
                block_kind: "vlm_region".to_string(),
                triage_score: 0.88,
            },
        ];
        let triaged = vec![
            PageTriage {
                score: 0.12,
                is_scanned: false,
                fallback_regions: Vec::new(),
            },
            PageTriage {
                score: 0.91,
                is_scanned: false,
                fallback_regions: Vec::new(),
            },
        ];
        apply_page_triage_scores(&mut chunks, &triaged);
        assert!((chunks[0].triage_score - 0.12).abs() < 1e-6);
        assert!((chunks[1].triage_score - 0.88).abs() < 1e-6);
    }

    #[tokio::test]
    async fn split_paragraph_stitches_across_pages() {
        let pdf = test_fixtures::split_paragraph_pdf();
        let doc = assemble_document(
            &pdf,
            "split.pdf",
            &ParserConfig::default(),
            Arc::new(NoVlmProvider),
        )
        .await
        .expect("assemble");
        assert_eq!(doc.total_pages, 2);
        assert!(
            doc.markdown.contains("The example was hard."),
            "markdown:\n{}",
            doc.markdown
        );
        assert!(
            !doc.markdown.contains("exam-"),
            "hyphen stump leaked:\n{}",
            doc.markdown
        );
    }
}
