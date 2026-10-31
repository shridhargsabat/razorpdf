//! Recursive XY-Cut for multi-column reading order.
//!
//! Projects line bboxes onto X/Y occupancy histograms (bin `Δ = 2.0pt`),
//! splits on whitespace valleys, and recurses. Y-cuts (top/bottom) take
//! priority over X-cuts (left/right) so full-width titles stay first.

use crate::config::ParserConfig;
use crate::geometry::lines::TextLine;
use crate::models::BBox;

/// Anything with a bounding box can be XY-cut ordered.
pub trait HasBBox {
    fn bbox(&self) -> BBox;
}

impl HasBBox for BBox {
    fn bbox(&self) -> BBox {
        *self
    }
}

impl HasBBox for crate::models::TextSpan {
    fn bbox(&self) -> BBox {
        self.bbox
    }
}

impl HasBBox for TextLine {
    fn bbox(&self) -> BBox {
        self.bbox
    }
}

/// Reading order of `items` as indices, using explicit medians.
pub fn reading_order_with_medians<T: HasBBox>(
    items: &[T],
    median_font_size: f32,
    median_line_height: f32,
    config: &ParserConfig,
) -> Vec<usize> {
    if items.len() <= 1 {
        return (0..items.len()).collect();
    }
    let boxes: Vec<BBox> = items.iter().map(HasBBox::bbox).collect();
    let region = BBox::union_all(boxes.iter().copied()).unwrap_or(BBox::zero());
    let indices: Vec<usize> = (0..items.len()).collect();
    recurse(
        &boxes,
        indices,
        region,
        median_font_size.max(1.0),
        median_line_height.max(1.0),
        config,
    )
}

/// Reading order of text lines, computing medians from the lines themselves.
pub fn reading_order(lines: &[TextLine], config: &ParserConfig) -> Vec<usize> {
    let med_font = crate::geometry::lines::median_font_size(lines);
    let med_h = crate::geometry::lines::median_line_height(lines);
    reading_order_with_medians(lines, med_font, med_h, config)
}

/// Clone `items` into XY-cut reading order.
pub fn sort_by_reading_order<T: HasBBox + Clone>(
    items: &[T],
    median_font_size: f32,
    median_line_height: f32,
    config: &ParserConfig,
) -> Vec<T> {
    reading_order_with_medians(items, median_font_size, median_line_height, config)
        .into_iter()
        .map(|i| items[i].clone())
        .collect()
}

