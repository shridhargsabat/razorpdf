//! Cross-page continuity: stitch tables and paragraphs split across pages.
//!
//! Operates on finished per-page [`LayoutBlock`]s in [`stitch_across_pages`],
//! after XY-cut layout and before Markdown/chunk emission.

use crate::config::ParserConfig;
use crate::models::{BBox, BlockKind, LayoutBlock, PageBBox};

/// Minimum mean column X-interval IoU for table stitching.
pub const COLUMN_IOU_THRESHOLD: f32 = 0.85;

/// Sentence-terminal punctuation: a paragraph ending with one of these
/// never continues on the next page.
const TERMINAL_PUNCT: &[char] = &['.', '?', '!', ':', ';', '"', '\u{201D}'];

/// Outcome counters for one [`stitch_across_pages`] pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CrossPageStitchStats {
    /// Table pairs merged across a page boundary.
    pub tables_stitched: usize,
    /// Repeated header rows dropped from page N+1 tables.
    pub headers_deduplicated: usize,
    /// Paragraph pairs merged (incl. de-hyphenated) across a boundary.
    pub paragraphs_merged: usize,
}

/// Stitch tables/paragraphs split across consecutive pages, in place.
///
/// For each page pair (N, N+1): merges a trailing `DeterministicTable`
/// with a leading one when column counts match and mean column X-IoU is
/// at least [`COLUMN_IOU_THRESHOLD`] (dedupes repeated headers, folds
/// continuation cells upward); merges a trailing unterminated `Paragraph`
/// with a leading lowercase one (de-hyphenating split words). Stitched
/// blocks absorb the continuation rows/text, union the bbox, and record
/// the extra page in `page_spans`. No-op when
/// `enable_cross_page_stitching` is false.
pub fn stitch_across_pages(
    pages: &mut [Vec<LayoutBlock>],
    config: &ParserConfig,
) -> CrossPageStitchStats {
    if !config.enable_cross_page_stitching || pages.len() < 2 {
        return CrossPageStitchStats::default();
    }
    let mut tables_stitched = 0;
    let mut headers_deduplicated = 0;
    let mut paragraphs_merged = 0;
    for n in 0..pages.len() - 1 {
        let (head, tail) = pages.split_at_mut(n + 1);
        let (prev, next) = (&mut head[n], &mut tail[0]);
        match try_stitch_tables(prev, next) {
            Some(deduped) => {
                tables_stitched += 1;
                if deduped {
                    headers_deduplicated += 1;
                }
            }
            None => {
                if try_stitch_paragraph(prev, next) {
                    paragraphs_merged += 1;
                }
            }
        }
    }
    CrossPageStitchStats {
        tables_stitched,
        headers_deduplicated,
        paragraphs_merged,
    }
}

/// Column count, bbox, and header row of a table block (or `None`).
fn table_shape(block: &LayoutBlock) -> Option<(usize, BBox, Vec<String>)> {
    match &block.kind {
        BlockKind::DeterministicTable { rows, .. } if !rows.is_empty() => {
            Some((rows[0].len(), block.bbox, rows[0].clone()))
        }
        _ => None,
    }
}

/// Mean 1D IoU of even-split column X-intervals between two table bboxes.
///
/// Blocks carry no per-column geometry, so each bbox width is split
/// evenly by column count; genuine continuations share x-span and column
/// count, so their splits align near 1.0.
fn mean_column_iou(a: BBox, b: BBox, cols: usize) -> f32 {
    let aw = (a.x1 - a.x0).max(1.0) / cols as f32;
    let bw = (b.x1 - b.x0).max(1.0) / cols as f32;
    let mut sum = 0.0;
    for c in 0..cols {
        let a0 = a.x0 + c as f32 * aw;
        let b0 = b.x0 + c as f32 * bw;
        let inter = (a0 + aw).min(b0 + bw) - a0.max(b0);
        let union = (a0 + aw).max(b0 + bw) - a0.min(b0);
        sum += if union <= 0.0 {
            1.0
        } else {
            inter.max(0.0) / union
        };
    }
    sum / cols as f32
}

/// Normalize one header cell for repeated-header comparison.
fn normalize_header_cell(cell: &str) -> String {
    cell.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// True when two header rows match cell-wise after normalization.
fn headers_match(a: &[String], b: &[String]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| normalize_header_cell(x) == normalize_header_cell(y))
}

