//! Smart triage & sub-page complexity scoring.
//!
//! Implements `S(R) = min(1, 0.40*ρ_overlap + 0.35*ρ_tab_entropy + 0.25*ρ_vector_noise)`
//! over candidate regions, plus structural rules for scanned pages and
//! vector diagrams.
//!
//! Two deliberate deviations from a literal reading of the spec, both
//! required for the `0.45` threshold to function:
//!
//! 1. **ρ_overlap is a span-involvement rate** (spans touching ≥1 overlap
//!    above `0.25`, divided by spans in R), not a pair fraction. A pair
//!    fraction dilutes quadratically: one double-struck pair among 20
//!    spans scores `1/190 ≈ 0.005`, which would deaden the `0.40` weight
//!    for every realistic page. Involvement preserves the intent (broken
//!    kerning, watermarks over tables, stacked layers).
//! 2. **Single-signal regions combine by proximity.** The weights cap any
//!    lone signal at `0.40`, below the `0.45` threshold, so co-located
//!    mess signals (overlap clusters + entropy runs within a few line
//!    heights) merge into one candidate region before scoring.
//!
//! Vector-diagram clusters trip structurally (a pure chart has no text to
//! score) and scanned pages trip by the spec's explicit coverage rule.

use std::collections::{BTreeMap, BTreeSet};

use rstar::{RTree, RTreeObject, AABB};

use crate::config::ParserConfig;
use crate::geometry::lines::{body_font_size, merge_spans_to_lines, TextLine};
use crate::models::{BBox, RawPageElements, TextSpan, TriageReason, VectorSegment};

/// Overlap predicate for ρ_overlap involvement.
pub const OVERLAP_PAIR_THRESHOLD: f32 = 0.25;
/// Minimum consecutive multi-island rows for a table-entropy run.
pub const MIN_ENTROPY_ROWS: usize = 4;
/// Minimum islands per row for table-entropy candidacy.
pub const MIN_ISLANDS_PER_ROW: usize = 3;
/// Island separator: gap multiple over the row font size.
pub const ISLAND_GAP_MULT: f32 = 3.0;
/// Column-misalignment tolerance, as a multiple of median font size.
pub const ALIGN_TOL_FONT_MULT: f32 = 0.75;
/// Signal-merge distance, as a multiple of median font size.
pub const SIGNAL_MERGE_FONT_MULT: f32 = 3.0;
/// Minimum diagonal segments for a structural diagram region.
pub const MIN_DIAGRAM_SEGMENTS: usize = 4;
/// Scanned-page text cutoff (characters).
pub const SCANNED_MAX_CHARS: usize = 30;
/// Scanned-page image coverage cutoff (fraction of page area).
pub const SCANNED_MIN_COVERAGE: f32 = 0.5;

/// A sub-page region routed to VLM fallback.
#[derive(Debug, Clone)]
pub struct FallbackRegion {
    /// Tight union of the triggering signals (citation bbox).
    pub content_bbox: BBox,
    /// Padded + page-clamped bbox (crop bbox).
    pub bbox: BBox,
    /// Dominant trigger reason.
    pub reason: TriageReason,
    /// Complexity `S(R)` of the content bbox.
    pub score: f32,
}

/// Whole-page triage verdict.
#[derive(Debug, Clone)]
pub struct PageTriage {
    /// Page-level complexity `S` (same formula over the whole page).
    pub score: f32,
    /// True when the scanned-page rule fired.
    pub is_scanned: bool,
    /// Sub-page regions routed to VLM (empty ⇒ pure fast path).
    ///
    /// Note: a page can carry regions while `score < threshold` when a
    /// structural rule (diagram cluster) fired. The fast-path verdict is
    /// `fallback_regions.is_empty()`, not the score comparison.
    pub fallback_regions: Vec<FallbackRegion>,
}

/// Score a page and extract VLM fallback regions.
#[must_use]
pub fn score_page(page: &RawPageElements, config: &ParserConfig) -> PageTriage {
    let lines = merge_spans_to_lines(&page.spans);
    score_page_with_lines(page, &lines, config)
}

