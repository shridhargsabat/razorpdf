//! Deterministic spatial math-to-LaTeX reconstruction.
//!
//! Common inline/display equations rebuild from span geometry (math
//! fonts, baseline sub/superscript shifts, horizontal fraction bars) in
//! microseconds; only high-entropy formulas keep their VLM crops.

use std::collections::BTreeMap;

use crate::extractor::fonts::is_math_font;
use crate::geometry::lines::TextLine;
use crate::models::{BBox, TextSpan, VectorSegment};

/// Minimum reconstruction confidence for deterministic emission.
pub const MATH_CONFIDENCE_THRESHOLD: f32 = 0.80;

/// Minimum math-signal confidence floor before signal bonuses.
const BASE_CONFIDENCE: f32 = 0.60;
/// Bonus per distinct signal (math font, math symbol, script shift).
const SIGNAL_BONUS: f32 = 0.12;
/// Bonus for a valid numerator/denominator fraction bar.
const FRACTION_BONUS: f32 = 0.25;
/// Confidence cap (deterministic output is never asserted as certain).
const MAX_CONFIDENCE: f32 = 0.95;
/// Script size ceiling as a fraction of the median span size.
const SCRIPT_SIZE_FRAC: f32 = 0.85;
/// Script baseline shift window as a fraction of parent height.
const SCRIPT_SHIFT_MIN: f32 = 0.15;
const SCRIPT_SHIFT_MAX: f32 = 0.65;
/// Inter-token gap (× median size) that emits a separating space.
const SPACE_GAP_MULT: f32 = 0.3;

