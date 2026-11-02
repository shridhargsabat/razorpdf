//! Grounded RAG chunk synthesis: one semantic chunk per layout block with
//! heading breadcrumbs, page bboxes, and token counts.

use crate::emitter::markdown::blocks_to_markdown;
use crate::models::{BlockKind, GroundedChunk, LayoutBlock};

/// Synthesize grounded chunks from layout blocks (any order).
///
/// Tracks the heading hierarchy across pages so each chunk carries its
/// `H1 > H2 > H3` ancestor breadcrumb. Blocks rendering to blank Markdown
/// are skipped, so every chunk has non-empty text. Tables exceeding
/// `max_chunk_tokens` split into header-preserving sub-chunks.
#[must_use]
pub fn synthesize_chunks(blocks: &[LayoutBlock], max_chunk_tokens: usize) -> Vec<GroundedChunk> {
    let mut ordered: Vec<&LayoutBlock> = blocks.iter().collect();
    ordered.sort_by_key(|b| (b.page_index, b.reading_order));

    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut chunks = Vec::new();
    for block in ordered {
        let markdown = blocks_to_markdown(std::slice::from_ref(block));
        if markdown.trim().is_empty() {
            continue;
        }
        let breadcrumb: Vec<String> = stack.iter().map(|(_, t)| t.clone()).collect();
        if let BlockKind::Heading { level } = &block.kind {
            while stack.last().is_some_and(|(l, _)| *l >= *level) {
                stack.pop();
            }
            let ancestors: Vec<String> = stack.iter().map(|(_, t)| t.clone()).collect();
            chunks.push(chunk_for(block, ancestors, markdown));
            stack.push((*level, block.raw_text.trim().to_string()));
        } else if let BlockKind::DeterministicTable { rows, has_header } = &block.kind {
            if count_tokens(&markdown) <= max_chunk_tokens.max(1) {
                chunks.push(chunk_for(block, breadcrumb, markdown));
            } else {
                chunks.extend(split_table_chunks(
                    block,
                    rows,
                    *has_header,
                    &breadcrumb,
                    max_chunk_tokens.max(1),
                ));
            }
        } else {
            chunks.push(chunk_for(block, breadcrumb, markdown));
        }
    }
    chunks
}

fn chunk_for(block: &LayoutBlock, breadcrumb: Vec<String>, markdown: String) -> GroundedChunk {
    GroundedChunk {
        chunk_id: format!("p{}-{:03}", block.page_index, block.reading_order),
        page_index: block.page_index,
        bbox: block.bbox,
        breadcrumb,
        token_estimate: count_tokens(&markdown),
        markdown,
        parsed_by_vlm: matches!(
            &block.kind,
            BlockKind::VlmFallbackRegion {
                resolved_markdown: Some(_),
                ..
            }
        ),
        page_spans: block.page_spans.clone(),
    }
}

/// Render table rows to Markdown exactly like the block path.
fn table_slice_markdown(
    rows: &[Vec<String>],
    has_header: bool,
    bbox: crate::models::BBox,
) -> String {
    let table = crate::geometry::table_grid::DetectedTable {
        bbox,
        rows: rows.to_vec(),
        has_header,
        source: crate::geometry::TableSource::VectorGrid,
    };
    let mut md = crate::geometry::table_grid::detected_table_to_markdown(&table);
    md.push('\n');
    md
}

/// Slice a block bbox vertically by body-row share `[start, end)` of total.
///
/// Blocks carry no per-row geometry, so sub-chunk boxes split the height
/// evenly — an approximation for citations, exact at the boundaries.
fn vertical_slice(
    bbox: crate::models::BBox,
    start: usize,
    end: usize,
    total: usize,
) -> crate::models::BBox {
    let height = bbox.y1 - bbox.y0;
    crate::models::BBox::new(
        bbox.x0,
        bbox.y0 + height * start as f32 / total as f32,
        bbox.x1,
        bbox.y0 + height * end as f32 / total as f32,
    )
}