/// Score a page with pre-merged lines (saves a merge when the caller
/// already merged for table detection).
#[must_use]
pub fn score_page_with_lines(
    page: &RawPageElements,
    lines: &[TextLine],
    config: &ParserConfig,
) -> PageTriage {
    // Structural rule 1: scanned page.
    if is_scanned_page(page) {
        let content = BBox::new(0.0, 0.0, page.width, page.height);
        return PageTriage {
            score: 1.0,
            is_scanned: true,
            fallback_regions: vec![FallbackRegion {
                content_bbox: content,
                bbox: content,
                reason: TriageReason::ScannedImageRegion,
                score: 1.0,
            }],
        };
    }

    let med_font = body_font_size(lines).max(1.0);
    let involved = overlap_involvement(&page.spans);
    let runs = entropy_runs(lines, med_font);
    let diagrams = diagram_clusters(&page.vectors);

    // Page-level S(R) with the same formula.
    let page_score = complexity_score(
        involvement_rate(&involved, None),
        f32::from(!runs.is_empty()),
        vector_noise_ratio(&page.vectors, &page.spans, None),
    );

    // Candidate signals → proximity-merged regions → thresholded.
    let mut signals: Vec<Signal> = Vec::new();
    for run in &runs {
        signals.push(Signal {
            bbox: run.bbox,
            has_entropy: true,
            has_overlap: false,
            has_diagram: false,
        });
    }
    for cluster in overlap_clusters(&page.spans, &involved) {
        signals.push(Signal {
            bbox: cluster,
            has_entropy: false,
            has_overlap: true,
            has_diagram: false,
        });
    }
    for bbox in diagrams {
        signals.push(Signal {
            bbox,
            has_entropy: false,
            has_overlap: false,
            has_diagram: true,
        });
    }
    let merged = merge_signals(signals, med_font * SIGNAL_MERGE_FONT_MULT);
    let mut regions: Vec<FallbackRegion> = merged
        .into_iter()
        .filter_map(|m| {
            let in_region: Vec<usize> = page
                .spans
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    let (cx, cy) = s.bbox.centroid();
                    m.bbox.contains_point(cx, cy)
                })
                .map(|(i, _)| i)
                .collect();
            let rho_o = involvement_rate(&involved, Some(&in_region));
            let rho_t = f32::from(m.has_entropy);
            let rho_v = vector_noise_ratio(&page.vectors, &page.spans, Some(&m.bbox));
            let score = complexity_score(rho_o, rho_t, rho_v);
            // Diagrams trip structurally (a pure chart has no text to score).
            if score < config.triage_threshold && !m.has_diagram {
                return None;
            }
            let reason = if m.has_entropy {
                TriageReason::BorderlessIrregularTable
            } else if m.has_overlap {
                TriageReason::OverlappingSpans
            } else {
                TriageReason::ComplexMathOrDiagram
            };
            Some(FallbackRegion {
                content_bbox: m.bbox,
                bbox: m
                    .bbox
                    .pad_clamped(config.vlm_region_pad_pt, page.width, page.height),
                reason,
                score,
            })
        })
        .collect();

    regions.sort_by(|a, b| {
        a.content_bbox
            .y0
            .partial_cmp(&b.content_bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    PageTriage {
        score: page_score,
        is_scanned: false,
        fallback_regions: regions,
    }
}

/// `S(R) = min(1, 0.40*ρo + 0.35*ρt + 0.25*ρv)`.
#[must_use]
pub fn complexity_score(rho_overlap: f32, rho_tab: f32, rho_vec: f32) -> f32 {
    (0.40 * rho_overlap + 0.35 * rho_tab + 0.25 * rho_vec).min(1.0)
}

/// Scanned-page rule: `< 30` chars and images covering `> 50%` of the page.
#[must_use]
pub fn is_scanned_page(page: &RawPageElements) -> bool {
    let chars: usize = page.spans.iter().map(|s| s.text.chars().count()).sum();
    if chars >= SCANNED_MAX_CHARS {
        return false;
    }
    let page_area = (page.width * page.height).max(1.0);
    let img_area: f32 = page.image_bboxes.iter().map(BBox::area).sum();
    img_area / page_area > SCANNED_MIN_COVERAGE
}

/// Per-span overlap involvement: `true` when the span overlaps another span
/// above [`OVERLAP_PAIR_THRESHOLD`]. Candidate pairs come from an R-tree so
/// realistic pages stay far below quadratic work.
#[must_use]
pub fn overlap_involvement(spans: &[TextSpan]) -> Vec<bool> {
    let mut involved = vec![false; spans.len()];
    if spans.len() < 2 {
        return involved;
    }
    let tree = RTree::bulk_load(
        spans
            .iter()
            .enumerate()
            .map(|(i, s)| Env {
                id: i,
                env: envelope_of(s.bbox.x0, s.bbox.y0, s.bbox.x1, s.bbox.y1, 0.0),
            })
            .collect::<Vec<_>>(),
    );
    for (i, span) in spans.iter().enumerate() {
        if involved[i] {
            continue;
        }
        let query = envelope_of(span.bbox.x0, span.bbox.y0, span.bbox.x1, span.bbox.y1, 0.0);
        for cand in tree.locate_in_envelope_intersecting(&query) {
            let j = cand.id;
            if j != i && span.bbox.overlap(&spans[j].bbox) > OVERLAP_PAIR_THRESHOLD {
                involved[i] = true;
                involved[j] = true;
            }
        }
    }
    involved
}

/// Involvement rate over an optional span subset (`None` ⇒ all spans).
fn involvement_rate(involved: &[bool], subset: Option<&[usize]>) -> f32 {
    match subset {
        None => {
            if involved.is_empty() {
                return 0.0;
            }
            involved.iter().filter(|v| **v).count() as f32 / involved.len() as f32
        }
        Some(idx) => {
            if idx.is_empty() {
                return 0.0;
            }
            idx.iter().filter(|i| involved[**i]).count() as f32 / idx.len() as f32
        }
    }
}

/// Connected components (union-find) of overlapping spans, as bboxes.
fn overlap_clusters(spans: &[TextSpan], involved: &[bool]) -> Vec<BBox> {
    let ids: Vec<usize> = involved
        .iter()
        .enumerate()
        .filter(|(_, v)| **v)
        .map(|(i, _)| i)
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }
    let mut dsu = Dsu::new(ids.len());
    let tree = RTree::bulk_load(
        ids.iter()
            .enumerate()
            .map(|(k, &i)| Env {
                id: k,
                env: envelope_of(
                    spans[i].bbox.x0,
                    spans[i].bbox.y0,
                    spans[i].bbox.x1,
                    spans[i].bbox.y1,
                    0.0,
                ),
            })
            .collect::<Vec<_>>(),
    );
    for (k, &i) in ids.iter().enumerate() {
        let query = envelope_of(
            spans[i].bbox.x0,
            spans[i].bbox.y0,
            spans[i].bbox.x1,
            spans[i].bbox.y1,
            0.0,
        );
        for cand in tree.locate_in_envelope_intersecting(&query) {
            let j = ids[cand.id];
            if j != i && spans[i].bbox.overlap(&spans[j].bbox) > OVERLAP_PAIR_THRESHOLD {
                dsu.union(k, cand.id);
            }
        }
    }
    let mut comp: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, &i) in ids.iter().enumerate() {
        comp.entry(dsu.find(k)).or_default().push(i);
    }
    comp.into_values()
        .filter_map(|members| BBox::union_all(members.iter().map(|&i| spans[i].bbox)))
        .collect()
}

