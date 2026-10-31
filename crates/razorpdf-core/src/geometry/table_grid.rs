//! Deterministic vector table reconstruction.
//!
//! Axis-aligned vector segments are clustered into grid components with an
//! R-tree; closed grids snap text-span centroids into Markdown tables with
//! zero model calls.

use crate::models::{BBox, TextSpan, VectorSegment};
use rstar::{AABB, RTree, RTreeObject};

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

/// A deterministically reconstructed table.
#[derive(Debug, Clone)]
pub struct DetectedTable {
    /// Union bbox of the grid.
    pub bbox: BBox,
    /// Cell texts, row-major. `rows[r][c]` is the text of cell (r, c).
    pub rows: Vec<Vec<String>>,
    /// Whether the first row is a header.
    pub has_header: bool,
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
        [
            f64::from(x0.min(x1) - pad),
            f64::from(y0.min(y1) - pad),
        ],
        [
            f64::from(x0.max(x1) + pad),
            f64::from(y0.max(y1) + pad),
        ],
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
pub fn detect_vector_tables(
    vectors: &[VectorSegment],
    spans: &[TextSpan],
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
        if let Some(t) = build_table(vectors, hs, vs, spans, &span_tree) {
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
    if y_lines.len() < MIN_H_LINES || x_lines.len() < MIN_V_LINES {
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

    // Header heuristic: bold first row (with non-bold body), else require a
    // fully populated first row; default to header for 2+ row grids.
    let first_full = rows.first().map_or(false, |r| {
        !r.is_empty() && r.iter().all(|c| !c.is_empty())
    });
    let has_header = (header_bold && !body_bold) || first_full || nr >= 2;

    let bbox = BBox::new(
        x_lines[0],
        y_lines[0],
        x_lines[nc],
        y_lines[nr],
    );
    Some(DetectedTable {
        bbox,
        rows,
        has_header,
    })
}

/// Render a detected table as a Markdown table.
pub fn detected_table_to_markdown(table: &DetectedTable) -> String {
    if table.rows.is_empty() || table.n_cols() == 0 {
        return String::new();
    }
    let esc = |s: &str| s.replace('|', "\\|").replace('\n', "<br>");
    let mut out = String::new();
    let (header, body): (&[String], &[Vec<String>]) = if table.has_header {
        (&table.rows[0], &table.rows[1..])
    } else {
        (&[], &table.rows[..])
    };
    if table.has_header {
        out.push_str("| ");
        out.push_str(
            &header.iter().map(|c| esc(c)).collect::<Vec<_>>().join(" | "),
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

    fn grid_vectors(
        xs: &[f32],
        ys: &[f32],
    ) -> Vec<VectorSegment> {
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
        let tables = detect_vector_tables(&vectors, &spans);
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
            VectorSegment { x0: 0.0, y0: 10.0, x1: 100.0, y1: 10.0, stroke_width: 1.0 },
            VectorSegment { x0: 0.0, y0: 20.0, x1: 100.0, y1: 20.0, stroke_width: 1.0 },
            VectorSegment { x0: 5.0, y0: 0.0, x1: 5.0, y1: 30.0, stroke_width: 1.0 },
        ];
        assert!(detect_vector_tables(&vectors, &[]).is_empty());
    }

    #[test]
    fn ignores_diagonal_noise() {
        let vectors = vec![
            VectorSegment { x0: 0.0, y0: 0.0, x1: 100.0, y1: 50.0, stroke_width: 1.0 },
            VectorSegment { x0: 10.0, y0: 40.0, x1: 90.0, y1: 5.0, stroke_width: 1.0 },
        ];
        assert!(detect_vector_tables(&vectors, &[]).is_empty());
    }
}