/// Split an over-budget table into header-preserving sub-chunks.
///
/// Each sub-chunk repeats the header + separator rows and packs body rows
/// greedily within `max_tokens`. Sub-chunk ids gain a `t{n}` suffix.
fn split_table_chunks(
    block: &LayoutBlock,
    rows: &[Vec<String>],
    has_header: bool,
    breadcrumb: &[String],
    max_tokens: usize,
) -> Vec<GroundedChunk> {
    let (header_rows, body_rows): (&[Vec<String>], &[Vec<String>]) =
        if has_header && !rows.is_empty() {
            (&rows[..1], &rows[1..])
        } else {
            (&[], rows)
        };
    if body_rows.is_empty() {
        // Header-only table: unsplittable, emit whole.
        return vec![chunk_for(
            block,
            breadcrumb.to_vec(),
            blocks_to_markdown(std::slice::from_ref(block)),
        )];
    }
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start = 0;
    while start < body_rows.len() {
        let mut end = start;
        while end < body_rows.len() {
            let mut candidate = header_rows.to_vec();
            candidate.extend_from_slice(&body_rows[start..=end]);
            let md = table_slice_markdown(&candidate, has_header, block.bbox);
            if count_tokens(&md) <= max_tokens {
                end += 1;
            } else {
                break;
            }
        }
        if end == start {
            end = start + 1; // single row over budget: emit alone, never drop
        }
        groups.push(start..end);
        start = end;
    }
    let total_body = body_rows.len();
    groups
        .iter()
        .enumerate()
        .map(|(g, range)| {
            let mut candidate = header_rows.to_vec();
            candidate.extend_from_slice(&body_rows[range.clone()]);
            let markdown = table_slice_markdown(&candidate, has_header, block.bbox);
            GroundedChunk {
                chunk_id: format!("p{}-{:03}t{g}", block.page_index, block.reading_order),
                page_index: block.page_index,
                bbox: vertical_slice(block.bbox, range.start, range.end, total_body),
                breadcrumb: breadcrumb.to_vec(),
                token_estimate: count_tokens(&markdown),
                markdown,
                parsed_by_vlm: false,
                page_spans: block.page_spans.clone(),
            }
        })
        .collect()
}

/// Token count: accurate tiktoken when the `accurate-tokens` feature is on
/// (falls back to the heuristic offline), else the heuristic.
#[must_use]
pub fn count_tokens(text: &str) -> usize {
    #[cfg(feature = "accurate-tokens")]
    {
        if let Ok(n) = accurate_count(text) {
            return n;
        }
    }
    estimate_tokens(text)
}