/// Merge N's trailing table with N+1's leading table. Returns
/// `Some(header_deduped)` on a merge, `None` when the pair cannot join.
fn try_stitch_tables(prev: &mut [LayoutBlock], next: &mut Vec<LayoutBlock>) -> Option<bool> {
    let last_idx = prev.len().checked_sub(1)?;
    let first_idx = next
        .iter()
        .position(|b| !matches!(b.kind, BlockKind::Heading { .. }))?;
    let (n_cols, n_bbox, n_header) = table_shape(&prev[last_idx])?;
    let (m_cols, m_bbox, m_header) = table_shape(&next[first_idx])?;
    if n_cols == 0 || n_cols != m_cols {
        return None;
    }
    if mean_column_iou(n_bbox, m_bbox, n_cols) < COLUMN_IOU_THRESHOLD {
        return None;
    }
    let dedup = headers_match(&n_header, &m_header);
    let next_block = next.remove(first_idx);
    let (next_page, next_bbox) = (next_block.page_index, next_block.bbox);
    let mut next_rows = match next_block.kind {
        BlockKind::DeterministicTable { rows, .. } => rows,
        _ => return None,
    };
    if dedup {
        next_rows.remove(0);
    }
    // Continuation row: empty leading cells mean the row finishes N's
    // final row, so fold its non-empty cells upward and drop it.
    if next_rows
        .first()
        .is_some_and(|r| r.first().is_some_and(|c| c.trim().is_empty()))
    {
        let first = next_rows.remove(0);
        if let BlockKind::DeterministicTable { rows, .. } = &mut prev[last_idx].kind {
            if let Some(last) = rows.last_mut() {
                for (c, cell) in first.iter().enumerate() {
                    if c < last.len() && !cell.trim().is_empty() {
                        if last[c].trim().is_empty() {
                            last[c] = cell.clone();
                        } else {
                            last[c] = format!("{} {}", last[c].trim_end(), cell.trim());
                        }
                    }
                }
            }
        }
    }
    let merged_text = match &mut prev[last_idx].kind {
        BlockKind::DeterministicTable { rows, .. } => {
            rows.append(&mut next_rows);
            Some(
                rows.iter()
                    .map(|r| r.join(" | "))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        _ => None,
    };
    let prev_block = &mut prev[last_idx];
    if let Some(text) = merged_text {
        prev_block.raw_text = text;
    }
    prev_block.bbox = prev_block.bbox.union(&next_bbox);
    prev_block.page_spans.push(PageBBox {
        page: next_page as u32,
        bbox: next_bbox,
    });
    Some(dedup)
}

/// True for assembler-generated form-field blocks (never merge targets).
fn is_form_block(block: &LayoutBlock) -> bool {
    block.id.contains("-form")
}

/// Merge N's trailing unterminated paragraph with N+1's leading
/// lowercase paragraph, de-hyphenating split words.
fn try_stitch_paragraph(prev: &mut [LayoutBlock], next: &mut Vec<LayoutBlock>) -> bool {
    let Some(last_idx) = prev.len().checked_sub(1) else {
        return false;
    };
    let Some(first_idx) = next
        .iter()
        .position(|b| !matches!(b.kind, BlockKind::Heading { .. }))
    else {
        return false;
    };
    if is_form_block(&prev[last_idx]) || is_form_block(&next[first_idx]) {
        return false;
    }
    if !matches!(prev[last_idx].kind, BlockKind::Paragraph)
        || !matches!(next[first_idx].kind, BlockKind::Paragraph)
    {
        return false;
    }
    let tail = prev[last_idx].raw_text.trim_end().to_string();
    if tail
        .chars()
        .last()
        .is_none_or(|c| TERMINAL_PUNCT.contains(&c))
    {
        return false;
    }
    let head = next[first_idx].raw_text.trim_start().to_string();
    if head.chars().next().is_none_or(|c| !c.is_lowercase()) {
        return false;
    }
    let next_block = next.remove(first_idx);
    let joined = match tail.strip_suffix('-') {
        Some(stem) => format!("{stem}{head}"),
        None => format!("{tail} {head}"),
    };
    let prev_block = &mut prev[last_idx];
    prev_block.raw_text = joined;
    prev_block.bbox = prev_block.bbox.union(&next_block.bbox);
    prev_block.page_spans.push(PageBBox {
        page: next_block.page_index as u32,
        bbox: next_block.bbox,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ParserConfig;
    use crate::models::{BBox, PageBBox};

    fn table_block(
        page: usize,
        order: usize,
        x0: f32,
        x1: f32,
        rows: Vec<Vec<&str>>,
    ) -> LayoutBlock {
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|r| r.into_iter().map(str::to_string).collect())
            .collect();
        let bbox = BBox::new(x0, 100.0, x1, 300.0);
        LayoutBlock {
            id: format!("p{page}-table{order}"),
            page_index: page,
            bbox,
            kind: BlockKind::DeterministicTable {
                rows,
                has_header: true,
            },
            raw_text: String::new(),
            reading_order: order,
            page_spans: vec![PageBBox {
                page: page as u32,
                bbox,
            }],
        }
    }

    fn para_block(page: usize, order: usize, text: &str) -> LayoutBlock {
        let bbox = BBox::new(72.0, 100.0, 540.0, 120.0);
        LayoutBlock {
            id: format!("p{page}-line{order}"),
            page_index: page,
            bbox,
            kind: BlockKind::Paragraph,
            raw_text: text.to_string(),
            reading_order: order,
            page_spans: vec![PageBBox {
                page: page as u32,
                bbox,
            }],
        }
    }

    #[test]
    fn stitched_tables_merge_and_dedup_headers() {
        let mut pages = vec![
            vec![table_block(
                0,
                0,
                72.0,
                540.0,
                vec![vec!["Item", "Qty", "Price"], vec!["Apples", "10", "$5"]],
            )],
            vec![table_block(
                1,
                0,
                72.0,
                540.0,
                vec![vec!["Item", "Qty", "Price"], vec!["Pears", "4", "$3"]],
            )],
        ];
        let stats = stitch_across_pages(&mut pages, &ParserConfig::default());
        assert_eq!(
            stats,
            CrossPageStitchStats {
                tables_stitched: 1,
                headers_deduplicated: 1,
                paragraphs_merged: 0
            }
        );
        assert_eq!(pages[0].len(), 1);
        assert!(pages[1].is_empty());
        match &pages[0][0].kind {
            BlockKind::DeterministicTable { rows, .. } => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[2], vec!["Pears", "4", "$3"]);
            }
            other => panic!("expected table, got {other:?}"),
        }
        assert_eq!(pages[0][0].page_spans.len(), 2);
    }

    #[test]
    fn split_continuation_cells_merge_upward() {
        let mut pages = vec![
            vec![table_block(
                0,
                0,
                72.0,
                540.0,
                vec![vec!["Item", "Desc"], vec!["Widget", "blue"]],
            )],
            vec![table_block(1, 0, 72.0, 540.0, vec![vec!["", "large"]])],
        ];
        let stats = stitch_across_pages(&mut pages, &ParserConfig::default());
        assert_eq!(stats.tables_stitched, 1);
        assert_eq!(stats.headers_deduplicated, 0);
        assert!(pages[1].is_empty());
        match &pages[0][0].kind {
            BlockKind::DeterministicTable { rows, .. } => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[1], vec!["Widget", "blue large"]);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn column_mismatch_blocks_stitch() {
        let mut pages = vec![
            vec![table_block(0, 0, 72.0, 540.0, vec![vec!["A", "B", "C"]])],
            vec![table_block(1, 0, 72.0, 540.0, vec![vec!["X", "Y"]])],
        ];
        let stats = stitch_across_pages(&mut pages, &ParserConfig::default());
        assert_eq!(stats, CrossPageStitchStats::default());
        assert_eq!(pages[0].len(), 1);
        assert_eq!(pages[1].len(), 1);
    }

    #[test]
    fn narrow_table_blocks_stitch() {
        let mut pages = vec![
            vec![table_block(0, 0, 72.0, 540.0, vec![vec!["A", "B", "C"]])],
            vec![table_block(1, 0, 72.0, 200.0, vec![vec!["X", "Y", "Z"]])],
        ];
        let stats = stitch_across_pages(&mut pages, &ParserConfig::default());
        assert_eq!(stats, CrossPageStitchStats::default());
        assert_eq!(pages[1].len(), 1);
    }

    #[test]
    fn paragraphs_merge_and_dehyphenate() {
        let mut pages = vec![
            vec![
                para_block(0, 0, "Intro para."),
                para_block(0, 1, "The exam-"),
            ],
            vec![para_block(1, 0, "ple was hard."), para_block(1, 1, "Next.")],
        ];
        let stats = stitch_across_pages(&mut pages, &ParserConfig::default());
        assert_eq!(stats.paragraphs_merged, 1);
        assert_eq!(pages[0].len(), 2);
        assert_eq!(pages[0][1].raw_text, "The example was hard.");
        assert_eq!(pages[1].len(), 1);
        assert_eq!(pages[1][0].raw_text, "Next.");
    }

    #[test]
    fn terminal_punctuation_blocks_merge() {
        let mut pages = vec![
            vec![para_block(0, 0, "Done.")],
            vec![para_block(1, 0, "next page starts.")],
        ];
        let stats = stitch_across_pages(&mut pages, &ParserConfig::default());
        assert_eq!(stats, CrossPageStitchStats::default());
        assert_eq!(pages[0][0].raw_text, "Done.");
    }

    #[test]
    fn uppercase_start_blocks_merge() {
        let mut pages = vec![
            vec![para_block(0, 0, "He went")],
            vec![para_block(1, 0, "Paris today")],
        ];
        let stats = stitch_across_pages(&mut pages, &ParserConfig::default());
        assert_eq!(stats, CrossPageStitchStats::default());
        assert_eq!(pages[1].len(), 1);
    }

    #[test]
    fn disabled_flag_skips_stitching() {
        let config = ParserConfig {
            enable_cross_page_stitching: false,
            ..ParserConfig::default()
        };
        let mut pages = vec![
            vec![para_block(0, 0, "The exam-")],
            vec![para_block(1, 0, "ple was hard.")],
        ];
        let stats = stitch_across_pages(&mut pages, &config);
        assert_eq!(stats, CrossPageStitchStats::default());
        assert_eq!(pages[0][0].raw_text, "The exam-");
    }
}
