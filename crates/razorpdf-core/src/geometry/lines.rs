//! Horizontal line merging + font-size distribution analysis.
//!
//! Raw [`TextSpan`] fragments are grouped into [`TextLine`]s using the
//! vertical-overlap criterion from the spec, then classified as headings
//! via median body font size analysis.

use crate::models::{BBox, TextSpan};
use anyhow::Result;

/// Default vertical-overlap threshold for merging spans into one line.
pub const DEFAULT_LINE_MERGE_OVERLAP: f32 = 0.55;

/// Pair-check budget for the greedy group scan: each span scans every
/// existing line group, so 10K identical spans cost ~50M checks (~80s).
/// Past the cap the merge hard-errors instead of hanging. Generous: real
/// pages cost tens of thousands of checks at most.
const MAX_MERGE_PAIR_CHECKS: usize = 2_000_000;

/// Maximum horizontal gap (as a multiple of font size) for merging spans.
/// Same-row spans across a column gutter must NOT merge, otherwise the
/// merged box spans the gutter and XY-cut can never split the columns.
pub const MERGE_MAX_GAP_FONT_MULT: f32 = 2.0;

/// Overlap coefficient at/above which spans are treated as stacked
/// double-strikes and kept on separate lines.
pub const STACK_SPLIT_OVERLAP: f32 = 0.5;

/// Minimum vertical containment (`overlap_y / max(ha, hb)`) for merging.
/// Same-line spans score ~1.0; a tall rotated watermark covering a short
/// row scores ~0.05 and stays on its own line.
pub const MERGE_MIN_CONTAINMENT: f32 = 0.3;

/// Default heading threshold multiplier over median body font size.
pub const DEFAULT_HEADING_SIZE_MULT: f32 = 1.15;

/// A merged horizontal line of text.
#[derive(Debug, Clone)]
pub struct TextLine {
    /// Spans composing this line, sorted left-to-right by `x0`.
    pub spans: Vec<TextSpan>,
    /// Union bbox of all spans.
    pub bbox: BBox,
    /// Line text with inter-span gaps resolved to spaces.
    pub text: String,
    /// Median font size of member spans.
    pub font_size: f32,
    /// True when a strict majority of spans is bold.
    pub is_bold: bool,
    /// True when a strict majority of spans is monospace.
    pub is_monospace: bool,
}