/// A misaligned multi-island row run (borderless-table candidate).
#[derive(Debug, Clone)]
pub struct EntropyRun {
    /// Union bbox of the run's rows.
    pub bbox: BBox,
    /// Number of rows in the run.
    pub row_count: usize,
    /// Islands per row.
    pub islands_per_row: usize,
}

/// Find misaligned borderless-table runs: ≥4 consecutive rows of ≥3 islands
/// (gaps `> 3×font`) whose columns fail to bucket (jagged counts or column
/// `x0` stdev above tolerance).
#[must_use]
pub fn entropy_runs(lines: &[TextLine], median_font: f32) -> Vec<EntropyRun> {
    let rows = group_rows(lines);
    let islands: Vec<Vec<BBox>> = rows
        .iter()
        .map(|row| {
            row_islands(
                &row.iter().map(|&i| &lines[i]).collect::<Vec<_>>(),
                median_font,
            )
        })
        .collect();

    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    let flush = |runs: &mut Vec<EntropyRun>, s: usize, e: usize| {
        if e - s >= MIN_ENTROPY_ROWS && misaligned(&islands[s..e], median_font) {
            let bbox = BBox::union_all(
                rows[s..e]
                    .iter()
                    .flat_map(|row| row.iter().map(|&i| lines[i].bbox)),
            );
            if let Some(bbox) = bbox {
                runs.push(EntropyRun {
                    bbox,
                    row_count: e - s,
                    islands_per_row: islands[s].len(),
                });
            }
        }
    };
    for (r, isl) in islands.iter().enumerate() {
        if isl.len() >= MIN_ISLANDS_PER_ROW {
            if start.is_none() {
                start = Some(r);
            }
        } else if let Some(s) = start.take() {
            flush(&mut runs, s, r);
        }
    }
    if let Some(s) = start.take() {
        flush(&mut runs, s, islands.len());
    }
    runs
}

