//! Horizontal line merging + font-size distribution analysis.
//!
//! Raw [`TextSpan`] fragments are grouped into [`TextLine`]s using the
//! vertical-overlap criterion from the spec, then classified as headings
//! via median body font size analysis.

use crate::models::{BBox, TextSpan};

/// Default vertical-overlap threshold for merging spans into one line.
pub const DEFAULT_LINE_MERGE_OVERLAP: f32 = 0.55;

/// Maximum horizontal gap (as a multiple of font size) for merging spans.
/// Same-row spans across a column gutter must NOT merge, otherwise the
/// merged box spans the gutter and XY-cut can never split the columns.
pub const MERGE_MAX_GAP_FONT_MULT: f32 = 2.0;

/// Overlap coefficient at/above which spans are treated as stacked
/// double-strikes and kept on separate lines.
pub const STACK_SPLIT_OVERLAP: f32 = 0.5;

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
pub fn merge_spans_to_lines_with_threshold(spans: &[TextSpan], threshold: f32) -> Vec<TextLine> {
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
    for idx in order {
        let span_box = spans[idx].bbox;
        let max_gap = MERGE_MAX_GAP_FONT_MULT * spans[idx].font_size.max(1.0);
        let mut best: Option<(usize, f32)> = None;
        for (gi, gbox) in group_boxes.iter().enumerate() {
            let ratio = span_box.vertical_overlap_ratio(gbox);
            if ratio > threshold
                && span_box.h_gap(gbox) <= max_gap
                && span_box.overlap(gbox) < STACK_SPLIT_OVERLAP
                && best.is_none_or(|(_, r)| ratio > r)
            {
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
    lines
}

/// Merge spans into horizontal lines with the default 0.55 overlap threshold.
pub fn merge_spans_to_lines(spans: &[TextSpan]) -> Vec<TextLine> {
    merge_spans_to_lines_with_threshold(spans, DEFAULT_LINE_MERGE_OVERLAP)
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
        let lines = merge_spans_to_lines(&spans);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello world");
    }

    #[test]
    fn splits_stacked_lines() {
        let spans = vec![
            span("Top", 72.0, 100.0, 110.0, 111.0, 11.0),
            span("Bottom", 72.0, 120.0, 120.0, 131.0, 11.0),
        ];
        let lines = merge_spans_to_lines(&spans);
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
        let lines = merge_spans_to_lines(&spans);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello");
    }

    #[test]
    fn stacked_duplicates_stay_split() {
        // Double-strike (overlap ~= 0.96): separate lines, no doubled text.
        let spans = vec![
            span("Item0", 72.0, 100.0, 102.0, 111.0, 11.0),
            span("Item0", 73.2, 100.0, 103.2, 111.0, 11.0),
        ];
        let lines = merge_spans_to_lines(&spans);
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
        let lines = merge_spans_to_lines(&spans);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn merged_line_inherits_bold_majority() {
        let spans = vec![
            bold_span("Sec", 72.0, 100.0, 90.0, 111.0, 11.0),
            bold_span("tion", 93.0, 100.0, 120.0, 111.0, 11.0),
            span("x", 123.0, 100.0, 130.0, 111.0, 11.0),
        ];
        let lines = merge_spans_to_lines(&spans);
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
}
