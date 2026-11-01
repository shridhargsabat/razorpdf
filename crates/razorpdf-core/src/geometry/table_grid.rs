//! Deterministic vector table reconstruction.
//!
//! Axis-aligned vector segments are clustered into grid components with an
//! R-tree; closed grids snap text-span centroids into Markdown tables with
//! zero model calls.

use crate::geometry::lines::{TextLine, MERGE_MIN_CONTAINMENT};
use crate::models::{BBox, TextSpan, VectorSegment};
use rstar::{RTree, RTreeObject, AABB};

/// Max deviation from axis alignment (points).
const AXIS_TOL: f32 = 1.5;
/// Minimum horizontal segment length (points).
const MIN_H_LEN: f32 = 15.0;
/// Minimum vertical segment length (points).
const MIN_V_LEN: f32 = 10.0;
/// Intersection / coordinate-merge tolerance (points).
const MERGE_TOL: f32 = 2.0;
/// Minimum lines per grid component.
const MIN_H_LINES: usize = 3;
const MIN_V_LINES: usize = 2;
/// Minimum columns for a reportable table (kills 1-col figure rules).
const MIN_TABLE_COLS: usize = 2;
/// Minimum share of non-empty cells (kills empty figure-rule grids).
const MIN_CELL_FILL_RATIO: f32 = 0.30;
/// Maximum share of shaded cells (above this: heatmap figure, not a table).
const MAX_GRID_SHADE_RATIO: f32 = 0.6;

/// How a table was detected (all three are deterministic, zero VLM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSource {
    /// Closed vector grid (vertical + horizontal rules).
    VectorGrid,
    /// Horizontal rules delimiting aligned text rows (booktabs).
    Ruled,
    /// Aligned multi-island row runs, no rules at all.
    Borderless,
}

impl TableSource {
    /// Short lowercase label for logs and MCP payloads.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::VectorGrid => "grid",
            Self::Ruled => "ruled",
            Self::Borderless => "borderless",
        }
    }
}

/// A deterministically reconstructed table.
#[derive(Debug, Clone)]
pub struct DetectedTable {
    /// Union bbox of the grid.
    pub bbox: BBox,
    /// Cell texts, row-major. `rows[r][c]` is the text of cell (r, c).
    pub rows: Vec<Vec<String>>,
    /// Whether the first row is a header.
    pub has_header: bool,
    /// Which detector produced this table.
    pub source: TableSource,
}

impl DetectedTable {
    /// Number of body+header rows.
    #[must_use]
    pub fn n_rows(&self) -> usize {
        self.rows.len()
    }

    /// Number of columns (0 when empty).
    #[must_use]
    pub fn n_cols(&self) -> usize {
        self.rows.first().map_or(0, Vec::len)
    }
}

/// R-tree envelope wrapper (f64 for robust rstar support).
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

/// Tiny union-find for segment clustering.
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