/// Reconstruct inner LaTeX (no `$` wrapping) from positioned spans.
/// Returns the LaTeX body plus confidence in `[0.0, 1.0]`, or `None`
/// when the spans carry no math signal at all.
pub fn reconstruct_latex_from_spans(
    spans: &[TextSpan],
    vectors: &[VectorSegment],
) -> Option<(String, f32)> {
    if spans.is_empty() {
        return None;
    }
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&a, &b| {
        spans[a]
            .bbox
            .x0
            .partial_cmp(&spans[b].bbox.x0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Fraction bars partition first; the rest flows inline.
    if let Some((num, den, bar_x0)) = split_fraction(spans, &order, vectors) {
        let (num_tex, num_sig) = reconstruct_tokens(spans, &num);
        let (den_tex, den_sig) = reconstruct_tokens(spans, &den);
        let inline: Vec<usize> = order
            .iter()
            .filter(|i| !num.contains(i) && !den.contains(i))
            .copied()
            .collect();
        let (mut latex, mut signals) = reconstruct_tokens(spans, &inline);
        signals.fraction = true;
        let frac = format!("\\frac{{{num_tex}}}{{{den_tex}}}");
        // Splice the fraction at the bar's x position.
        if inline.is_empty() {
            latex = frac;
        } else {
            let pos = inline.partition_point(|&i| spans[i].bbox.x0 < bar_x0);
            let (head, tail) = split_tokens_at(&latex, pos);
            latex = format!("{head}{frac}{tail}");
        }
        signals.merge(num_sig);
        signals.merge(den_sig);
        return Some((latex, signals.confidence()));
    }
    let (latex, signals) = reconstruct_tokens(spans, &order);
    if !signals.any() {
        return None;
    }
    Some((latex, signals.confidence()))
}

/// Rewrite high-confidence math lines to `$...$`/`$$...$$` LaTeX.
///
/// Fully mathematical lines use `$$...$$`; mixed lines splice `$...$`
/// around each contiguous reconstructed math run and keep prose verbatim.
/// Returns the (possibly rewritten) lines plus tight bboxes of rewritten
/// runs so triage can drop their VLM regions.
pub fn rewrite_math_lines(
    lines: Vec<TextLine>,
    vectors: &[VectorSegment],
) -> (Vec<TextLine>, Vec<BBox>) {
    let mut rewritten = Vec::with_capacity(lines.len());
    let mut math_boxes = Vec::new();
    for mut line in lines {
        if line.spans.is_empty() {
            rewritten.push(line);
            continue;
        }
        let median_size = median(line.spans.iter().map(|s| s.font_size).collect()).max(1.0);
        let mut order: Vec<usize> = (0..line.spans.len()).collect();
        order.sort_by(|&a, &b| {
            line.spans[a]
                .bbox
                .x0
                .partial_cmp(&line.spans[b].bbox.x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let math: Vec<bool> = order
            .iter()
            .map(|&i| is_math_span(&line.spans[i], median_size))
            .collect();
        if math.iter().all(|m| *m) {
            let hit = reconstruct_latex_from_spans(&line.spans, vectors)
                .filter(|(_, conf)| *conf >= MATH_CONFIDENCE_THRESHOLD);
            if let Some((inner, _)) = hit {
                line.text = format!("$${inner}$$");
                math_boxes.push(line.bbox);
            }
        } else if math.iter().any(|m| *m) {
            if let Some((text, boxes)) =
                splice_mixed_line(&line.spans, &order, &math, vectors, median_size)
            {
                line.text = text;
                math_boxes.extend(boxes);
            }
        }
        rewritten.push(line);
    }
    (rewritten, math_boxes)
}

/// True for spans that read as math: math fonts, math symbols, or
/// script-sized glyphs. Reconstruction still validates each run, so
/// false positives here fall back to original text.
fn is_math_span(span: &TextSpan, median_size: f32) -> bool {
    if is_math_font(&span.font_name) {
        return true;
    }
    if span.text.chars().any(|c| latex_for_char(c).is_some()) {
        return true;
    }
    span.font_size < SCRIPT_SIZE_FRAC * median_size
}

/// Splice `$...$` around reconstructed math runs in a mixed line.
/// Returns `None` when no run clears the confidence threshold (the
/// caller then keeps the original text untouched).
fn splice_mixed_line(
    spans: &[TextSpan],
    order: &[usize],
    math: &[bool],
    vectors: &[VectorSegment],
    median_size: f32,
) -> Option<(String, Vec<BBox>)> {
    // Partition x-ordered spans into maximal math/prose runs.
    let mut runs: Vec<(bool, Vec<usize>)> = Vec::new();
    for (pos, &i) in order.iter().enumerate() {
        match runs.last_mut() {
            Some((kind, ids)) if *kind == math[pos] => ids.push(i),
            _ => runs.push((math[pos], vec![i])),
        }
    }
    let mut out = String::new();
    let mut boxes = Vec::new();
    let mut prev_x1 = f32::MIN;
    let mut rewrote_any = false;
    for (is_math, ids) in &runs {
        let run_spans: Vec<TextSpan> = ids.iter().map(|&i| spans[i].clone()).collect();
        let run_x0 = run_spans.iter().map(|s| s.bbox.x0).fold(f32::MAX, f32::min);
        if prev_x1 != f32::MIN && run_x0 - prev_x1 > SPACE_GAP_MULT * median_size {
            out.push(' ');
        }
        if *is_math {
            let run_x1 = run_spans.iter().map(|s| s.bbox.x1).fold(f32::MIN, f32::max);
            let run_vectors: Vec<VectorSegment> = vectors
                .iter()
                .filter(|v| v.x0 < run_x1 && v.x1 > run_x0)
                .copied()
                .collect();
            let hit = reconstruct_latex_from_spans(&run_spans, &run_vectors)
                .filter(|(_, conf)| *conf >= MATH_CONFIDENCE_THRESHOLD);
            if let Some((inner, _)) = hit {
                out.push_str(&format!("${inner}$"));
                boxes.push(run_bbox(&run_spans));
                rewrote_any = true;
                prev_x1 = run_x1;
                continue;
            }
        }
        out.push_str(&join_original(&run_spans, median_size));
        prev_x1 = run_spans.iter().map(|s| s.bbox.x1).fold(f32::MIN, f32::max);
    }
    rewrote_any.then_some((out, boxes))
}

/// Union bbox of a span run.
fn run_bbox(spans: &[TextSpan]) -> BBox {
    spans
        .iter()
        .map(|s| s.bbox)
        .reduce(|a, b| a.union(&b))
        .unwrap_or_else(BBox::zero)
}

/// Join original span texts with gap-driven spacing.
fn join_original(spans: &[TextSpan], median_size: f32) -> String {
    let mut out = String::new();
    let mut prev_x1 = f32::MIN;
    for span in spans {
        if prev_x1 != f32::MIN && span.bbox.x0 - prev_x1 > SPACE_GAP_MULT * median_size {
            out.push(' ');
        }
        out.push_str(span.text.trim());
        prev_x1 = span.bbox.x1;
    }
    out
}

// ---------------------------------------------------------------------------
// Token reconstruction
// ---------------------------------------------------------------------------

/// Math signals observed during reconstruction.
#[derive(Debug, Clone, Copy, Default)]
struct MathSignals {
    font: bool,
    symbol: bool,
    script: bool,
    fraction: bool,
}

impl MathSignals {
    fn any(self) -> bool {
        self.font || self.symbol || self.script || self.fraction
    }

    fn merge(&mut self, other: Self) {
        self.font |= other.font;
        self.symbol |= other.symbol;
        self.script |= other.script;
        self.fraction |= other.fraction;
    }

    fn confidence(self) -> f32 {
        let mut conf = BASE_CONFIDENCE;
        for present in [self.font, self.symbol, self.script] {
            if present {
                conf += SIGNAL_BONUS;
            }
        }
        if self.fraction {
            conf += FRACTION_BONUS;
        }
        conf.min(MAX_CONFIDENCE)
    }
}

/// Median of a non-empty size list (callers guarantee non-empty).
fn median(mut values: Vec<f32>) -> f32 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values[values.len() / 2]
}

/// Reconstruct one token stream (no fraction handling) from span ids.
fn reconstruct_tokens(spans: &[TextSpan], ids: &[usize]) -> (String, MathSignals) {
    let mut signals = MathSignals::default();
    if ids.is_empty() {
        return (String::new(), signals);
    }
    let median_size = median(ids.iter().map(|&i| spans[i].font_size).collect()).max(1.0);
    // Base spans anchor the baseline; smaller shifted spans are scripts.
    let base: Vec<usize> = ids
        .iter()
        .filter(|&&i| spans[i].font_size >= SCRIPT_SIZE_FRAC * median_size)
        .copied()
        .collect();
    let base_centers: Vec<f32> = base
        .iter()
        .map(|&i| (spans[i].bbox.y0 + spans[i].bbox.y1) * 0.5)
        .collect();
    let base_center = if base_centers.is_empty() {
        (spans[ids[0]].bbox.y0 + spans[ids[0]].bbox.y1) * 0.5
    } else {
        median(base_centers)
    };
    let parent_h = base
        .iter()
        .map(|&i| spans[i].bbox.y1 - spans[i].bbox.y0)
        .collect::<Vec<_>>();
    let parent_h = if parent_h.is_empty() {
        1.0
    } else {
        median(parent_h).max(1.0)
    };

    // Classify scripts and attach each to its nearest preceding base.
    let mut subs: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut sups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut is_script = vec![false; spans.len()];
    for &i in ids {
        if spans[i].font_size >= SCRIPT_SIZE_FRAC * median_size {
            continue;
        }
        let center = (spans[i].bbox.y0 + spans[i].bbox.y1) * 0.5;
        let shift = (base_center - center) / parent_h;
        if !(SCRIPT_SHIFT_MIN..=SCRIPT_SHIFT_MAX).contains(&shift.abs()) {
            continue;
        }
        let base_idx = base
            .iter()
            .filter(|&&b| spans[b].bbox.x1 <= spans[i].bbox.x0 + 0.3 * median_size)
            .max_by(|a, b| {
                spans[**a]
                    .bbox
                    .x1
                    .partial_cmp(&spans[**b].bbox.x1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some(&b) = base_idx {
            is_script[i] = true;
            signals.script = true;
            if shift > 0.0 {
                sups.entry(b).or_default().push(i);
            } else {
                subs.entry(b).or_default().push(i);
            }
        }
    }

    // Emit base tokens left-to-right with gap-driven spacing.
    let mut out = String::new();
    let mut prev_x1 = f32::MIN;
    for &i in ids {
        if is_script[i] {
            continue;
        }
        if is_math_font(&spans[i].font_name) {
            signals.font = true;
        }
        if prev_x1 != f32::MIN && spans[i].bbox.x0 - prev_x1 > SPACE_GAP_MULT * median_size {
            out.push(' ');
        } else if needs_brace_break(&out) {
            out.push_str("{}");
        }
        let (mut token, sym) = map_span_text(&spans[i].text);
        signals.symbol |= sym;
        if let Some(group) = subs.get(&i) {
            token.push_str(&format!("_{{{}}}", group_text(spans, group)));
        }
        if let Some(group) = sups.get(&i) {
            token.push_str(&format!("^{{{}}}", group_text(spans, group)));
        }
        out.push_str(&token);
        let mut x1 = spans[i].bbox.x1;
        for g in subs.get(&i).into_iter().chain(sups.get(&i)) {
            for &s in g {
                x1 = x1.max(spans[s].bbox.x1);
                let (_, sym) = map_span_text(&spans[s].text);
                signals.symbol |= sym;
                if is_math_font(&spans[s].font_name) {
                    signals.font = true;
                }
            }
        }
        prev_x1 = x1;
    }
    (out, signals)
}

/// Concatenated mapped text of a script group (x-ordered).
fn group_text(spans: &[TextSpan], group: &[usize]) -> String {
    let mut sorted = group.to_vec();
    sorted.sort_by(|&a, &b| {
        spans[a]
            .bbox
            .x0
            .partial_cmp(&spans[b].bbox.x0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted
        .iter()
        .map(|&i| map_span_text(&spans[i].text).0)
        .collect::<Vec<_>>()
        .join("")
}

/// True when appending an ASCII letter next would fuse with a trailing
/// LaTeX command (`\alpha` + `x`); callers insert `{}` first.
fn needs_brace_break(out: &str) -> bool {
    let tail: String = out.chars().rev().take(12).collect();
    let mut seen_letter = false;
    for ch in tail.chars() {
        if ch.is_ascii_alphabetic() {
            seen_letter = true;
        } else if ch == '\\' {
            return seen_letter;
        } else {
            return false;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Fraction bars
// ---------------------------------------------------------------------------

/// Find the first horizontal bar with spans strictly above and below.
/// Returns (numerator ids, denominator ids, bar x0).
fn split_fraction(
    spans: &[TextSpan],
    order: &[usize],
    vectors: &[VectorSegment],
) -> Option<(Vec<usize>, Vec<usize>, f32)> {
    for bar in vectors {
        let dy = (bar.y1 - bar.y0).abs();
        let len = (bar.x1 - bar.x0).abs();
        if dy >= 1.5 || len <= 4.0 {
            continue;
        }
        let bar_y = (bar.y0 + bar.y1) * 0.5;
        let (bx0, bx1) = (bar.x0.min(bar.x1), bar.x0.max(bar.x1));
        let mut num = Vec::new();
        let mut den = Vec::new();
        for &i in order {
            let cy = (spans[i].bbox.y0 + spans[i].bbox.y1) * 0.5;
            let overlaps = spans[i].bbox.x0 < bx1 && spans[i].bbox.x1 > bx0;
            if !overlaps {
                continue;
            }
            if cy < bar_y - 1.0 {
                num.push(i);
            } else if cy > bar_y + 1.0 {
                den.push(i);
            }
        }
        if !num.is_empty() && !den.is_empty() {
            return Some((num, den, bx0));
        }
    }
    None
}

/// Split rendered inline latex after its `pos`-th top-level token so a
/// `\frac` splices at the bar's x position.
fn split_tokens_at(latex: &str, pos: usize) -> (String, String) {
    if pos == 0 {
        return (String::new(), latex.to_string());
    }
    let mut depth = 0i32;
    let mut seen = 0usize;
    let mut cut = latex.len();
    for (idx, ch) in latex.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            ' ' if depth == 0 => {
                seen += 1;
                if seen == pos {
                    cut = idx + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    (latex[..cut].to_string(), latex[cut..].to_string())
}

// ---------------------------------------------------------------------------
// Glyph mapping
// ---------------------------------------------------------------------------

/// Map one math/operator char to its LaTeX command.
fn latex_for_char(ch: char) -> Option<&'static str> {
    Some(match ch {
        'α' => "\\alpha",
        'β' => "\\beta",
        'γ' => "\\gamma",
        'δ' => "\\delta",
        'ε' | 'ϵ' => "\\epsilon",
        'θ' => "\\theta",
        'λ' => "\\lambda",
        'μ' => "\\mu",
        'π' => "\\pi",
        'σ' => "\\sigma",
        'φ' => "\\phi",
        'ω' => "\\omega",
        'Δ' => "\\Delta",
        'Π' => "\\Pi",
        'Σ' => "\\Sigma",
        'Ω' => "\\Omega",
        '∑' => "\\sum",
        '∏' => "\\prod",
        '∫' => "\\int",
        '∞' => "\\infty",
        '≤' => "\\leq",
        '≥' => "\\geq",
        '≠' => "\\neq",
        '≈' => "\\approx",
        '×' => "\\times",
        '÷' => "\\div",
        '±' => "\\pm",
        '∓' => "\\mp",
        '⋅' => "\\cdot",
        '∘' => "\\circ",
        '∂' => "\\partial",
        '∇' => "\\nabla",
        '∈' => "\\in",
        '∋' => "\\ni",
        '∀' => "\\forall",
        '∃' => "\\exists",
        '∅' => "\\emptyset",
        '∧' => "\\land",
        '∨' => "\\lor",
        '¬' => "\\neg",
        '→' => "\\to",
        '←' => "\\leftarrow",
        '⇒' => "\\Rightarrow",
        '⇔' => "\\Leftrightarrow",
        '√' => "\\sqrt",
        '⊥' => "\\perp",
        '∥' => "\\parallel",
        '°' => "^\\circ",
        '′' => "'",
        'ℓ' => "\\ell",
        'ℝ' => "\\mathbb{R}",
        _ => return None,
    })
}

/// Map span text to LaTeX, reporting whether a math symbol mapped.
fn map_span_text(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut symbol = false;
    let mut prev_space = false;
    for ch in text.chars() {
        if let Some(cmd) = latex_for_char(ch) {
            out.push_str(cmd);
            symbol = true;
            prev_space = false;
        } else if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else if matches!(ch, '_' | '%' | '&' | '$' | '#' | '{' | '}') {
            out.push('\\');
            out.push(ch);
            prev_space = false;
        } else if ch == '^' {
            out.push_str("\\^{}");
            prev_space = false;
        } else if ch == '~' {
            out.push_str("\\~{}");
            prev_space = false;
        } else if ch == '\\' {
            out.push_str("\\textbackslash{}");
            prev_space = false;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    (out, symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BBox;

    fn span(text: &str, font: &str, size: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            bbox: BBox::new(x0, y0, x1, y1),
            font_name: font.to_string(),
            font_size: size,
            is_bold: false,
            is_italic: false,
            is_monospace: false,
        }
    }

    #[test]
    fn reconstructs_sub_superscript_and_greek() {
        // x (base 11pt, center 105.5) + i (7pt, center 107.5: sub) +
        // 2 (7pt, center 100.5: sup) + symbols.
        let spans = vec![
            span("x", "CMMI10", 11.0, 72.0, 100.0, 80.0, 111.0),
            span("i", "CMMI7", 7.0, 80.0, 104.0, 84.0, 111.0),
            span("2", "CMR7", 7.0, 80.0, 97.0, 84.0, 104.0),
            span("+", "CMR10", 11.0, 88.0, 100.0, 94.0, 111.0),
            span("α", "Symbol", 11.0, 98.0, 100.0, 106.0, 111.0),
            span("≥", "CMSY10", 11.0, 110.0, 100.0, 118.0, 111.0),
            span("0", "CMR10", 11.0, 122.0, 100.0, 130.0, 111.0),
        ];
        let (latex, conf) = reconstruct_latex_from_spans(&spans, &[]).expect("reconstructs");
        assert_eq!(latex, "x_{i}^{2} + \\alpha \\geq 0");
        assert!(conf >= 0.80, "conf: {conf}");
    }

    #[test]
    fn reconstructs_fraction_bar() {
        let spans = vec![
            span("a + b", "CMR10", 11.0, 72.0, 90.0, 110.0, 100.0),
            span("c", "CMR10", 11.0, 72.0, 110.0, 80.0, 120.0),
        ];
        let bar = VectorSegment {
            x0: 70.0,
            y0: 105.0,
            x1: 112.0,
            y1: 105.4,
            stroke_width: 0.8,
        };
        let (latex, conf) = reconstruct_latex_from_spans(&spans, &[bar]).expect("reconstructs");
        assert_eq!(latex, "\\frac{a + b}{c}");
        assert!(conf >= 0.80, "conf: {conf}");
    }

    #[test]
    fn plain_text_returns_none() {
        let spans = vec![span(
            "Hello world",
            "Helvetica",
            11.0,
            72.0,
            100.0,
            150.0,
            111.0,
        )];
        assert!(reconstruct_latex_from_spans(&spans, &[]).is_none());
    }

    #[test]
    fn lone_math_glyph_scores_below_threshold() {
        let spans = vec![span("x", "CMMI10", 11.0, 72.0, 100.0, 80.0, 111.0)];
        let (_, conf) = reconstruct_latex_from_spans(&spans, &[]).expect("some");
        assert!(conf < MATH_CONFIDENCE_THRESHOLD, "conf: {conf}");
    }

    #[test]
    fn rewrite_wraps_standalone_and_inline() {
        let math_line = TextLine {
            spans: vec![
                span("x", "CMMI10", 11.0, 72.0, 100.0, 80.0, 111.0),
                span("i", "CMMI7", 7.0, 80.0, 104.0, 84.0, 111.0),
                span("2", "CMMI7", 7.0, 80.0, 97.0, 84.0, 104.0),
                span("+", "CMMI10", 11.0, 88.0, 100.0, 94.0, 111.0),
                span("α", "Symbol", 11.0, 98.0, 100.0, 106.0, 111.0),
            ],
            bbox: BBox::new(72.0, 97.0, 106.0, 111.0),
            text: "placeholder".to_string(),
            font_size: 11.0,
            is_bold: false,
            is_monospace: false,
        };
        let plain_line = TextLine {
            spans: vec![span(
                "See Figure 3 for the layout.",
                "Helvetica",
                11.0,
                72.0,
                200.0,
                300.0,
                211.0,
            )],
            bbox: BBox::new(72.0, 200.0, 300.0, 211.0),
            text: "See Figure 3 for the layout.".to_string(),
            font_size: 11.0,
            is_bold: false,
            is_monospace: false,
        };
        let (lines, boxes) = rewrite_math_lines(vec![math_line, plain_line], &[]);
        assert_eq!(lines[0].text, "$$x_{i}^{2} + \\alpha$$");
        assert_eq!(lines[1].text, "See Figure 3 for the layout.");
        assert_eq!(boxes.len(), 1);
    }

    #[test]
    fn mixed_line_wraps_only_math_runs() {
        let line = TextLine {
            spans: vec![
                span("The value", "Helvetica", 11.0, 72.0, 100.0, 140.0, 111.0),
                span("x", "CMMI10", 11.0, 145.0, 100.0, 152.0, 111.0),
                span("i", "CMMI7", 7.0, 152.0, 104.0, 156.0, 111.0),
                span("grows", "Helvetica", 11.0, 175.0, 100.0, 210.0, 111.0),
            ],
            bbox: BBox::new(72.0, 100.0, 210.0, 111.0),
            text: "The value xi grows".to_string(),
            font_size: 11.0,
            is_bold: false,
            is_monospace: false,
        };
        let (lines, boxes) = rewrite_math_lines(vec![line], &[]);
        assert_eq!(lines[0].text, "The value $x_{i}$ grows");
        assert_eq!(boxes.len(), 1);
        // Tight run bbox, not the whole line.
        assert!((boxes[0].x0 - 145.0).abs() < 1e-6);
        assert!((boxes[0].x1 - 156.0).abs() < 1e-6);
    }

    #[test]
    fn weak_math_run_keeps_original_text() {
        // Lone CMMI glyph scores 0.72 < 0.80: no rewrite at all.
        let line = TextLine {
            spans: vec![
                span("See", "Helvetica", 11.0, 72.0, 100.0, 95.0, 111.0),
                span("x", "CMMI10", 11.0, 100.0, 100.0, 107.0, 111.0),
            ],
            bbox: BBox::new(72.0, 100.0, 107.0, 111.0),
            text: "See x".to_string(),
            font_size: 11.0,
            is_bold: false,
            is_monospace: false,
        };
        let (lines, boxes) = rewrite_math_lines(vec![line], &[]);
        assert_eq!(lines[0].text, "See x");
        assert!(boxes.is_empty());
    }
}