/// Group line indices into rows by vertical overlap (lines pre-sorted by y0).
fn group_rows(lines: &[TextLine]) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let same_row = rows.last().is_some_and(|row: &Vec<usize>| {
            row.iter()
                .any(|&j| line.bbox.vertical_overlap_ratio(&lines[j].bbox) > 0.5)
        });
        if same_row {
            if let Some(row) = rows.last_mut() {
                row.push(i);
            }
        } else {
            rows.push(vec![i]);
        }
    }
    rows
}

/// Split a row of lines into x-islands on gaps `> 3×font`.
fn row_islands(row: &[&TextLine], median_font: f32) -> Vec<BBox> {
    if row.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&TextLine> = row.to_vec();
    sorted.sort_by(|a, b| {
        a.bbox
            .x0
            .partial_cmp(&b.bbox.x0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut islands = Vec::new();
    let mut cur = sorted[0].bbox;
    for line in &sorted[1..] {
        if cur.h_gap(&line.bbox) > ISLAND_GAP_MULT * median_font {
            islands.push(cur);
            cur = line.bbox;
        } else {
            cur = cur.union(&line.bbox);
        }
    }
    islands.push(cur);
    islands
}

/// True when a run's columns fail deterministic bucketing.
fn misaligned(run: &[Vec<BBox>], median_font: f32) -> bool {
    let first = run[0].len();
    if run.iter().any(|isl| isl.len() != first) {
        return true; // jagged island counts never bucket
    }
    let tol = ALIGN_TOL_FONT_MULT * median_font;
    let mut stdev_sum = 0.0f32;
    for j in 0..first {
        let xs: Vec<f32> = run.iter().map(|isl| isl[j].x0).collect();
        let mean = xs.iter().sum::<f32>() / xs.len() as f32;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / xs.len() as f32;
        stdev_sum += var.sqrt();
    }
    stdev_sum / first as f32 > tol
}

/// Diagonal vector clusters (flowchart/chart candidates), as bboxes.
#[must_use]
pub fn diagram_clusters(vectors: &[VectorSegment]) -> Vec<BBox> {
    let diag: Vec<usize> = vectors
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            let dx = (s.x1 - s.x0).abs();
            let dy = (s.y1 - s.y0).abs();
            dx >= 1.5 && dy >= 1.5 && dx.hypot(dy) > 5.0
        })
        .map(|(i, _)| i)
        .collect();
    if diag.len() < MIN_DIAGRAM_SEGMENTS {
        return Vec::new();
    }
    let tree = RTree::bulk_load(
        diag.iter()
            .enumerate()
            .map(|(k, &i)| Env {
                id: k,
                env: envelope_of(
                    vectors[i].x0,
                    vectors[i].y0,
                    vectors[i].x1,
                    vectors[i].y1,
                    4.0,
                ),
            })
            .collect::<Vec<_>>(),
    );
    let mut dsu = Dsu::new(diag.len());
    for (k, &i) in diag.iter().enumerate() {
        let query = envelope_of(
            vectors[i].x0,
            vectors[i].y0,
            vectors[i].x1,
            vectors[i].y1,
            0.0,
        );
        for cand in tree.locate_in_envelope_intersecting(&query) {
            dsu.union(k, cand.id);
        }
    }
    let mut comp: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, &i) in diag.iter().enumerate() {
        comp.entry(dsu.find(k)).or_default().push(i);
    }
    comp.into_values()
        .filter(|members| members.len() >= MIN_DIAGRAM_SEGMENTS)
        .filter_map(|members| {
            BBox::union_all(
                members.iter().map(|&i| {
                    BBox::new(vectors[i].x0, vectors[i].y0, vectors[i].x1, vectors[i].y1)
                }),
            )
        })
        .collect()
}

/// ρ_vector_noise: diagonal segments over text spans (`None` ⇒ whole page).
fn vector_noise_ratio(vectors: &[VectorSegment], spans: &[TextSpan], region: Option<&BBox>) -> f32 {
    let in_region = |x: f32, y: f32| region.is_none_or(|r| r.contains_point(x, y));
    let diagonals = vectors
        .iter()
        .filter(|s| {
            let dx = (s.x1 - s.x0).abs();
            let dy = (s.y1 - s.y0).abs();
            dx >= 1.5 && dy >= 1.5 && in_region((s.x0 + s.x1) * 0.5, (s.y0 + s.y1) * 0.5)
        })
        .count();
    if diagonals == 0 {
        return 0.0;
    }
    let texts = spans
        .iter()
        .filter(|s| {
            let (cx, cy) = s.bbox.centroid();
            in_region(cx, cy)
        })
        .count();
    if texts == 0 {
        return 1.0;
    }
    (diagonals as f32 / texts as f32).min(1.0)
}