/// Merge spans into horizontal lines using `threshold` vertical overlap.
pub fn merge_spans_to_lines_with_threshold(
    spans: &[TextSpan],
    threshold: f32,
) -> Result<Vec<TextLine>> {
    // Order spans top-to-bottom, then left-to-right for deterministic grouping.
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&a, &b| {
        spans[a]
            .bbox
            .y0
            .partial_cmp(&spans[b].bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                spans[a]
                    .bbox
                    .x0
                    .partial_cmp(&spans[b].bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    // Greedy grouping: attach each span to the existing line with the best
    // vertical overlap above threshold, else start a new line. Spans only
    // join a line when horizontally adjacent (no gutter-crossing merges)
    // and not stacked on top of it (double-strikes stay separate so their
    // text is not concatenated).
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_boxes: Vec<BBox> = Vec::new();
    let mut pair_checks: usize = 0;
    for idx in order {
        let span_box = spans[idx].bbox;
        let max_gap = MERGE_MAX_GAP_FONT_MULT * spans[idx].font_size.max(1.0);
        let mut best: Option<(usize, f32)> = None;
        for (gi, gbox) in group_boxes.iter().enumerate() {
            pair_checks += 1;
            if pair_checks > MAX_MERGE_PAIR_CHECKS {
                anyhow::bail!(
                    "line-merge budget exceeded: more than {MAX_MERGE_PAIR_CHECKS} span-group checks on one page"
                );
            }
            let ratio = span_box.vertical_overlap_ratio(gbox);
            if ratio > threshold
                && span_box.h_gap(gbox) <= max_gap
                && span_box.vertical_containment_ratio(gbox) > MERGE_MIN_CONTAINMENT
                && best.is_none_or(|(_, r)| ratio > r)
            {
                // Stacked double-strikes (same text re-struck) stay on
                // separate lines; kerned fragments with distinct text merge.
                if span_box.overlap(gbox) >= STACK_SPLIT_OVERLAP
                    && is_double_strike(spans, &groups[gi], idx)
                {
                    continue;
                }
                best = Some((gi, ratio));
            }
        }
        match best {
            Some((gi, _)) => {
                groups[gi].push(idx);
                group_boxes[gi] = group_boxes[gi].union(&span_box);
            }
            None => {
                groups.push(vec![idx]);
                group_boxes.push(span_box);
            }
        }
    }

    let mut lines: Vec<TextLine> = groups
        .into_iter()
        .map(|mut g| {
            g.sort_by(|&a, &b| {
                spans[a]
                    .bbox
                    .x0
                    .partial_cmp(&spans[b].bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            build_line(spans, &g)
        })
        .collect();

    // Canonical line order: top-to-bottom, then left-to-right.
    lines.sort_by(|a, b| {
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
    Ok(lines)
}

/// Merge spans into horizontal lines with the default 0.55 overlap threshold.
pub fn merge_spans_to_lines(spans: &[TextSpan]) -> Result<Vec<TextLine>> {
    merge_spans_to_lines_with_threshold(spans, DEFAULT_LINE_MERGE_OVERLAP)
}

/// True when the span re-strikes the text of an overlapped member span.
fn is_double_strike(spans: &[TextSpan], group: &[usize], idx: usize) -> bool {
    let text = spans[idx].text.trim();
    if text.is_empty() {
        return true;
    }
    group.iter().any(|&m| {
        spans[m].bbox.overlap(&spans[idx].bbox) >= STACK_SPLIT_OVERLAP
            && spans[m].text.trim() == text
    })
}

/// Build a [`TextLine`] from already x-sorted span indices.
fn build_line(spans: &[TextSpan], idx: &[usize]) -> TextLine {
    debug_assert!(!idx.is_empty());
    let mut bbox = spans[idx[0]].bbox;
    let mut sizes: Vec<f32> = Vec::with_capacity(idx.len());
    let mut bold = 0usize;
    let mut mono = 0usize;
    let mut parts: Vec<String> = Vec::with_capacity(idx.len());

    for (pos, &i) in idx.iter().enumerate() {
        let s = &spans[i];
        bbox = bbox.union(&s.bbox);
        sizes.push(s.font_size);
        if s.is_bold {
            bold += 1;
        }
        if s.is_monospace {
            mono += 1;
        }
        if pos > 0 {
            let prev = &spans[idx[pos - 1]];
            let gap = prev.bbox.h_gap(&s.bbox);
            // Insert a separating space when the inter-span gap is significant
            // relative to the font size, unless whitespace already borders it.
            let need_space = gap > 0.3 * s.font_size.min(prev.font_size)
                && !parts
                    .last()
                    .is_some_and(|p: &String| p.ends_with(char::is_whitespace))
                && !s.text.starts_with(char::is_whitespace);
            if need_space {
                parts.push(" ".to_string());
            } else if gap <= 0.0
                && !parts
                    .last()
                    .is_none_or(|p: &String| p.ends_with(char::is_whitespace))
                && !s.text.starts_with(char::is_whitespace)
            {
                // Touching/overlapping fragments (e.g. kerned Tj splits): no space.
            }
        }
        parts.push(s.text.clone());
    }

    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let font_size = median_sorted(&sizes);
    let n = idx.len();
    TextLine {
        spans: idx.iter().map(|&i| spans[i].clone()).collect(),
        bbox,
        text: parts.concat(),
        font_size,
        is_bold: bold * 2 > n,
        is_monospace: mono * 2 > n,
    }
}

/// Median of a pre-sorted slice; 0.0 when empty.
fn median_sorted(sorted: &[f32]) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) * 0.5
    } else {
        sorted[mid]
    }
}

/// Median body font size across lines (median of line medians).
pub fn median_font_size(lines: &[TextLine]) -> f32 {
    let mut sizes: Vec<f32> = lines.iter().map(|l| l.font_size).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    median_sorted(&sizes)
}

/// Length-weighted median font size: robust body-size estimate on pages
/// where micro-labels (figures, scripts) outnumber body lines.
pub fn body_font_size(lines: &[TextLine]) -> f32 {
    if lines.is_empty() {
        return 0.0;
    }
    let mut by_size: Vec<&TextLine> = lines.iter().collect();
    by_size.sort_by(|a, b| {
        a.font_size
            .partial_cmp(&b.font_size)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total: usize = lines.iter().map(|l| l.text.chars().count().max(1)).sum();
    let mut acc = 0usize;
    for line in by_size {
        acc += line.text.chars().count().max(1);
        if acc * 2 >= total {
            return line.font_size;
        }
    }
    0.0
}

/// Median line height across lines.
pub fn median_line_height(lines: &[TextLine]) -> f32 {
    let mut heights: Vec<f32> = lines.iter().map(|l| l.bbox.height()).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    median_sorted(&heights)
}

/// Heading predicate: `size >= mult * median_body` or a bold isolated line.
///
/// "Isolated" is approximated without layout context as a short bold line
/// (<= 12 words); full gap-aware isolation is resolved by the XY-cut caller.
pub fn classify_heading_with_mult(line: &TextLine, median_body: f32, mult: f32) -> bool {
    if median_body <= 0.0 {
        return false;
    }
    if line.font_size >= mult * median_body {
        return true;
    }
    if line.is_bold && line.text.split_whitespace().count() <= 12 {
        return true;
    }
    false
}

/// Heading predicate with the default 1.15 multiplier.
pub fn classify_heading(line: &TextLine, median_body: f32) -> bool {
    classify_heading_with_mult(line, median_body, DEFAULT_HEADING_SIZE_MULT)
}

/// Heading level from size ratio: >=1.6 → H1, >=1.35 → H2, else H3.
pub fn heading_level(line: &TextLine, median_body: f32) -> u8 {
    if median_body <= 0.0 {
        return 3;
    }
    let ratio = line.font_size / median_body;
    if ratio >= 1.6 {
        1
    } else if ratio >= 1.35 {
        2
    } else {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, x0: f32, y0: f32, x1: f32, y1: f32, size: f32) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            bbox: BBox::new(x0, y0, x1, y1),
            font_name: "Helvetica".to_string(),
            font_size: size,
            is_bold: false,
            is_italic: false,
            is_monospace: false,
        }
    }

    fn bold_span(text: &str, x0: f32, y0: f32, x1: f32, y1: f32, size: f32) -> TextSpan {
        let mut s = span(text, x0, y0, x1, y1, size);
        s.font_name = "Helvetica-Bold".to_string();
        s.is_bold = true;
        s
    }

    #[test]
    fn merges_colinear_spans_with_space() {
        let spans = vec![
            span("Hello", 72.0, 100.0, 110.0, 111.0, 11.0),
            span("world", 125.0, 100.0, 160.0, 111.0, 11.0),
        ];
        let lines = merge_spans_to_lines(&spans).expect("merge");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello world");
    }

    #[test]
    fn splits_stacked_lines() {
        let spans = vec![
            span("Top", 72.0, 100.0, 110.0, 111.0, 11.0),
            span("Bottom", 72.0, 120.0, 120.0, 131.0, 11.0),
        ];
        let lines = merge_spans_to_lines(&spans).expect("merge");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Top");
        assert_eq!(lines[1].text, "Bottom");
    }

    #[test]
    fn kerned_fragments_join_without_space() {
        // Touching fragments (zero gap) join directly.
        let spans = vec![
            span("Hel", 72.0, 100.0, 90.0, 111.0, 11.0),
            span("lo", 90.0, 100.0, 102.0, 111.0, 11.0),
        ];
        let lines = merge_spans_to_lines(&spans).expect("merge");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello");
    }

    #[test]
    fn tall_watermark_span_does_not_swallow_rows() {
        // 200pt-tall rotated span covering two 10pt rows: 3 lines, not 1.
        let spans = vec![
            span("CONFIDENTIAL", 100.0, 200.0, 400.0, 400.0, 36.0),
            span("Row one text", 72.0, 250.0, 300.0, 260.0, 10.0),
            span("Row two text", 72.0, 300.0, 300.0, 310.0, 10.0),
        ];
        let lines = merge_spans_to_lines(&spans).expect("merge");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn large_inline_word_still_merges() {
        // 24pt display word beside 10pt body (containment 0.42): one line.
        let spans = vec![
            span("Sale", 72.0, 100.0, 100.0, 110.0, 10.0),
            span("50%", 105.0, 96.0, 150.0, 120.0, 24.0),
        ];
        let lines = merge_spans_to_lines(&spans).expect("merge");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn body_size_ignores_micro_labels() {
        // 6 tiny figure labels + 2 long body lines → body size wins.
        let mut lines = Vec::new();
        for i in 0..6 {
            lines.push(TextLine {
                spans: vec![],
                bbox: BBox::new(0.0, i as f32 * 20.0, 10.0, i as f32 * 20.0 + 3.0),
                text: "x".to_string(),
                font_size: 3.0,
                is_bold: false,
                is_monospace: false,
            });
        }
        for i in 0..2 {
            lines.push(TextLine {
                spans: vec![],
                bbox: BBox::new(0.0, 200.0 + i as f32 * 20.0, 400.0, 211.0 + i as f32 * 20.0),
                text: "a reasonably long body line with many characters in it".to_string(),
                font_size: 11.0,
                is_bold: false,
                is_monospace: false,
            });
        }
        assert!((median_font_size(&lines) - 3.0).abs() < 1e-6);
        assert!((body_font_size(&lines) - 11.0).abs() < 1e-6);
    }

    #[test]
    fn stacked_duplicates_stay_split() {
        // Double-strike (overlap ~= 0.96): separate lines, no doubled text.
        let spans = vec![
            span("Item0", 72.0, 100.0, 102.0, 111.0, 11.0),
            span("Item0", 73.2, 100.0, 103.2, 111.0, 11.0),
        ];
        let lines = merge_spans_to_lines(&spans).expect("merge");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Item0");
    }

    #[test]
    fn gutter_separated_spans_stay_split() {
        // Same baseline, 150pt gutter at 10pt font: two lines, not one.
        let spans = vec![
            span("Left", 72.0, 100.0, 110.0, 110.0, 10.0),
            span("Right", 330.0, 100.0, 370.0, 110.0, 10.0),
        ];
        let lines = merge_spans_to_lines(&spans).expect("merge");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn merged_line_inherits_bold_majority() {
        let spans = vec![
            bold_span("Sec", 72.0, 100.0, 90.0, 111.0, 11.0),
            bold_span("tion", 93.0, 100.0, 120.0, 111.0, 11.0),
            span("x", 123.0, 100.0, 130.0, 111.0, 11.0),
        ];
        let lines = merge_spans_to_lines(&spans).expect("merge");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].is_bold);
    }

    #[test]
    fn median_and_heading_classification() {
        let lines = vec![
            TextLine {
                spans: vec![],
                bbox: BBox::new(72.0, 100.0, 300.0, 111.0),
                text: "body one two three".to_string(),
                font_size: 11.0,
                is_bold: false,
                is_monospace: false,
            },
            TextLine {
                spans: vec![],
                bbox: BBox::new(72.0, 120.0, 300.0, 131.0),
                text: "body four five six".to_string(),
                font_size: 11.0,
                is_bold: false,
                is_monospace: false,
            },
            TextLine {
                spans: vec![],
                bbox: BBox::new(72.0, 60.0, 300.0, 80.0),
                text: "Big Title".to_string(),
                font_size: 20.0,
                is_bold: true,
                is_monospace: false,
            },
        ];
        let med = median_font_size(&lines);
        assert!((med - 11.0).abs() < 1e-6);
        assert!(!classify_heading(&lines[0], med));
        assert!(classify_heading(&lines[2], med));
        assert_eq!(heading_level(&lines[2], med), 1);
    }

    #[test]
    fn bold_short_line_is_heading() {
        let line = TextLine {
            spans: vec![],
            bbox: BBox::new(72.0, 100.0, 200.0, 111.0),
            text: "Section Intro".to_string(),
            font_size: 11.0,
            is_bold: true,
            is_monospace: false,
        };
        assert!(classify_heading(&line, 11.0));
        assert_eq!(heading_level(&line, 11.0), 3);
    }

    #[test]
    fn identical_span_storm_hits_merge_budget() {
        // HG-04: 10K identical spans made the greedy group scan O(n^2)
        // (~80s debug). A pair-check budget fails fast with a named error.
        let spans: Vec<TextSpan> = (0..10_000)
            .map(|_| span("same", 72.0, 100.0, 110.0, 111.0, 11.0))
            .collect();
        let err = format!("{:?}", merge_spans_to_lines(&spans).unwrap_err());
        assert!(err.contains("budget"), "must name the budget: {err}");
    }

    #[test]
    fn merge_threshold_config_is_live() {
        // HG-09: `line_merge_overlap` was write-only (callers hardcoded
        // the const). Two spans with 0.73 vertical overlap merge at the
        // default 0.55 but stay split at 0.99.
        let spans = vec![
            span("alpha", 72.0, 100.0, 110.0, 111.0, 11.0),
            span("beta", 72.0, 103.0, 110.0, 114.0, 11.0),
        ];
        let merged = merge_spans_to_lines_with_threshold(&spans, 0.55).expect("merge");
        assert_eq!(merged.len(), 1, "0.73 overlap merges at 0.55");
        let split = merge_spans_to_lines_with_threshold(&spans, 0.99).expect("merge");
        assert_eq!(split.len(), 2, "0.73 overlap splits at 0.99");
    }

    #[test]
    fn heading_mult_config_is_live() {
        // HG-09: `heading_size_mult` was write-only. A 1.2x line is a
        // heading at mult 1.15 but body text at 1.5 (non-bold isolates
        // the size rule from the bold-short-line rule).
        let line = TextLine {
            spans: vec![],
            bbox: BBox::new(72.0, 100.0, 200.0, 111.0),
            text: "A moderately long section title here".to_string(),
            font_size: 13.2,
            is_bold: false,
            is_monospace: false,
        };
        assert!(classify_heading_with_mult(&line, 11.0, 1.15));
        assert!(!classify_heading_with_mult(&line, 11.0, 1.5));
    }

    #[test]
    fn config_defaults_match_geometry_consts() {
        // The threading above is behavior-neutral ONLY because defaults
        // equal the consts. Pin the equivalence: changing a default is
        // an intentional behavior change, not drift.
        use crate::config::ParserConfig;
        let config = ParserConfig::default();
        assert_eq!(config.line_merge_overlap, DEFAULT_LINE_MERGE_OVERLAP);
        assert_eq!(config.heading_size_mult, DEFAULT_HEADING_SIZE_MULT);
    }
}
