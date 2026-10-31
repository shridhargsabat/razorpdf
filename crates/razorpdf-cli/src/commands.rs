//! CLI command implementations (fast geometric path live; VLM + MCP in later phases).

use std::time::Instant;

use anyhow::{Context, Result};
use razorpdf_core::{
    emitter, extractor, geometry, triage, BlockKind, GroundedChunk, LayoutBlock, ParsedDocument,
    ParserConfig,
};

/// Fast-path parse: extract → lines + tables → XY-cut order → emit.
pub fn parse(
    input: &str,
    output: Option<&str>,
    json: Option<&str>,
    _vlm_url: Option<&str>,
) -> Result<()> {
    let started = Instant::now();
    let bytes = std::fs::read(input).with_context(|| format!("read {input}"))?;
    let config = ParserConfig::default();
    let pages = extractor::extract_document(&bytes, input)?;

    let mut markdown_pages: Vec<String> = Vec::new();
    let mut chunks: Vec<GroundedChunk> = Vec::new();
    let mut fast_path_pages = 0usize;

    for page in &pages {
        let triage_score = triage::score_page(page);
        if triage_score.score < config.triage_threshold {
            fast_path_pages += 1;
        }
        let blocks = page_to_blocks(page, &config);
        markdown_pages.push(emitter::blocks_to_markdown(&blocks));
        for b in &blocks {
            chunks.push(GroundedChunk {
                chunk_id: format!("p{}-{}", b.page_index, b.reading_order),
                page_index: b.page_index,
                bbox: b.bbox,
                breadcrumb: Vec::new(),
                markdown: emitter::blocks_to_markdown(std::slice::from_ref(b)),
                token_estimate: emitter::estimate_tokens(&b.raw_text),
                parsed_by_vlm: false,
            });
        }
    }

    let markdown = markdown_pages.join("\n");
    if let Some(path) = output {
        std::fs::write(path, &markdown).with_context(|| format!("write {path}"))?;
    } else {
        print!("{markdown}");
    }

    if let Some(path) = json {
        let doc = ParsedDocument {
            source_path: input.to_string(),
            total_pages: pages.len(),
            fast_path_pages,
            vlm_fallback_regions: 0,
            elapsed_ms: started.elapsed().as_millis(),
            markdown,
            chunks,
        };
        std::fs::write(path, serde_json::to_string_pretty(&doc)?)
            .with_context(|| format!("write {path}"))?;
    }
    Ok(())
}

/// Build ordered layout blocks for one page.
///
/// Table bboxes join text lines as first-class XY-cut items so tables keep
/// their natural position (e.g. full-width tables between paragraphs), and
/// lines whose centroid falls inside a table are dropped (cell text lives
/// in the table block, not as stray paragraphs).
fn page_to_blocks(
    page: &razorpdf_core::RawPageElements,
    config: &ParserConfig,
) -> Vec<LayoutBlock> {
    let lines = geometry::merge_spans_to_lines(&page.spans);
    let tables = geometry::detect_vector_tables(&page.vectors, &page.spans);
    if lines.is_empty() && tables.is_empty() {
        return Vec::new();
    }

    #[derive(Clone, Copy)]
    enum Item {
        Line(usize),
        Table(usize),
    }
    let mut boxes: Vec<razorpdf_core::BBox> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        boxes.push(line.bbox);
        items.push(Item::Line(i));
    }
    for (t, table) in tables.iter().enumerate() {
        boxes.push(table.bbox);
        items.push(Item::Table(t));
    }
    let med_font = geometry::median_font_size(&lines).max(1.0);
    let med_h = lines
        .iter()
        .map(|l| l.bbox.height())
        .fold(0.0f32, f32::max)
        .max(1.0);
    let order = geometry::xy_cut::reading_order_with_medians(&boxes, med_font, med_h, config);

    let median_body = geometry::median_font_size(&lines);
    let mut blocks = Vec::new();
    for (rank, &pos) in order.iter().enumerate() {
        match items[pos] {
            Item::Table(t) => {
                let table = &tables[t];
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
                    reading_order: rank,
                });
            }
            Item::Line(i) => {
                let line = &lines[i];
                let (cx, cy) = line.bbox.centroid();
                if tables.iter().any(|t| t.bbox.contains_point(cx, cy)) {
                    continue; // cell text already captured by the table block
                }
                let is_heading = geometry::classify_heading(line, median_body);
                let kind = if is_heading {
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
                    reading_order: rank,
                });
            }
        }
    }
    blocks
}

/// Print per-page layout stats in a terminal table.
pub fn inspect(input: &str) -> Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("read {input}"))?;
    let config = ParserConfig::default();
    let pages = extractor::extract_document(&bytes, input)?;
    println!(
        "{:>4} {:>6} {:>8} {:>6} {:>6} {:>8} {:>6}",
        "page", "spans", "vectors", "lines", "tables", "med_font", "S(R)"
    );
    for page in &pages {
        let lines = geometry::merge_spans_to_lines(&page.spans);
        let tables = geometry::detect_vector_tables(&page.vectors, &page.spans);
        let med = geometry::median_font_size(&lines);
        let score = triage::score_page(page);
        let verdict = if score.score < config.triage_threshold {
            "fast"
        } else {
            "vlm"
        };
        println!(
            "{:>4} {:>6} {:>8} {:>6} {:>6} {:>8.1} {:>5.2} {verdict}",
            page.page_index,
            page.spans.len(),
            page.vectors.len(),
            lines.len(),
            tables.len(),
            med,
            score.score,
        );
    }
    Ok(())
}