/// A mess signal awaiting proximity merge.
#[derive(Debug, Clone)]
struct Signal {
    bbox: BBox,
    has_entropy: bool,
    has_overlap: bool,
    has_diagram: bool,
}

/// Greedily merge signals with vertical gap below `merge_dist` and nonzero
/// horizontal overlap; repeats to a fixpoint (a merge can bridge a third).
fn merge_signals(signals: Vec<Signal>, merge_dist: f32) -> Vec<Signal> {
    let mut seen = BTreeSet::new();
    let mut current = signals;
    loop {
        let key: Vec<[i32; 4]> = current
            .iter()
            .map(|m| {
                [
                    (m.bbox.x0 * 10.0) as i32,
                    (m.bbox.y0 * 10.0) as i32,
                    (m.bbox.x1 * 10.0) as i32,
                    (m.bbox.y1 * 10.0) as i32,
                ]
            })
            .collect();
        if !seen.insert(key) || current.len() < 2 {
            return current;
        }
        current = merge_pass(current, merge_dist);
    }
}

/// Single greedy merge pass (fixpoint driver above calls it repeatedly).
fn merge_pass(signals: Vec<Signal>, merge_dist: f32) -> Vec<Signal> {
    let mut sorted = signals;
    sorted.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut merged: Vec<Signal> = Vec::new();
    for sig in sorted {
        let mut placed = false;
        for m in merged.iter_mut() {
            let h_overlap = (m.bbox.x1.min(sig.bbox.x1) - m.bbox.x0.max(sig.bbox.x0)) > 0.0;
            if h_overlap && m.bbox.v_gap(&sig.bbox) < merge_dist {
                m.bbox = m.bbox.union(&sig.bbox);
                m.has_entropy |= sig.has_entropy;
                m.has_overlap |= sig.has_overlap;
                m.has_diagram |= sig.has_diagram;
                placed = true;
                break;
            }
        }
        if !placed {
            merged.push(sig);
        }
    }
    merged
}

// ---------------------------------------------------------------------------
// Small spatial helpers (rstar envelope + union-find)
// ---------------------------------------------------------------------------

struct Env {
    id: usize,
    env: AABB<[f64; 2]>,
}

impl RTreeObject for Env {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        self.env
    }
}

fn envelope_of(x0: f32, y0: f32, x1: f32, y1: f32, pad: f32) -> AABB<[f64; 2]> {
    AABB::from_corners(
        [f64::from(x0.min(x1) - pad), f64::from(y0.min(y1) - pad)],
        [f64::from(x0.max(x1) + pad), f64::from(y0.max(y1) + pad)],
    )
}

struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            bbox: BBox::new(x0, y0, x1, y1),
            font_name: "Helvetica".to_string(),
            font_size: 11.0,
            is_bold: false,
            is_italic: false,
            is_monospace: false,
        }
    }

    fn page(spans: Vec<TextSpan>, vectors: Vec<VectorSegment>) -> RawPageElements {
        RawPageElements {
            page_index: 0,
            width: 612.0,
            height: 792.0,
            spans,
            vectors,
            image_bboxes: Vec::new(),
            filled_rects: Vec::new(),
            embedded_images: Vec::new(),
        }
    }

    #[test]
    fn formula_matches_spec_weights() {
        assert!((complexity_score(1.0, 1.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((complexity_score(1.0, 1.0, 0.0) - 0.75).abs() < 1e-6);
        assert!((complexity_score(0.0, 0.0, 0.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn involvement_flags_stacked_pair_only() {
        let spans = vec![
            span("a", 72.0, 100.0, 120.0, 111.0),
            span("a", 73.0, 100.0, 121.0, 111.0),
            span("far", 400.0, 500.0, 440.0, 511.0),
        ];
        assert_eq!(overlap_involvement(&spans), vec![true, true, false]);
    }

    #[test]
    fn disjoint_spans_have_zero_involvement() {
        let spans = vec![
            span("a", 72.0, 100.0, 120.0, 111.0),
            span("b", 200.0, 100.0, 250.0, 111.0),
        ];
        assert_eq!(overlap_involvement(&spans), vec![false, false]);
    }

    #[test]
    fn aligned_borderless_grid_is_not_entropy() {
        // 4 rows x 3 islands, perfectly column-aligned.
        let mut spans = Vec::new();
        for r in 0..4 {
            for (c, x) in [72.0, 250.0, 420.0].iter().enumerate() {
                spans.push(span(
                    &format!("r{r}c{c}"),
                    *x,
                    100.0 + r as f32 * 16.0,
                    *x + 30.0,
                    111.0 + r as f32 * 16.0,
                ));
            }
        }
        let lines = merge_spans_to_lines(&spans);
        assert!(entropy_runs(&lines, 11.0).is_empty());
    }

    #[test]
    fn jittered_islands_form_entropy_run() {
        let jitter = [0.0, 14.0, -10.0, 8.0, -16.0];
        let mut spans = Vec::new();
        for (r, j) in jitter.iter().enumerate() {
            for x in [72.0 + j, 250.0 - j, 420.0 + j] {
                spans.push(span(
                    "cell",
                    x,
                    100.0 + r as f32 * 16.0,
                    x + 25.0,
                    111.0 + r as f32 * 16.0,
                ));
            }
        }
        let lines = merge_spans_to_lines(&spans);
        let runs = entropy_runs(&lines, 11.0);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].row_count, 5);
        assert_eq!(runs[0].islands_per_row, 3);
    }

    #[test]
    fn two_island_rows_are_not_runs() {
        // Two-column lookalike: 6 rows x 2 islands.
        let mut spans = Vec::new();
        for r in 0..6 {
            for x in [72.0, 330.0] {
                spans.push(span(
                    "col",
                    x,
                    100.0 + r as f32 * 14.0,
                    x + 100.0,
                    110.0 + r as f32 * 14.0,
                ));
            }
        }
        let lines = merge_spans_to_lines(&spans);
        assert!(entropy_runs(&lines, 11.0).is_empty());
    }

    #[test]
    fn diagram_cluster_needs_four_diagonals() {
        let diag = |x0: f32, y0: f32| VectorSegment {
            x0,
            y0,
            x1: x0 + 40.0,
            y1: y0 + 30.0,
            stroke_width: 1.0,
        };
        let three: Vec<VectorSegment> = (0..3)
            .map(|i| diag(100.0 + i as f32 * 5.0, 100.0))
            .collect();
        assert!(diagram_clusters(&three).is_empty());
        let five: Vec<VectorSegment> = (0..5)
            .map(|i| diag(100.0 + i as f32 * 5.0, 100.0))
            .collect();
        assert_eq!(diagram_clusters(&five).len(), 1);
    }

    #[test]
    fn nearby_diagram_clusters_merge() {
        let cluster = |oy: f32| {
            (0..4)
                .map(|i| VectorSegment {
                    x0: 100.0 + i as f32 * 5.0,
                    y0: oy,
                    x1: 100.0 + i as f32 * 5.0 + 40.0,
                    y1: oy + 30.0,
                    stroke_width: 1.0,
                })
                .collect::<Vec<_>>()
        };
        // Anchor text sets a body-size merge distance (~33pt).
        let anchor = || span("body text here", 72.0, 500.0, 300.0, 511.0);
        let mut near = cluster(100.0);
        near.extend(cluster(140.0)); // 10pt vertical gap
        let verdict = score_page(&page(vec![anchor()], near), &ParserConfig::default());
        assert_eq!(verdict.fallback_regions.len(), 1);
        assert!(matches!(
            verdict.fallback_regions[0].reason,
            TriageReason::ComplexMathOrDiagram
        ));
        let mut far = cluster(100.0);
        far.extend(cluster(400.0));
        let verdict = score_page(&page(vec![anchor()], far), &ParserConfig::default());
        assert_eq!(verdict.fallback_regions.len(), 2);
    }

    #[test]
    fn scanned_rule_requires_chars_and_coverage() {
        let mut p = page(vec![span("hi", 0.0, 0.0, 10.0, 10.0)], vec![]);
        assert!(!is_scanned_page(&p)); // no image coverage
        p.image_bboxes.push(BBox::new(0.0, 0.0, 612.0, 792.0));
        assert!(is_scanned_page(&p));
        p.spans.push(span(&"x".repeat(40), 0.0, 20.0, 100.0, 30.0));
        assert!(!is_scanned_page(&p)); // too much text
    }
}