fn recurse(
    boxes: &[BBox],
    indices: Vec<usize>,
    region: BBox,
    med_font: f32,
    med_h: f32,
    config: &ParserConfig,
) -> Vec<usize> {
    if indices.len() <= 1 {
        return indices;
    }
    let bin = config.xy_bin_pt.max(0.5);
    let tau_y = config.xy_tau_y_mult * med_h;
    let tau_x = config.xy_tau_x_mult * med_font;

    // Horizontal (Y) valleys first: full-width whitespace bands.
    if let Some(split_y) = find_valley(
        boxes,
        &indices,
        region,
        bin,
        tau_y,
        Axis::Y,
    ) {
        let (mut top, mut bottom) = (Vec::new(), Vec::new());
        for &i in &indices {
            let (_, cy) = boxes[i].centroid();
            if cy < split_y {
                top.push(i);
            } else {
                bottom.push(i);
            }
        }
        if !top.is_empty() && !bottom.is_empty() {
            let top_region =
                BBox::union_all(top.iter().map(|&i| boxes[i])).unwrap_or(region);
            let bottom_region =
                BBox::union_all(bottom.iter().map(|&i| boxes[i])).unwrap_or(region);
            let mut out = recurse(boxes, top, top_region, med_font, med_h, config);
            out.extend(recurse(
                boxes,
                bottom,
                bottom_region,
                med_font,
                med_h,
                config,
            ));
            return out;
        }
    }

    // Vertical (X) valleys: column gutters.
    if let Some(split_x) = find_valley(
        boxes,
        &indices,
        region,
        bin,
        tau_x,
        Axis::X,
    ) {
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for &i in &indices {
            let (cx, _) = boxes[i].centroid();
            if cx < split_x {
                left.push(i);
            } else {
                right.push(i);
            }
        }
        if !left.is_empty() && !right.is_empty() {
            let left_region =
                BBox::union_all(left.iter().map(|&i| boxes[i])).unwrap_or(region);
            let right_region =
                BBox::union_all(right.iter().map(|&i| boxes[i])).unwrap_or(region);
            let mut out = recurse(boxes, left, left_region, med_font, med_h, config);
            out.extend(recurse(
                boxes,
                right,
                right_region,
                med_font,
                med_h,
                config,
            ));
            return out;
        }
    }

    // Base case: top-to-bottom, then left-to-right.
    let mut sorted = indices;
    sorted.sort_by(|&a, &b| {
        boxes[a]
            .y0
            .partial_cmp(&boxes[b].y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                boxes[a]
                    .x0
                    .partial_cmp(&boxes[b].x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    sorted
}

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
}

/// Find the center of the largest whitespace valley along `axis` whose
/// extent meets `threshold` points. Returns the split coordinate.
fn find_valley(
    boxes: &[BBox],
    indices: &[usize],
    region: BBox,
    bin: f32,
    threshold: f32,
    axis: Axis,
) -> Option<f32> {
    let extent = match axis {
        Axis::X => region.width(),
        Axis::Y => region.height(),
    };
    if extent <= bin {
        return None;
    }
    let n_bins = (extent / bin).ceil() as usize;
    if n_bins == 0 {
        return None;
    }
    let mut occupied = vec![false; n_bins];
    let origin = match axis {
        Axis::X => region.x0,
        Axis::Y => region.y0,
    };
    for &i in indices {
        let b = boxes[i];
        let (lo, hi) = match axis {
            Axis::X => (b.x0, b.x1),
            Axis::Y => (b.y0, b.y1),
        };
        let start = ((lo - origin) / bin).floor() as isize;
        let end = ((hi - origin) / bin).ceil() as isize;
        for k in start.max(0)..end.min(n_bins as isize) {
            occupied[k as usize] = true;
        }
    }

    // Longest contiguous run of unoccupied bins.
    let mut best: Option<(usize, usize)> = None; // (start, len)
    let mut run_start: Option<usize> = None;
    for (k, &occ) in occupied.iter().enumerate() {
        if occ {
            if let Some(s) = run_start.take() {
                let len = k - s;
                if best.map_or(true, |(_, bl)| len > bl) {
                    best = Some((s, len));
                }
            }
        } else if run_start.is_none() {
            run_start = Some(k);
        }
    }
    if let Some(s) = run_start {
        let len = n_bins - s;
        if best.map_or(true, |(_, bl)| len > bl) {
            best = Some((s, len));
        }
    }

    // Ignore valleys touching the region border (leading/trailing margins).
    let (start, len) = best?;
    if start == 0 || start + len == n_bins {
        // A border-touching run is only a margin, not a separator — unless a
        // second qualifying interior valley exists. Re-scan excluding borders.
        return find_interior_valley(&occupied, origin, bin, threshold);
    }
    if len as f32 * bin >= threshold {
        Some(origin + (start as f32 + len as f32 * 0.5) * bin)
    } else {
        // Largest run too small, but a smaller interior run cannot qualify
        // either (largest < threshold), so bail out.
        None
    }
}

/// Largest interior (non-border-touching) valley meeting the threshold.
fn find_interior_valley(
    occupied: &[bool],
    origin: f32,
    bin: f32,
    threshold: f32,
) -> Option<f32> {
    let n = occupied.len();
    if n < 3 {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    let mut k = 1usize;
    while k < n - 1 {
        if occupied[k] {
            k += 1;
            continue;
        }
        let s = k;
        while k < n - 1 && !occupied[k] {
            k += 1;
        }
        let len = k - s;
        if best.map_or(true, |(_, bl)| len > bl) {
            best = Some((s, len));
        }
    }
    let (start, len) = best?;
    if len as f32 * bin >= threshold {
        Some(origin + (start as f32 + len as f32 * 0.5) * bin)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TextSpan;

    fn line(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> TextLine {
        TextLine {
            spans: vec![TextSpan {
                text: text.to_string(),
                bbox: BBox::new(x0, y0, x1, y1),
                font_name: "Helvetica".to_string(),
                font_size: 10.0,
                is_bold: false,
                is_italic: false,
                is_monospace: false,
            }],
            bbox: BBox::new(x0, y0, x1, y1),
            text: text.to_string(),
            font_size: 10.0,
            is_bold: false,
            is_monospace: false,
        }
    }

    #[test]
    fn single_column_top_to_bottom() {
        let cfg = ParserConfig::default();
        let lines = vec![
            line("b", 72.0, 130.0, 300.0, 140.0),
            line("a", 72.0, 100.0, 300.0, 110.0),
            line("c", 72.0, 160.0, 300.0, 170.0),
        ];
        let order = reading_order(&lines, &cfg);
        let texts: Vec<&str> = order.iter().map(|&i| lines[i].text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
    }

    #[test]
    fn two_columns_left_before_right() {
        let cfg = ParserConfig::default();
        // Interleaved input: right-col lines sorted before left-col lines.
        let mut lines = Vec::new();
        for r in 0..6 {
            let y = 120.0 + r as f32 * 14.0;
            lines.push(line(
                &format!("R{r}"),
                330.0,
                y,
                520.0,
                y + 10.0,
            ));
            lines.push(line(&format!("L{r}"), 72.0, y, 260.0, y + 10.0));
        }
        let order = reading_order(&lines, &cfg);
        let texts: Vec<&str> = order.iter().map(|&i| lines[i].text.as_str()).collect();
        let expected = vec![
            "L0", "L1", "L2", "L3", "L4", "L5", "R0", "R1", "R2", "R3", "R4", "R5",
        ];
        assert_eq!(texts, expected);
    }

    #[test]
    fn full_width_title_stays_first() {
        let cfg = ParserConfig::default();
        let mut lines = vec![line("Title", 150.0, 60.0, 460.0, 78.0)];
        for r in 0..4 {
            let y = 120.0 + r as f32 * 14.0;
            lines.push(line(&format!("R{r}"), 330.0, y, 520.0, y + 10.0));
            lines.push(line(&format!("L{r}"), 72.0, y, 260.0, y + 10.0));
        }
        let order = reading_order(&lines, &cfg);
        let texts: Vec<&str> = order.iter().map(|&i| lines[i].text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["Title", "L0", "L1", "L2", "L3", "R0", "R1", "R2", "R3"]
        );
    }
}