/// Heuristic token estimate (~4 chars per token).
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// cl100k token count; fails offline (BPE ranks download on first use).
/// The tokenizer initializes once per process via `OnceLock` (stable API:
/// a losing construction race retries `get`, never panics).
#[cfg(feature = "accurate-tokens")]
fn accurate_count(text: &str) -> anyhow::Result<usize> {
    use std::sync::OnceLock;
    static BPE: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    if let Some(bpe) = BPE.get() {
        return Ok(bpe.encode_with_special_tokens(text).len());
    }
    let bpe = tiktoken_rs::cl100k_base()?;
    let _ = BPE.set(bpe);
    BPE.get()
        .map(|bpe| bpe.encode_with_special_tokens(text).len())
        .ok_or_else(|| anyhow::anyhow!("BPE cell empty after init"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BBox;

    fn block(order: usize, kind: BlockKind, text: &str) -> LayoutBlock {
        let bbox = BBox::new(0.0, 0.0, 10.0, 10.0);
        LayoutBlock {
            id: format!("b{order}"),
            page_index: 0,
            bbox,
            kind,
            raw_text: text.to_string(),
            reading_order: order,
            page_spans: vec![crate::models::PageBBox { page: 0, bbox }],
        }
    }

    #[test]
    fn heuristic_scales_with_length() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn breadcrumbs_track_hierarchy() {
        let blocks = vec![
            block(0, BlockKind::Heading { level: 1 }, "Title"),
            block(1, BlockKind::Heading { level: 3 }, "Section"),
            block(2, BlockKind::Paragraph, "Body text."),
            block(3, BlockKind::Heading { level: 3 }, "Next"),
            block(4, BlockKind::Paragraph, "More."),
        ];
        let chunks = synthesize_chunks(&blocks, 512);
        assert_eq!(chunks.len(), 5);
        assert!(chunks[0].breadcrumb.is_empty());
        assert_eq!(chunks[1].breadcrumb, vec!["Title"]);
        assert_eq!(chunks[2].breadcrumb, vec!["Title", "Section"]);
        assert_eq!(chunks[3].breadcrumb, vec!["Title"]);
        assert_eq!(chunks[4].breadcrumb, vec!["Title", "Next"]);
        assert_eq!(chunks[0].chunk_id, "p0-000");
        assert_eq!(chunks[2].markdown, "Body text.\n\n");
        assert!(!chunks[2].parsed_by_vlm);
    }

    #[test]
    fn blank_blocks_are_skipped() {
        let blocks = vec![
            block(0, BlockKind::Paragraph, "   "),
            block(1, BlockKind::Paragraph, "Real."),
        ];
        let chunks = synthesize_chunks(&blocks, 512);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].markdown, "Real.\n\n");
    }

    #[test]
    fn resolved_vlm_chunks_are_flagged() {
        let blocks = vec![block(
            0,
            BlockKind::VlmFallbackRegion {
                reason: crate::models::TriageReason::OverlappingSpans,
                complexity_score: 0.75,
                resolved_markdown: Some("| a |\n| --- |\n| b |\n".to_string()),
            },
            "raw",
        )];
        let chunks = synthesize_chunks(&blocks, 512);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].parsed_by_vlm);
        assert!(chunks[0].markdown.contains("| a |"));
        assert!(chunks[0].token_estimate > 0);
    }

    fn table_block(order: usize, rows: Vec<Vec<&str>>, has_header: bool) -> LayoutBlock {
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(str::to_string).collect())
            .collect();
        let bbox = BBox::new(72.0, 100.0, 540.0, 300.0);
        LayoutBlock {
            id: format!("b{order}"),
            page_index: 0,
            bbox,
            kind: BlockKind::DeterministicTable { rows, has_header },
            raw_text: String::new(),
            reading_order: order,
            page_spans: vec![crate::models::PageBBox { page: 0, bbox }],
        }
    }

    #[test]
    fn splits_large_table_with_repeated_headers() {
        let mut rows = vec![vec!["H1", "H2"]];
        for _ in 0..20 {
            rows.push(vec!["aaaa", "bbbb"]);
        }
        let blocks = vec![table_block(3, rows, true)];
        let chunks = synthesize_chunks(&blocks, 60);
        // Exact split count depends on the counter (heuristic vs BPE).
        assert!(chunks.len() >= 2, "expected a split, got {}", chunks.len());
        let mut ids = std::collections::BTreeSet::new();
        for chunk in &chunks {
            assert!(
                chunk.markdown.starts_with("| H1 | H2 |\n| --- | --- |\n"),
                "missing repeated header: {:?}",
                chunk.markdown
            );
            assert!(
                chunk.token_estimate <= 60,
                "over budget: {}",
                chunk.token_estimate
            );
            assert!(ids.insert(chunk.chunk_id.clone()), "duplicate id");
        }
        // Bboxes partition the block vertically without gaps.
        assert!((chunks.first().expect("split").bbox.y0 - 100.0).abs() < 0.01);
        assert!((chunks.last().expect("split").bbox.y1 - 300.0).abs() < 0.01);
        for pair in chunks.windows(2) {
            assert!((pair[0].bbox.y1 - pair[1].bbox.y0).abs() < 0.01);
        }
        assert!((chunks[0].bbox.x0 - 72.0).abs() < 0.01);
        assert!((chunks[0].bbox.x1 - 540.0).abs() < 0.01);
    }

    #[cfg(feature = "accurate-tokens")]
    #[test]
    fn exact_counts_when_bpe_available() {
        // Offline environments cannot load BPE ranks: skip, don't fail.
        if accurate_count("hello").is_err() {
            return;
        }
        assert_eq!(count_tokens("hello world"), 2);
        // Second call hits the OnceLock cache (same result, no re-init).
        assert_eq!(count_tokens("hello world"), 2);
    }

    #[test]
    fn small_table_stays_single_chunk_byte_identical() {
        let blocks = vec![table_block(0, vec![vec!["H1", "H2"], vec!["a", "b"]], true)];
        let chunks = synthesize_chunks(&blocks, 512);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].markdown, blocks_to_markdown(&blocks));
    }

    #[test]
    fn chunks_carry_block_page_spans() {
        let bbox = BBox::new(72.0, 100.0, 540.0, 120.0);
        let spans = vec![
            crate::models::PageBBox { page: 0, bbox },
            crate::models::PageBBox {
                page: 1,
                bbox: BBox::new(72.0, 100.0, 540.0, 140.0),
            },
        ];
        let block = LayoutBlock {
            id: "b0".to_string(),
            page_index: 0,
            bbox,
            kind: BlockKind::Paragraph,
            raw_text: "Stitched text.".to_string(),
            reading_order: 0,
            page_spans: spans.clone(),
        };
        let chunks = synthesize_chunks(std::slice::from_ref(&block), 512);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].page_spans, spans);
    }
}