/// Detect vector-grid tables, snapping `spans` into cells.
///
/// Grids whose cells are mostly shaded (`fills`) are figures (heatmaps),
/// not tables.
pub fn detect_vector_tables(
    vectors: &[VectorSegment],
    spans: &[TextSpan],
    fills: &[BBox],
) -> Vec<DetectedTable> {
    // 1. Classify axis-aligned segments.
    let mut h: Vec<usize> = Vec::new();
    let mut v: Vec<usize> = Vec::new();
    for (i, s) in vectors.iter().enumerate() {
        let dx = (s.x1 - s.x0).abs();
        let dy = (s.y1 - s.y0).abs();
        if dy < AXIS_TOL && dx > MIN_H_LEN {
            h.push(i);
        } else if dx < AXIS_TOL && dy > MIN_V_LEN {
            v.push(i);
        }
    }
    if h.is_empty() || v.is_empty() {
        return Vec::new();
    }

    // 2. Cluster intersecting H/V segments via R-tree + union-find.
    let v_tree = RTree::bulk_load(
        v.iter()
            .enumerate()
            .map(|(k, &i)| {
                let s = &vectors[i];
                Env {
                    id: k,
                    env: envelope_of(s.x0, s.y0, s.x1, s.y1, MERGE_TOL),
                }
            })
            .collect::<Vec<_>>(),
    );
    let mut dsu = Dsu::new(h.len() + v.len());
    for (hk, &hi) in h.iter().enumerate() {
        let hs = &vectors[hi];
        let query = envelope_of(hs.x0, hs.y0, hs.x1, hs.y1, 0.0);
        for cand in v_tree.locate_in_envelope_intersecting(&query) {
            let vs = &vectors[v[cand.id]];
            if segments_intersect(hs, vs) {
                dsu.union(hk, h.len() + cand.id);
            }
        }
    }

    // 3. Group components; keep grid-like clusters.
    let mut comp_h: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    let mut comp_v: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (hk, &hi) in h.iter().enumerate() {
        comp_h.entry(dsu.find(hk)).or_default().push(hi);
    }
    for (vk, &vi) in v.iter().enumerate() {
        comp_v.entry(dsu.find(h.len() + vk)).or_default().push(vi);
    }
    let mut roots: std::collections::BTreeSet<usize> = comp_h.keys().copied().collect();
    roots.extend(comp_v.keys().copied());

    // 4. R-tree over span envelopes for cell assignment.
    let span_tree = RTree::bulk_load(
        spans
            .iter()
            .enumerate()
            .map(|(i, s)| Env {
                id: i,
                env: envelope_of(s.bbox.x0, s.bbox.y0, s.bbox.x1, s.bbox.y1, 0.0),
            })
            .collect::<Vec<_>>(),
    );

    let mut tables = Vec::new();
    for root in roots {
        let hs = comp_h.get(&root).map_or(&[][..], Vec::as_slice);
        let vs = comp_v.get(&root).map_or(&[][..], Vec::as_slice);
        if hs.len() < MIN_H_LINES || vs.len() < MIN_V_LINES {
            continue;
        }
        if let Some(t) = build_table(vectors, hs, vs, spans, &span_tree, fills) {
            tables.push(t);
        }
    }

    tables.sort_by(|a, b| {
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
    tables
}

// ---------------------------------------------------------------------------
// Booktabs (rule-delimited) + borderless table detection
// ---------------------------------------------------------------------------

/// Minimum horizontal rule length for booktabs candidacy (points).
const MIN_RULE_LEN: f32 = 40.0;
/// Minimum x-overlap ratio between consecutive rules to pair them.
const MIN_RULE_OVERLAP: f32 = 0.5;
/// Minimum islands per row for borderless candidacy (3+ keeps two-column
/// prose safe: text columns never form 3-island aligned runs).
const MIN_BORDERLESS_ISLANDS: usize = 3;
/// Minimum islands for run membership. Two-island rows join provisionally
/// (bridge rows with merged cells); the final gates (≥3 columns + an
/// anchor row for borderless) reject two-column prose.
const MIN_RUN_ISLANDS: usize = 2;
/// Minimum consecutive rows per table run.
const MIN_TABLE_ROWS: usize = 2;
/// Column-bucket tolerance as a multiple of median font size.
const BUCKET_TOL_FONT_MULT: f32 = 0.75;
/// Max y-gap between consecutive rows of one run (× median line height).
const MAX_ROW_GAP_MULT: f32 = 3.0;
/// Island separator: gap multiple over the median font size.
const ISLAND_GAP_FONT_MULT: f32 = 2.0;
/// Script cutoff: lines at or below this fraction of body size are
/// super/subscript fragments, not table rows.
const SCRIPT_MAX_FONT_MULT: f32 = 0.75;
/// Column spread cap: every cluster must be tight (this × font) on at
/// least one edge, killing transitive chains across math/figure fragments.
const SPREAD_CAP_FONT_MULT: f32 = 2.0;

/// Detect booktabs-style tables: ≥2 long horizontal rules delimiting
/// ≥2 aligned multi-island text rows (no vertical rules needed).
pub fn detect_ruled_tables(
    vectors: &[VectorSegment],
    lines: &[TextLine],
    median_font: f32,
) -> Vec<DetectedTable> {
    if lines.is_empty() || median_font <= 0.0 {
        return Vec::new();
    }
    let mut rules: Vec<&VectorSegment> = vectors
        .iter()
        .filter(|s| {
            let dx = (s.x1 - s.x0).abs();
            let dy = (s.y1 - s.y0).abs();
            dy < AXIS_TOL && dx > MIN_RULE_LEN
        })
        .collect();
    if rules.len() < 2 {
        return Vec::new();
    }
    rules.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap_or(std::cmp::Ordering::Equal));
    // Cluster consecutive rules with shared x-span; each cluster needs ≥2.
    let mut tables = Vec::new();
    let mut start = 0;
    for i in 1..rules.len() {
        if rule_pair_overlap(rules[i - 1], rules[i]) < MIN_RULE_OVERLAP {
            tables.extend(ruled_cluster_to_tables(
                &rules[start..i],
                lines,
                median_font,
            ));
            start = i;
        }
    }
    tables.extend(ruled_cluster_to_tables(&rules[start..], lines, median_font));
    tables
}

/// X-overlap of two rules as a ratio of the shorter rule.
fn rule_pair_overlap(a: &VectorSegment, b: &VectorSegment) -> f32 {
    let (ax0, ax1) = (a.x0.min(a.x1), a.x0.max(a.x1));
    let (bx0, bx1) = (b.x0.min(b.x1), b.x0.max(b.x1));
    let overlap = (ax1.min(bx1) - ax0.max(bx0)).max(0.0);
    let shorter = (ax1 - ax0).min(bx1 - bx0).max(1.0);
    overlap / shorter
}

/// Turn one rule cluster (≥2 rules) into tables via aligned row runs.
fn ruled_cluster_to_tables(
    rules: &[&VectorSegment],
    lines: &[TextLine],
    median_font: f32,
) -> Vec<DetectedTable> {
    if rules.len() < 2 {
        return Vec::new();
    }
    let x0 = rules
        .iter()
        .map(|r| r.x0.min(r.x1))
        .fold(f32::INFINITY, f32::min);
    let x1 = rules
        .iter()
        .map(|r| r.x0.max(r.x1))
        .fold(f32::NEG_INFINITY, f32::max);
    let y_top = rules.first().map_or(0.0, |r| (r.y0 + r.y1) * 0.5);
    let y_bot = rules.last().map_or(0.0, |r| (r.y0 + r.y1) * 0.5);
    // Text lines strictly between the outer rules and inside the x-span.
    let inside: Vec<&TextLine> = lines
        .iter()
        .filter(|l| {
            let (cx, cy) = l.bbox.centroid();
            cy > y_top && cy < y_bot && cx >= x0 && cx <= x1
        })
        .collect();
    aligned_runs_to_tables(&inside, median_font, TableSource::Ruled)
}

/// Detect borderless tables: runs of ≥2 consecutive rows with ≥3 aligned
/// islands each. Two-column prose is excluded by the island count.
pub fn detect_borderless_tables(lines: &[TextLine], median_font: f32) -> Vec<DetectedTable> {
    if lines.is_empty() || median_font <= 0.0 {
        return Vec::new();
    }
    let refs: Vec<&TextLine> = lines.iter().collect();
    aligned_runs_to_tables(&refs, median_font, TableSource::Borderless)
}

/// All deterministic tables: vector grids, then ruled, then borderless.
/// Later detectors yield to earlier ones on >50% bbox overlap.
pub fn detect_all_tables(
    vectors: &[VectorSegment],
    spans: &[TextSpan],
    lines: &[TextLine],
    median_font: f32,
    fills: &[BBox],
) -> Vec<DetectedTable> {
    let mut tables = detect_vector_tables(vectors, spans, fills);
    for table in detect_ruled_tables(vectors, lines, median_font)
        .into_iter()
        .chain(detect_borderless_tables(lines, median_font))
    {
        if tables
            .iter()
            .all(|done| done.bbox.overlap(&table.bbox) < 0.5)
        {
            tables.push(table);
        }
    }
    tables.sort_by(|a, b| {
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
    tables
}

/// A table island: owned line texts + spans (for bridge splitting) +
/// script marks with x-positions (for split-aware reattachment).
#[derive(Debug, Clone)]
struct TableIsland {
    line_texts: Vec<String>,
    spans: Vec<TextSpan>,
    scripts: Vec<(f32, String)>,
    x0: f32,
    x1: f32,
    bbox: BBox,
    bold: bool,
}

impl TableIsland {
    fn from_lines(lines: &[&TextLine]) -> Self {
        let mut line_texts = Vec::new();
        let mut spans = Vec::new();
        let mut x0 = f32::INFINITY;
        let mut x1 = f32::NEG_INFINITY;
        let mut bbox: Option<BBox> = None;
        let mut bold = false;
        for line in lines {
            let text = line.text.trim();
            if !text.is_empty() {
                line_texts.push(text.to_string());
            }
            spans.extend(line.spans.iter().cloned());
            x0 = x0.min(line.bbox.x0);
            x1 = x1.max(line.bbox.x1);
            bbox = Some(bbox.map_or(line.bbox, |b| b.union(&line.bbox)));
            bold |= line.is_bold;
        }
        spans.sort_by(|a, b| {
            a.bbox
                .x0
                .partial_cmp(&b.bbox.x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self {
            line_texts,
            spans,
            scripts: Vec::new(),
            x0,
            x1,
            bbox: bbox.unwrap_or_else(BBox::zero),
            bold,
        }
    }

    /// Rebuild an island from a span run (bridge-split pieces).
    fn from_spans(spans: &[TextSpan], median_font: f32) -> Self {
        let mut line_texts = Vec::new();
        let mut current = String::new();
        let mut cursor = f32::NEG_INFINITY;
        let mut x0 = f32::INFINITY;
        let mut x1 = f32::NEG_INFINITY;
        let mut bbox: Option<BBox> = None;
        let mut bold = false;
        for span in spans {
            let gap = span.bbox.x0 - cursor;
            if !current.is_empty() && gap > 0.3 * median_font {
                line_texts.push(std::mem::take(&mut current));
            } else if !current.is_empty()
                && !current.ends_with(char::is_whitespace)
                && !span.text.starts_with(char::is_whitespace)
            {
                // Kerned fragments join directly (mirrors the line merger).
            } else if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(span.text.trim());
            cursor = span.bbox.x1;
            x0 = x0.min(span.bbox.x0);
            x1 = x1.max(span.bbox.x1);
            bbox = Some(bbox.map_or(span.bbox, |b| b.union(&span.bbox)));
            bold |= span.is_bold;
        }
        if !current.is_empty() {
            line_texts.push(current);
        }
        Self {
            line_texts,
            spans: spans.to_vec(),
            scripts: Vec::new(),
            x0,
            x1,
            bbox: bbox.unwrap_or_else(BBox::zero),
            bold,
        }
    }

    /// Cell text: line parts spaced, `^`/`_` script marks attached directly.
    fn text(&self) -> String {
        let mut out = self.line_texts.join(" ");
        for (_, mark) in &self.scripts {
            if !mark.starts_with(['^', '_']) && !out.is_empty() {
                out.push(' ');
            }
            out.push_str(mark);
        }
        out
    }

    /// Split spans at the largest inter-span gap strictly inside (vlo, vhi),
    /// falling back to the inner space of a valley-covering span nearest
    /// the valley center (proportional x mapping).
    fn split_at_valley(
        &self,
        vlo: f32,
        vhi: f32,
        median_font: f32,
    ) -> Option<(TableIsland, TableIsland)> {
        let mut cut: Option<(Vec<TextSpan>, Vec<TextSpan>, f32)> = None;
        if self.spans.len() >= 2 {
            let mut best: Option<(usize, f32)> = None;
            for i in 1..self.spans.len() {
                let gap = self.spans[i].bbox.x0 - self.spans[i - 1].bbox.x1;
                let mid = (self.spans[i].bbox.x0 + self.spans[i - 1].bbox.x1) * 0.5;
                if mid > vlo && mid < vhi && best.is_none_or(|(_, g)| gap > g) {
                    best = Some((i, gap));
                }
            }
            if let Some((at, _)) = best {
                let split_x = (self.spans[at - 1].bbox.x1 + self.spans[at].bbox.x0) * 0.5;
                cut = Some((
                    self.spans[..at].to_vec(),
                    self.spans[at..].to_vec(),
                    split_x,
                ));
            }
        }
        if cut.is_none() {
            let mid_v = (vlo + vhi) * 0.5;
            for (si, span) in self.spans.iter().enumerate() {
                if span.bbox.x0 < mid_v && span.bbox.x1 > mid_v {
                    let text = span.text.trim();
                    let n = text.chars().count();
                    if n >= 2 {
                        let w = span.bbox.x1 - span.bbox.x0;
                        let mut best_space: Option<(usize, f32)> = None;
                        for (i, ch) in text.chars().enumerate() {
                            if ch == ' ' {
                                let x = span.bbox.x0 + w * ((i as f32 + 0.5) / n as f32);
                                if x > vlo && x < vhi {
                                    let d = (x - mid_v).abs();
                                    if best_space.is_none_or(|(_, bd)| d < bd) {
                                        best_space = Some((i, d));
                                    }
                                }
                            }
                        }
                        if let Some((at_char, _)) = best_space {
                            let left_text: String = text
                                .chars()
                                .take(at_char)
                                .collect::<String>()
                                .trim_end()
                                .to_string();
                            let right_text: String = text
                                .chars()
                                .skip(at_char + 1)
                                .collect::<String>()
                                .trim_start()
                                .to_string();
                            if !left_text.is_empty() && !right_text.is_empty() {
                                let split_x =
                                    span.bbox.x0 + w * ((at_char as f32 + 0.5) / n as f32);
                                let mut lspan = span.clone();
                                lspan.text = left_text;
                                lspan.bbox =
                                    BBox::new(span.bbox.x0, span.bbox.y0, split_x, span.bbox.y1);
                                let mut rspan = span.clone();
                                rspan.text = right_text;
                                rspan.bbox =
                                    BBox::new(split_x, span.bbox.y0, span.bbox.x1, span.bbox.y1);
                                let mut left_spans = self.spans[..si].to_vec();
                                left_spans.push(lspan);
                                let mut right_spans = vec![rspan];
                                right_spans.extend_from_slice(&self.spans[si + 1..]);
                                cut = Some((left_spans, right_spans, split_x));
                                break;
                            }
                        }
                    }
                }
            }
        }
        let (left_spans, right_spans, split_x) = cut?;
        let mut left = TableIsland::from_spans(&left_spans, median_font);
        let mut right = TableIsland::from_spans(&right_spans, median_font);
        for (x, mark) in &self.scripts {
            if *x <= split_x {
                left.scripts.push((*x, mark.clone()));
            } else {
                right.scripts.push((*x, mark.clone()));
            }
        }
        Some((left, right))
    }
}

/// Group line refs into runs of aligned multi-island rows → tables.
///
/// Script-only rows (super/subscripts) attach to the nearest content row
/// with `^`/`_` marks instead of splitting runs.
fn aligned_runs_to_tables(
    lines: &[&TextLine],
    median_font: f32,
    source: TableSource,
) -> Vec<DetectedTable> {
    let rows = group_rows_by_overlap(lines);
    // Borrowed pass first (no clones): partition content/script rows.
    let mut borrowed: Vec<(f32, f32, Vec<Vec<&TextLine>>)> = Vec::new();
    let mut scripts: Vec<Vec<&TextLine>> = Vec::new();
    for row in &rows {
        if is_script_row(row, median_font) {
            scripts.push(row.clone());
        } else {
            let y0 = row.iter().map(|l| l.bbox.y0).fold(f32::INFINITY, f32::min);
            let y1 = row
                .iter()
                .map(|l| l.bbox.y1)
                .fold(f32::NEG_INFINITY, f32::max);
            let islands = split_row_islands(row, ISLAND_GAP_FONT_MULT * median_font);
            borrowed.push((y0, y1, islands));
        }
    }
    // Early bails before any owned building (span-string clones): runs need
    // ≥2 multi-island rows, and borderless runs need an anchor row (≥3
    // islands) — rules corroborate ruled pairs instead. (A run whose every
    // row is a bridge could theoretically gain its anchor from splitting;
    // such systematic-merge runs never cluster ambiguously in practice.)
    let mut multi = 0usize;
    let mut anchored = false;
    for (_, _, islands) in &borrowed {
        if islands.len() >= MIN_RUN_ISLANDS {
            multi += 1;
        }
        if islands.len() >= MIN_BORDERLESS_ISLANDS {
            anchored = true;
        }
    }
    if multi < MIN_TABLE_ROWS {
        return Vec::new();
    }
    if source == TableSource::Borderless && !anchored {
        return Vec::new();
    }
    // Owned conversion (the only cloning point).
    let mut content: Vec<(f32, f32, Vec<TableIsland>)> = Vec::with_capacity(borrowed.len());
    for (y0, y1, islands) in &borrowed {
        let owned: Vec<TableIsland> = islands
            .iter()
            .map(|isl| TableIsland::from_lines(isl))
            .collect();
        content.push((*y0, *y1, owned));
    }
    for script in &scripts {
        attach_script_row(&mut content, script);
    }
    // Median height over content rows only.
    let med_h = median_of(
        &content
            .iter()
            .flat_map(|(_, _, islands)| islands.iter().map(|isl| isl.bbox.height()))
            .collect::<Vec<_>>(),
    )
    .max(1.0);
    // Split content rows into runs at short rows and wide gaps.
    let mut tables = Vec::new();
    let mut run: Vec<Vec<TableIsland>> = Vec::new();
    let mut prev_y1 = 0.0f32;
    let mut first = true;
    let flush = |acc: &mut Vec<Vec<TableIsland>>, out: &mut Vec<DetectedTable>| {
        if acc.len() >= MIN_TABLE_ROWS {
            if let Some(table) = run_to_table(acc, median_font, source) {
                out.push(table);
            }
        }
        acc.clear();
    };
    let debug = std::env::var("RAZOR_DEBUG_TABLES").is_ok();
    for (y0, y1, owned) in &content {
        if debug {
            let preview: Vec<String> = owned
                .iter()
                .map(|isl| format!("[{:.0}..{:.0}]{:?}", isl.x0, isl.x1, isl.text()))
                .collect();
            eprintln!("ROW y={y0:.0}..{y1:.0} n={} {preview:?}", owned.len());
        }
        if owned.len() < MIN_RUN_ISLANDS || (!first && y0 - prev_y1 > MAX_ROW_GAP_MULT * med_h) {
            flush(&mut run, &mut tables);
            if owned.len() < MIN_RUN_ISLANDS {
                prev_y1 = *y1;
                first = false;
                continue;
            }
        }
        run.push(owned.clone());
        prev_y1 = *y1;
        first = false;
    }
    flush(&mut run, &mut tables);
    tables
}

/// True when every line in the row is small (super/subscript fragments).
fn is_script_row(row: &[&TextLine], median_font: f32) -> bool {
    !row.is_empty()
        && row
            .iter()
            .all(|l| l.font_size <= SCRIPT_MAX_FONT_MULT * median_font)
}

/// Attach script lines to the nearest content row's islands by x-overlap.
/// Superscripts get `^`, subscripts `_` (vs the target row band center).
fn attach_script_row(content: &mut [(f32, f32, Vec<TableIsland>)], row: &[&TextLine]) {
    if content.is_empty() || row.is_empty() {
        return;
    }
    let script_y0 = row.iter().map(|l| l.bbox.y0).fold(f32::INFINITY, f32::min);
    let script_y1 = row
        .iter()
        .map(|l| l.bbox.y1)
        .fold(f32::NEG_INFINITY, f32::max);
    // Nearest content row by band gap.
    let mut target: Option<usize> = None;
    let mut best_gap = f32::INFINITY;
    for (k, (y0, y1, _)) in content.iter().enumerate() {
        let gap = (y0 - script_y1).max(script_y0 - y1).max(0.0);
        if gap < best_gap {
            best_gap = gap;
            target = Some(k);
        }
    }
    let Some(t) = target else {
        return;
    };
    let center = (content[t].0 + content[t].1) * 0.5;
    for line in row {
        let mut best_island: Option<usize> = None;
        let mut best_score = f32::NEG_INFINITY;
        for (k, island) in content[t].2.iter().enumerate() {
            let overlap = (island.x1.min(line.bbox.x1) - island.x0.max(line.bbox.x0)).max(0.0);
            let dist = ((island.x0 + island.x1) * 0.5 - (line.bbox.x0 + line.bbox.x1) * 0.5).abs();
            let score = overlap * 1000.0 - dist;
            if score > best_score {
                best_score = score;
                best_island = Some(k);
            }
        }
        if let Some(k) = best_island {
            let (cx, cy) = line.bbox.centroid();
            let mark = if cy < center { "^" } else { "_" };
            let island = &mut content[t].2[k];
            island
                .scripts
                .push((cx, format!("{mark}{}", line.text.trim())));
            island.bbox = island.bbox.union(&line.bbox);
            island.x0 = island.x0.min(line.bbox.x0);
            island.x1 = island.x1.max(line.bbox.x1);
        }
    }
}

/// Union-find column clustering over flattened (row, island) pairs.
fn cluster_islands(run: &[Vec<TableIsland>], tol: f32) -> (Vec<(usize, usize)>, Vec<usize>) {
    let mut flat: Vec<(usize, usize)> = Vec::new();
    for (r, row) in run.iter().enumerate() {
        for c in 0..row.len() {
            flat.push((r, c));
        }
    }
    let mut dsu = Dsu::new(flat.len());
    for (i, &(r1, c1)) in flat.iter().enumerate() {
        let a = &run[r1][c1];
        let acx = (a.x0 + a.x1) * 0.5;
        for (j, &(r2, c2)) in flat.iter().enumerate().skip(i + 1) {
            if r1 == r2 {
                continue; // same-row islands are gap-separated by construction
            }
            let b = &run[r2][c2];
            let bcx = (b.x0 + b.x1) * 0.5;
            if (a.x0 - b.x0).abs() <= tol || (a.x1 - b.x1).abs() <= tol || (acx - bcx).abs() <= tol
            {
                dsu.union(i, j);
            }
        }
    }
    let mut root_of = vec![0usize; flat.len()];
    for (k, root) in root_of.iter_mut().enumerate() {
        *root = dsu.find(k);
    }
    (flat, root_of)
}

/// True when some row holds two islands of one cluster.
fn has_ambiguity(flat: &[(usize, usize)], root_of: &[usize], n_rows: usize) -> bool {
    let mut row_clusters: Vec<Vec<usize>> = vec![Vec::new(); n_rows];
    for (k, (r, _)) in flat.iter().enumerate() {
        if row_clusters[*r].contains(&root_of[k]) {
            return true;
        }
        row_clusters[*r].push(root_of[k]);
    }
    false
}

/// Inter-column valleys from clean cluster extents (sorted by min x0).
fn valleys_from_clusters(
    run: &[Vec<TableIsland>],
    flat: &[(usize, usize)],
    root_of: &[usize],
) -> Vec<(f32, f32)> {
    let mut extents: Vec<(usize, f32, f32)> = Vec::new();
    for (k, (r, c)) in flat.iter().enumerate() {
        let island = &run[*r][*c];
        match extents.iter_mut().find(|(root, _, _)| *root == root_of[k]) {
            Some(entry) => {
                entry.1 = entry.1.min(island.x0);
                entry.2 = entry.2.max(island.x1);
            }
            None => extents.push((root_of[k], island.x0, island.x1)),
        }
    }
    if extents.len() < 2 {
        return Vec::new();
    }
    extents.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut valleys = Vec::new();
    for window in extents.windows(2) {
        if window[0].2 < window[1].1 {
            valleys.push((window[0].2, window[1].1));
        }
    }
    valleys
}

/// Split bridge islands that wrongly merged two columns. For each ambiguous
/// cluster, the bridge is the member whose removal leaves a clean clustering
/// (tried widest-first); it splits at the cleaned valleys. True if all
/// ambiguous clusters resolved.
fn resolve_bridges(
    run: &mut [Vec<TableIsland>],
    root_of: &[usize],
    flat: &[(usize, usize)],
    median_font: f32,
    tol: f32,
) -> bool {
    // Ambiguous clusters: holding 2+ islands of one row.
    let mut roots: Vec<usize> = root_of.to_vec();
    roots.sort_unstable();
    roots.dedup();
    let mut bad: Vec<usize> = Vec::new();
    for root in &roots {
        let mut seen_rows: Vec<usize> = Vec::new();
        for (k, (r, _)) in flat.iter().enumerate() {
            if root_of[k] == *root {
                if seen_rows.contains(r) {
                    bad.push(*root);
                    break;
                }
                seen_rows.push(*r);
            }
        }
    }
    for root in &bad {
        // Members widest-first (bridges span columns, so they are wide).
        let mut members: Vec<(usize, usize)> = flat
            .iter()
            .enumerate()
            .filter(|(k, _)| root_of[*k] == *root)
            .map(|(_, pos)| *pos)
            .collect();
        members.sort_by(|a, b| {
            let wa = run[a.0][a.1].x1 - run[a.0][a.1].x0;
            let wb = run[b.0][b.1].x1 - run[b.0][b.1].x0;
            wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut resolved = false;
        for (br, bc) in &members {
            // Remainder without this member: clean + ≥2 clusters?
            let mut tmp: Vec<Vec<TableIsland>> = run.to_vec();
            tmp[*br].remove(*bc);
            let (flat_t, root_t) = cluster_islands(&tmp, tol);
            let mut distinct = root_t.clone();
            distinct.sort_unstable();
            distinct.dedup();
            if distinct.len() >= 2 && !has_ambiguity(&flat_t, &root_t, tmp.len()) {
                let valleys = valleys_from_clusters(&tmp, &flat_t, &root_t);
                let island = &run[*br][*bc];
                for &(vlo, vhi) in &valleys {
                    if island.x0 < vlo && island.x1 > vhi {
                        if let Some((left, right)) = island.split_at_valley(vlo, vhi, median_font) {
                            run[*br][*bc] = left;
                            run[*br].insert(*bc + 1, right);
                            resolved = true;
                            break;
                        }
                    }
                }
                if resolved {
                    break;
                }
            }
        }
        if !resolved {
            return false;
        }
    }
    !bad.is_empty()
}

/// Build a table from a run of multi-island rows.
///
/// Columns cluster transitively: islands share a column when left edges,
/// right edges, or centers agree within tolerance (covers left-, right-,
/// and center-aligned columns side by side). Rows may leave cells empty
/// (short header rows); the 30% fill gate still applies. Bridge islands
/// spanning two columns split at inter-column valleys before rejection.
fn run_to_table(
    run: &mut [Vec<TableIsland>],
    median_font: f32,
    source: TableSource,
) -> Option<DetectedTable> {
    let tol = (BUCKET_TOL_FONT_MULT * median_font).max(6.0);
    let (mut flat, mut root_of) = cluster_islands(run, tol);
    if has_ambiguity(&flat, &root_of, run.len()) {
        if !resolve_bridges(run, &root_of, &flat, median_font, tol) {
            return None;
        }
        (flat, root_of) = cluster_islands(run, tol);
        if has_ambiguity(&flat, &root_of, run.len()) {
            return None;
        }
    }
    // Singleton suppression: every column needs cells in ≥2 rows. A
    // one-cell column is a stray island (figure labels inside axes), not a
    // column. (Rowspan cells never reach here as singletons: tall lines
    // merge their rows during grouping, before clustering.)
    {
        let mut rows_per_root: Vec<(usize, Vec<usize>)> = Vec::new();
        for (k, (r, _)) in flat.iter().enumerate() {
            match rows_per_root
                .iter_mut()
                .find(|(root, _)| *root == root_of[k])
            {
                Some(entry) => {
                    if !entry.1.contains(r) {
                        entry.1.push(*r);
                    }
                }
                None => rows_per_root.push((root_of[k], vec![*r])),
            }
        }
        if rows_per_root.iter().any(|(_, rows)| rows.len() < 2) {
            return None;
        }
    }
    // Cap cluster spread on the tightest edge (anti-chaining).
    let cap = SPREAD_CAP_FONT_MULT * median_font;
    let mut roots: Vec<usize> = root_of.clone();
    roots.sort_unstable();
    roots.dedup();
    for root in &roots {
        let (mut lo0, mut hi0) = (f32::INFINITY, f32::NEG_INFINITY);
        let (mut lo1, mut hi1) = (f32::INFINITY, f32::NEG_INFINITY);
        let (mut loc, mut hic) = (f32::INFINITY, f32::NEG_INFINITY);
        for (k, (r, c)) in flat.iter().enumerate() {
            if root_of[k] != *root {
                continue;
            }
            let island = &run[*r][*c];
            let cx = (island.x0 + island.x1) * 0.5;
            lo0 = lo0.min(island.x0);
            hi0 = hi0.max(island.x0);
            lo1 = lo1.min(island.x1);
            hi1 = hi1.max(island.x1);
            loc = loc.min(cx);
            hic = hic.max(cx);
        }
        let spread = (hi0 - lo0).min(hi1 - lo1).min(hic - loc);
        if spread > cap {
            return None;
        }
    }
    if roots.len() < 2 {
        return None;
    }
    // Order clusters left-to-right by min x0.
    let mut ordered: Vec<(usize, f32)> = Vec::with_capacity(roots.len());
    for root in &roots {
        let mut min_x0 = f32::INFINITY;
        for (k, (r, c)) in flat.iter().enumerate() {
            if root_of[k] == *root {
                min_x0 = min_x0.min(run[*r][*c].x0);
            }
        }
        ordered.push((*root, min_x0));
    }
    ordered.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let ncols = ordered.len();
    // Borderless final gates: ≥3 columns + an anchor row (≥3 islands).
    // Two-column prose and staggered pairs die here; rules corroborate
    // ruled pairs, so the ruled path keeps its ≥2-column bar.
    if source == TableSource::Borderless
        && (ncols < MIN_BORDERLESS_ISLANDS
            || !run.iter().any(|row| row.len() >= MIN_BORDERLESS_ISLANDS))
    {
        return None;
    }
    let mut rows: Vec<Vec<String>> = vec![vec![String::new(); ncols]; run.len()];
    let mut bbox: Option<BBox> = None;
    let mut header_bold = false;
    let mut body_bold = false;
    for (col, (root, _)) in ordered.iter().enumerate() {
        for (k, (r, c)) in flat.iter().enumerate() {
            if root_of[k] != *root {
                continue;
            }
            let island = &run[*r][*c];
            rows[*r][col] = island.text();
            bbox = Some(bbox.map_or(island.bbox, |b| b.union(&island.bbox)));
            if island.bold {
                if *r == 0 {
                    header_bold = true;
                } else {
                    body_bold = true;
                }
            }
        }
    }
    // Same precision bar as grids: real tables carry text.
    let filled = rows
        .iter()
        .flatten()
        .filter(|cell| !cell.trim().is_empty())
        .count();
    if (filled as f32) / ((rows.len() * ncols) as f32) < MIN_CELL_FILL_RATIO {
        return None;
    }
    let first_full = rows
        .first()
        .is_some_and(|row| row.iter().all(|c| !c.is_empty()));
    Some(DetectedTable {
        bbox: bbox?,
        rows,
        has_header: (header_bold && !body_bold) || first_full || run.len() >= 2,
        source,
    })
}

/// Group line refs into rows by vertical overlap (input y-sorted).
fn group_rows_by_overlap<'a>(lines: &[&'a TextLine]) -> Vec<Vec<&'a TextLine>> {
    let mut rows: Vec<Vec<&TextLine>> = Vec::new();
    for line in lines {
        let same_row = rows.last().is_some_and(|row: &Vec<&TextLine>| {
            row.iter().any(|other| {
                line.bbox.vertical_overlap_ratio(&other.bbox) > 0.5
                    && line.bbox.vertical_containment_ratio(&other.bbox) > MERGE_MIN_CONTAINMENT
            })
        });
        if same_row {
            if let Some(row) = rows.last_mut() {
                row.push(line);
            }
        } else {
            rows.push(vec![line]);
        }
    }
    rows
}

/// Split a row into x-islands on gaps wider than `gap`.
fn split_row_islands<'a>(row: &[&'a TextLine], gap: f32) -> Vec<Vec<&'a TextLine>> {
    let mut sorted: Vec<&TextLine> = row.to_vec();
    sorted.sort_by(|a, b| {
        a.bbox
            .x0
            .partial_cmp(&b.bbox.x0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut islands: Vec<Vec<&TextLine>> = Vec::new();
    for line in sorted {
        let new_island = islands.last().is_some_and(|last: &Vec<&TextLine>| {
            let prev = last.last().expect("non-empty island");
            prev.bbox.h_gap(&line.bbox) >= gap
        });
        if new_island || islands.is_empty() {
            islands.push(vec![line]);
        } else if let Some(last) = islands.last_mut() {
            last.push(line);
        }
    }
    islands
}

/// Median of values (0.0 when empty).
fn median_of(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) * 0.5
    } else {
        sorted[mid]
    }
}

/// Axis-aligned intersection test within [`MERGE_TOL`].
fn segments_intersect(h: &VectorSegment, v: &VectorSegment) -> bool {
    let hy = (h.y0 + h.y1) * 0.5;
    let vx = (v.x0 + v.x1) * 0.5;
    let (hx0, hx1) = (h.x0.min(h.x1), h.x0.max(h.x1));
    let (vy0, vy1) = (v.y0.min(v.y1), v.y0.max(v.y1));
    vx >= hx0 - MERGE_TOL && vx <= hx1 + MERGE_TOL && hy >= vy0 - MERGE_TOL && hy <= vy1 + MERGE_TOL
}

/// Sort + merge coordinates within [`MERGE_TOL`] (averaging each cluster).
fn merge_coords(mut xs: Vec<f32>) -> Vec<f32> {
    if xs.is_empty() {
        return xs;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<f32> = Vec::with_capacity(xs.len());
    let mut acc = xs[0];
    let mut n = 1usize;
    for &x in &xs[1..] {
        if (x - acc / n as f32).abs() <= MERGE_TOL {
            acc += x;
            n += 1;
        } else {
            out.push(acc / n as f32);
            acc = x;
            n = 1;
        }
    }
    out.push(acc / n as f32);
    out
}

#[allow(clippy::too_many_arguments)]
fn build_table(
    vectors: &[VectorSegment],
    hs: &[usize],
    vs: &[usize],
    spans: &[TextSpan],
    span_tree: &RTree<Env>,
    fills: &[BBox],
) -> Option<DetectedTable> {
    let y_lines = merge_coords(
        hs.iter()
            .map(|&i| (vectors[i].y0 + vectors[i].y1) * 0.5)
            .collect(),
    );
    let x_lines = merge_coords(
        vs.iter()
            .map(|&i| (vectors[i].x0 + vectors[i].x1) * 0.5)
            .collect(),
    );
    if y_lines.len() < MIN_H_LINES || x_lines.len() < MIN_TABLE_COLS + 1 {
        return None;
    }
    let (nr, nc) = (y_lines.len() - 1, x_lines.len() - 1);

    let mut rows = vec![vec![String::new(); nc]; nr];
    let mut header_bold = false;
    let mut body_bold = false;
    for r in 0..nr {
        for c in 0..nc {
            let cell = BBox::new(x_lines[c], y_lines[r], x_lines[c + 1], y_lines[r + 1]);
            let query = envelope_of(cell.x0, cell.y0, cell.x1, cell.y1, 0.0);
            let mut hits: Vec<usize> = span_tree
                .locate_in_envelope_intersecting(&query)
                .map(|e| e.id)
                .filter(|&i| {
                    let (cx, cy) = spans[i].bbox.centroid();
                    cell.contains_point(cx, cy)
                })
                .collect();
            hits.sort_by(|&a, &b| {
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
            let text = hits
                .iter()
                .map(|&i| spans[i].text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if r == 0 && hits.iter().any(|&i| spans[i].is_bold) {
                header_bold = true;
            }
            if r > 0 && hits.iter().any(|&i| spans[i].is_bold) {
                body_bold = true;
            }
            rows[r][c] = text;
        }
    }

    // Precision gate: figure rules form empty grids; real tables carry text.
    let filled = rows
        .iter()
        .flatten()
        .filter(|cell| !cell.trim().is_empty())
        .count();
    if (filled as f32) / ((nr * nc) as f32) < MIN_CELL_FILL_RATIO {
        return None;
    }
    // Shading veto: grids whose cells are mostly filled rects are figures
    // (heatmaps, shaded diagrams), not tables. Banded-row tables (50%)
    // pass; fully shaded grids do not.
    if !fills.is_empty() {
        let mut shaded = 0usize;
        for r in 0..nr {
            for c in 0..nc {
                let cx = (x_lines[c] + x_lines[c + 1]) * 0.5;
                let cy = (y_lines[r] + y_lines[r + 1]) * 0.5;
                if fills.iter().any(|f| f.contains_point(cx, cy)) {
                    shaded += 1;
                }
            }
        }
        if (shaded as f32) / ((nr * nc) as f32) > MAX_GRID_SHADE_RATIO {
            return None;
        }
    }

    // Header heuristic: bold first row (with non-bold body), else require a
    // fully populated first row; default to header for 2+ row grids.
    let first_full = rows
        .first()
        .is_some_and(|r| !r.is_empty() && r.iter().all(|c| !c.is_empty()));
    let has_header = (header_bold && !body_bold) || first_full || nr >= 2;

    let bbox = BBox::new(x_lines[0], y_lines[0], x_lines[nc], y_lines[nr]);
    Some(DetectedTable {
        bbox,
        rows,
        has_header,
        source: TableSource::VectorGrid,
    })
}

/// Render a detected table as a Markdown table.
pub fn detected_table_to_markdown(table: &DetectedTable) -> String {
    if table.rows.is_empty() || table.n_cols() == 0 {
        return String::new();
    }
    let esc = |s: &str| {
        // Single pass: escape table pipes, fold \r\n / \r / \n to <br>.
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '|' => out.push_str("\\|"),
                '\r' => {
                    out.push_str("<br>");
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                }
                '\n' => out.push_str("<br>"),
                c => out.push(c),
            }
        }
        out
    };
    let mut out = String::new();
    let (header, body): (&[String], &[Vec<String>]) = if table.has_header {
        (&table.rows[0], &table.rows[1..])
    } else {
        (&[], &table.rows[..])
    };
    if table.has_header {
        out.push_str("| ");
        out.push_str(
            &header
                .iter()
                .map(|c| esc(c))
                .collect::<Vec<_>>()
                .join(" | "),
        );
        out.push_str(" |\n");
    } else {
        out.push_str("| ");
        out.push_str(&vec![" "; table.n_cols()].join(" | "));
        out.push_str(" |\n");
    }
    out.push_str("| ");
    out.push_str(&vec!["---"; table.n_cols()].join(" | "));
    out.push_str(" |\n");
    for row in body {
        out.push_str("| ");
        out.push_str(&row.iter().map(|c| esc(c)).collect::<Vec<_>>().join(" | "));
        out.push_str(" |\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_vectors(xs: &[f32], ys: &[f32]) -> Vec<VectorSegment> {
        let mut v = Vec::new();
        for &y in ys {
            v.push(VectorSegment {
                x0: xs[0],
                y0: y,
                x1: xs[xs.len() - 1],
                y1: y,
                stroke_width: 0.5,
            });
        }
        for &x in xs {
            v.push(VectorSegment {
                x0: x,
                y0: ys[0],
                x1: x,
                y1: ys[ys.len() - 1],
                stroke_width: 0.5,
            });
        }
        v
    }

    fn cell_span(text: &str, cx: f32, cy: f32, bold: bool) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            bbox: BBox::new(cx - 15.0, cy - 5.0, cx + 15.0, cy + 5.0),
            font_name: if bold { "Helvetica-Bold" } else { "Helvetica" }.to_string(),
            font_size: 10.0,
            is_bold: bold,
            is_italic: false,
            is_monospace: false,
        }
    }

    #[test]
    fn reconstructs_simple_grid() {
        let xs = [72.0, 200.0, 340.0, 480.0];
        let ys = [120.0, 145.0, 170.0, 195.0];
        let vectors = grid_vectors(&xs, &ys);
        let data = [
            ["Name", "Qty", "Price"],
            ["Apples", "12", "1.20"],
            ["Bread", "3", "2.50"],
        ];
        let mut spans = Vec::new();
        for (r, row) in data.iter().enumerate() {
            for (c, text) in row.iter().enumerate() {
                let cx = (xs[c] + xs[c + 1]) * 0.5;
                let cy = (ys[r] + ys[r + 1]) * 0.5;
                spans.push(cell_span(text, cx, cy, r == 0));
            }
        }
        let tables = detect_vector_tables(&vectors, &spans, &[]);
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.n_rows(), 3);
        assert_eq!(t.n_cols(), 3);
        assert!(t.has_header);
        assert_eq!(t.rows[0], vec!["Name", "Qty", "Price"]);
        assert_eq!(t.rows[2], vec!["Bread", "3", "2.50"]);

        let md = detected_table_to_markdown(t);
        assert!(md.contains("| Name | Qty | Price |"));
        assert!(md.contains("| --- | --- | --- |"));
        assert!(md.contains("| Bread | 3 | 2.50 |"));
    }

    #[test]
    fn ignores_non_grid_vectors() {
        // Only 2 horizontal lines: below the grid threshold.
        let vectors = vec![
            VectorSegment {
                x0: 0.0,
                y0: 10.0,
                x1: 100.0,
                y1: 10.0,
                stroke_width: 1.0,
            },
            VectorSegment {
                x0: 0.0,
                y0: 20.0,
                x1: 100.0,
                y1: 20.0,
                stroke_width: 1.0,
            },
            VectorSegment {
                x0: 5.0,
                y0: 0.0,
                x1: 5.0,
                y1: 30.0,
                stroke_width: 1.0,
            },
        ];
        assert!(detect_vector_tables(&vectors, &[], &[]).is_empty());
    }

    #[test]
    fn ignores_diagonal_noise() {
        let vectors = vec![
            VectorSegment {
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: 50.0,
                stroke_width: 1.0,
            },
            VectorSegment {
                x0: 10.0,
                y0: 40.0,
                x1: 90.0,
                y1: 5.0,
                stroke_width: 1.0,
            },
        ];
        assert!(detect_vector_tables(&vectors, &[], &[]).is_empty());
    }

    #[test]
    fn rejects_single_column_grids() {
        // 3 rules × 2 verticals = 1 column: figure rule, not a table.
        let vectors = grid_vectors(&[72.0, 200.0], &[100.0, 125.0, 150.0]);
        let spans = vec![
            cell_span("a", 136.0, 112.5, false),
            cell_span("b", 136.0, 137.5, false),
        ];
        assert!(detect_vector_tables(&vectors, &spans, &[]).is_empty());
    }

    #[test]
    fn rejects_low_fill_grids() {
        // 2×3 grid: 1/6 cells filled (17%) → dropped; 2/6 (33%) → kept.
        let xs = [72.0, 200.0, 340.0];
        let ys = [100.0, 125.0, 150.0, 175.0];
        let vectors = grid_vectors(&xs, &ys);
        let one = vec![cell_span("a", 136.0, 112.5, false)];
        assert!(detect_vector_tables(&vectors, &one, &[]).is_empty());
        let two = vec![
            cell_span("a", 136.0, 112.5, false),
            cell_span("b", 270.0, 137.5, false),
        ];
        let tables = detect_vector_tables(&vectors, &two, &[]);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].n_cols(), 2);
        assert_eq!(tables[0].n_rows(), 3);
    }

    #[test]
    fn shaded_grids_are_figures() {
        // Same 2×3 grid fully text-filled: kept bare, rejected when every
        // cell center sits inside a filled rect (heatmap shading).
        let xs = [72.0, 200.0, 340.0];
        let ys = [100.0, 125.0, 150.0, 175.0];
        let vectors = grid_vectors(&xs, &ys);
        let spans = vec![
            cell_span("a", 136.0, 112.5, false),
            cell_span("b", 270.0, 112.5, false),
            cell_span("c", 136.0, 137.5, false),
            cell_span("d", 270.0, 137.5, false),
            cell_span("e", 136.0, 162.5, false),
            cell_span("f", 270.0, 162.5, false),
        ];
        assert_eq!(detect_vector_tables(&vectors, &spans, &[]).len(), 1);
        let fills = vec![BBox::new(72.0, 100.0, 340.0, 175.0)];
        assert!(detect_vector_tables(&vectors, &spans, &fills).is_empty());
        // Banded shading (exactly 50%) still passes.
        let ys2 = [100.0, 125.0, 150.0];
        let vectors2 = grid_vectors(&xs, &ys2);
        let spans2 = vec![
            cell_span("a", 136.0, 112.5, false),
            cell_span("b", 270.0, 112.5, false),
            cell_span("c", 136.0, 137.5, false),
            cell_span("d", 270.0, 137.5, false),
        ];
        let banded = vec![BBox::new(72.0, 100.0, 340.0, 125.0)];
        assert_eq!(detect_vector_tables(&vectors2, &spans2, &banded).len(), 1);
    }

    fn sized_line(text: &str, x0: f32, y0: f32, x1: f32, y1: f32, size: f32) -> TextLine {
        let span = TextSpan {
            text: text.to_string(),
            bbox: BBox::new(x0, y0, x1, y1),
            font_name: "Helvetica".to_string(),
            font_size: size,
            is_bold: false,
            is_italic: false,
            is_monospace: false,
        };
        TextLine {
            spans: vec![span],
            bbox: BBox::new(x0, y0, x1, y1),
            text: text.to_string(),
            font_size: size,
            is_bold: false,
            is_monospace: false,
        }
    }

    fn text_line(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> TextLine {
        sized_line(text, x0, y0, x1, y1, 10.0)
    }

    fn hrule(x0: f32, x1: f32, y: f32) -> VectorSegment {
        VectorSegment {
            x0,
            y0: y,
            x1,
            y1: y,
            stroke_width: 0.5,
        }
    }

    #[test]
    fn detects_ruled_booktabs_table() {
        // 3-col booktabs: 3 rules + header + 2 body rows, no verticals.
        let vectors = vec![
            hrule(72.0, 300.0, 100.0),
            hrule(72.0, 300.0, 128.0),
            hrule(72.0, 300.0, 184.0),
        ];
        let lines = vec![
            text_line("A", 72.0, 104.0, 100.0, 114.0),
            text_line("B", 150.0, 104.0, 180.0, 114.0),
            text_line("C", 230.0, 104.0, 260.0, 114.0),
            text_line("a1", 72.0, 132.0, 100.0, 142.0),
            text_line("b1", 150.0, 132.0, 180.0, 142.0),
            text_line("c1", 230.0, 132.0, 260.0, 142.0),
            text_line("a2", 72.0, 160.0, 100.0, 170.0),
            text_line("b2", 150.0, 160.0, 180.0, 170.0),
            text_line("c2", 230.0, 160.0, 260.0, 170.0),
        ];
        let tables = detect_ruled_tables(&vectors, &lines, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].source, TableSource::Ruled);
        assert_eq!(
            tables[0].rows,
            vec![
                vec!["A".to_string(), "B".to_string(), "C".to_string()],
                vec!["a1".to_string(), "b1".to_string(), "c1".to_string()],
                vec!["a2".to_string(), "b2".to_string(), "c2".to_string()],
            ]
        );
    }

    #[test]
    fn detects_short_borderless_table() {
        // 2 aligned 3-island rows, zero vectors.
        let lines = vec![
            text_line("A", 72.0, 104.0, 100.0, 114.0),
            text_line("B", 150.0, 104.0, 180.0, 114.0),
            text_line("C", 230.0, 104.0, 260.0, 114.0),
            text_line("a1", 72.0, 120.0, 100.0, 130.0),
            text_line("b1", 150.0, 120.0, 180.0, 130.0),
            text_line("c1", 230.0, 120.0, 260.0, 130.0),
        ];
        let tables = detect_borderless_tables(&lines, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].source, TableSource::Borderless);
        assert_eq!(tables[0].n_rows(), 2);
        assert_eq!(tables[0].n_cols(), 3);
    }

    #[test]
    fn right_aligned_columns_cluster_by_right_edge() {
        // x0 jitters (right-aligned numbers), x1 stable → still one table.
        let lines = vec![
            text_line("aa", 72.0, 104.0, 100.0, 114.0),
            text_line("b", 190.0, 104.0, 200.0, 114.0),
            text_line("c", 280.0, 104.0, 300.0, 114.0),
            text_line("aaaa", 72.0, 120.0, 110.0, 130.0),
            text_line("bb", 180.0, 120.0, 200.0, 130.0),
            text_line("cc", 270.0, 120.0, 300.0, 130.0),
        ];
        let tables = detect_borderless_tables(&lines, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].n_cols(), 3);
        assert_eq!(tables[0].rows[1][1], "bb");
    }

    #[test]
    fn script_rows_attach_with_marks() {
        // Superscript exponents above a body row attach with ^ marks.
        let lines = vec![
            sized_line("19", 195.0, 96.0, 210.0, 102.0, 7.0),
            sized_line("20", 275.0, 96.0, 290.0, 102.0, 7.0),
            text_line("Cost", 72.0, 104.0, 120.0, 114.0),
            text_line("1.0·10", 150.0, 104.0, 210.0, 114.0),
            text_line("2.0·10", 235.0, 104.0, 295.0, 114.0),
            text_line("Cost2", 72.0, 120.0, 120.0, 130.0),
            text_line("3.0·10", 150.0, 120.0, 210.0, 130.0),
            text_line("4.0·10", 235.0, 120.0, 295.0, 130.0),
        ];
        let tables = detect_borderless_tables(&lines, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(
            tables[0].rows,
            vec![
                vec![
                    "Cost".to_string(),
                    "1.0·10^19".to_string(),
                    "2.0·10^20".to_string()
                ],
                vec![
                    "Cost2".to_string(),
                    "3.0·10".to_string(),
                    "4.0·10".to_string()
                ],
            ]
        );
    }

    #[test]
    fn bridge_islands_split_at_valleys() {
        // Row 2 merges name+formula into one island spanning the A|B valley;
        // the inter-span gap inside the valley splits it back into columns.
        let bridge = TextLine {
            spans: vec![
                TextSpan {
                    text: "aa".to_string(),
                    bbox: BBox::new(72.0, 136.0, 100.0, 146.0),
                    font_name: "Helvetica".to_string(),
                    font_size: 10.0,
                    is_bold: false,
                    is_italic: false,
                    is_monospace: false,
                },
                TextSpan {
                    text: "bb".to_string(),
                    bbox: BBox::new(160.0, 136.0, 180.0, 146.0),
                    font_name: "Helvetica".to_string(),
                    font_size: 10.0,
                    is_bold: false,
                    is_italic: false,
                    is_monospace: false,
                },
            ],
            bbox: BBox::new(72.0, 136.0, 180.0, 146.0),
            text: "aa bb".to_string(),
            font_size: 10.0,
            is_bold: false,
            is_monospace: false,
        };
        let lines = vec![
            text_line("a", 72.0, 104.0, 100.0, 114.0),
            text_line("b", 150.0, 104.0, 180.0, 114.0),
            text_line("c", 230.0, 104.0, 260.0, 114.0),
            text_line("a", 72.0, 120.0, 100.0, 130.0),
            text_line("b", 150.0, 120.0, 180.0, 130.0),
            text_line("c", 230.0, 120.0, 260.0, 130.0),
            bridge,
            text_line("c", 230.0, 136.0, 260.0, 146.0),
        ];
        let tables = detect_borderless_tables(&lines, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].n_cols(), 3);
        assert_eq!(tables[0].rows[2], vec!["aa", "bb", "c"]);
    }

    #[test]
    fn bridge_span_splits_at_inner_space() {
        // Single-span island covering the valley splits at its inner space.
        let bridge = TextLine {
            spans: vec![TextSpan {
                text: "aa bb".to_string(),
                bbox: BBox::new(72.0, 136.0, 180.0, 146.0),
                font_name: "Helvetica".to_string(),
                font_size: 10.0,
                is_bold: false,
                is_italic: false,
                is_monospace: false,
            }],
            bbox: BBox::new(72.0, 136.0, 180.0, 146.0),
            text: "aa bb".to_string(),
            font_size: 10.0,
            is_bold: false,
            is_monospace: false,
        };
        let lines = vec![
            text_line("a", 72.0, 104.0, 100.0, 114.0),
            text_line("b", 150.0, 104.0, 180.0, 114.0),
            text_line("c", 230.0, 104.0, 260.0, 114.0),
            text_line("a", 72.0, 120.0, 100.0, 130.0),
            text_line("b", 150.0, 120.0, 180.0, 130.0),
            text_line("c", 230.0, 120.0, 260.0, 130.0),
            bridge,
            text_line("c", 230.0, 136.0, 260.0, 146.0),
        ];
        let tables = detect_borderless_tables(&lines, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows[2], vec!["aa", "bb", "c"]);
    }

    #[test]
    fn rotated_watermark_row_does_not_swallow_table() {
        // Tall watermark line overlapping two body rows: rows stay separate
        // and the 2-row table still forms.
        let watermark = TextLine {
            spans: vec![TextSpan {
                text: "CONFIDENTIAL DRAFT".to_string(),
                bbox: BBox::new(185.0, 37.0, 523.0, 220.0),
                font_name: "Helvetica-Bold".to_string(),
                font_size: 36.0,
                is_bold: true,
                is_italic: false,
                is_monospace: false,
            }],
            bbox: BBox::new(185.0, 37.0, 523.0, 220.0),
            text: "CONFIDENTIAL DRAFT".to_string(),
            font_size: 36.0,
            is_bold: true,
            is_monospace: false,
        };
        let lines = vec![
            watermark,
            text_line("Revenue", 72.0, 130.0, 150.0, 140.0),
            text_line("1,420.50", 260.0, 130.0, 310.0, 140.0),
            text_line("+33.1%", 360.0, 130.0, 410.0, 140.0),
            text_line("Costs", 72.0, 154.0, 150.0, 164.0),
            text_line("410.00", 260.0, 154.0, 310.0, 164.0),
            text_line("+25.0%", 360.0, 154.0, 410.0, 164.0),
        ];
        let tables = detect_borderless_tables(&lines, 10.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].n_rows(), 2);
        assert_eq!(tables[0].n_cols(), 3);
    }

    #[test]
    fn transitive_chains_are_not_columns() {
        // 5 rows × 3 cols drifting +6pt/row: chained clusters span 24pt
        // on every edge → spread cap rejects.
        let mut lines = Vec::new();
        for r in 0..5 {
            let y = 104.0 + r as f32 * 16.0;
            let dx = r as f32 * 6.0;
            lines.push(text_line("a", 72.0 + dx, y, 100.0 + dx, y + 10.0));
            lines.push(text_line("b", 150.0 + dx, y, 178.0 + dx, y + 10.0));
            lines.push(text_line("c", 230.0 + dx, y, 258.0 + dx, y + 10.0));
        }
        assert!(detect_borderless_tables(&lines, 10.0).is_empty());
    }

    #[test]
    fn two_column_prose_is_not_a_table() {
        // 6 rows × 2 islands: excluded by the island count.
        let mut lines = Vec::new();
        for r in 0..6 {
            let y = 100.0 + r as f32 * 14.0;
            lines.push(text_line("L", 72.0, y, 200.0, y + 10.0));
            lines.push(text_line("R", 330.0, y, 460.0, y + 10.0));
        }
        assert!(detect_borderless_tables(&lines, 10.0).is_empty());
        assert!(detect_all_tables(&[], &[], &lines, 10.0, &[]).is_empty());
    }
}
