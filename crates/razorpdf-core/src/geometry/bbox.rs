//! Bounding-box algebra with top-left origin.
//!
//! PDF internals use bottom-left origin; coordinates are flipped to top-left
//! immediately upon extraction via [`BBox::from_pdf_coords`].

use crate::models::BBox;

impl BBox {
    /// Construct a normalized bbox (ensures `x0 <= x1`, `y0 <= y1`).
    #[must_use]
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    /// Zero-area bbox at the origin.
    #[must_use]
    pub fn zero() -> Self {
        Self {
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        }
    }

    /// Convert bottom-left-origin PDF coords to top-left-origin coords.
    ///
    /// `page_height` is the page height in points. The x-axis is unchanged;
    /// `y_top = H - y_pdf` with normalization so `y0 <= y1`.
    #[must_use]
    pub fn from_pdf_coords(x0: f32, y0: f32, x1: f32, y1: f32, page_height: f32) -> Self {
        let (lx, hx) = (x0.min(x1), x0.max(x1));
        let (ly, hy) = (y0.min(y1), y0.max(y1));
        Self {
            x0: lx,
            y0: page_height - hy,
            x1: hx,
            y1: page_height - ly,
        }
    }

    /// Flip a single PDF y-coordinate to top-left origin.
    #[must_use]
    pub fn flip_y(y_pdf: f32, page_height: f32) -> f32 {
        page_height - y_pdf
    }

    #[must_use]
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    #[must_use]
    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    /// `Area(B) = max(0, x1-x0) * max(0, y1-y0)`
    #[must_use]
    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.x0 <= self.x1 && self.y0 <= self.y1
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.area() <= f32::EPSILON
    }

    /// Intersection area `I(Ba, Bb)`.
    #[must_use]
    pub fn intersection_area(&self, other: &BBox) -> f32 {
        let ix0 = self.x0.max(other.x0);
        let iy0 = self.y0.max(other.y0);
        let ix1 = self.x1.min(other.x1);
        let iy1 = self.y1.min(other.y1);
        (ix1 - ix0).max(0.0) * (iy1 - iy0).max(0.0)
    }

    /// Intersection bbox, or `None` when disjoint.
    #[must_use]
    pub fn intersection(&self, other: &BBox) -> Option<BBox> {
        let ix0 = self.x0.max(other.x0);
        let iy0 = self.y0.max(other.y0);
        let ix1 = self.x1.min(other.x1);
        let iy1 = self.y1.min(other.y1);
        if ix1 > ix0 && iy1 > iy0 {
            Some(BBox::new(ix0, iy0, ix1, iy1))
        } else {
            None
        }
    }

    /// Smallest bbox containing both boxes.
    #[must_use]
    pub fn union(&self, other: &BBox) -> BBox {
        BBox::new(
            self.x0.min(other.x0),
            self.y0.min(other.y0),
            self.x1.max(other.x1),
            self.y1.max(other.y1),
        )
    }

    /// Union of many boxes; `None` when the iterator is empty.
    #[must_use]
    pub fn union_all<I: IntoIterator<Item = BBox>>(boxes: I) -> Option<BBox> {
        let mut iter = boxes.into_iter();
        let first = iter.next()?;
        Some(iter.fold(first, |acc, b| acc.union(&b)))
    }

    /// Overlap coefficient (Szymkiewicz–Simpson):
    /// `I(Ba,Bb) / min(Area(Ba), Area(Bb))`. Returns 0 when either is empty.
    #[must_use]
    pub fn overlap(&self, other: &BBox) -> f32 {
        let denom = self.area().min(other.area());
        if denom <= f32::EPSILON {
            return 0.0;
        }
        self.intersection_area(other) / denom
    }

    /// Intersection-over-union (Jaccard). Returns 0 when union is empty.
    #[must_use]
    pub fn iou(&self, other: &BBox) -> f32 {
        let inter = self.intersection_area(other);
        let denom = self.area() + other.area() - inter;
        if denom <= f32::EPSILON {
            return 0.0;
        }
        inter / denom
    }

    /// Vertical (1D) overlap ratio used for line merging:
    /// `overlap_y / min(ha, hb)`.
    #[must_use]
    pub fn vertical_overlap_ratio(&self, other: &BBox) -> f32 {
        let denom = self.height().min(other.height());
        if denom <= f32::EPSILON {
            return 0.0;
        }
        let overlap = (self.y1.min(other.y1) - self.y0.max(other.y0)).max(0.0);
        overlap / denom
    }

    /// Horizontal (1D) overlap ratio: `overlap_x / min(wa, wb)`.
    #[must_use]
    pub fn horizontal_overlap_ratio(&self, other: &BBox) -> f32 {
        let denom = self.width().min(other.width());
        if denom <= f32::EPSILON {
            return 0.0;
        }
        let overlap = (self.x1.min(other.x1) - self.x0.max(other.x0)).max(0.0);
        overlap / denom
    }

    /// Line-merge criterion from the spec: two spans share a text line when
    /// their vertical overlap ratio exceeds `threshold` (default 0.55).
    #[must_use]
    pub fn same_line(&self, other: &BBox, threshold: f32) -> bool {
        self.vertical_overlap_ratio(other) > threshold
    }

    /// Centroid `(cx, cy)`.
    #[must_use]
    pub fn centroid(&self) -> (f32, f32) {
        ((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }

    /// True when `(x, y)` lies inside (inclusive).
    #[must_use]
    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    /// True when `other` is fully inside `self`.
    #[must_use]
    pub fn contains(&self, other: &BBox) -> bool {
        other.x0 >= self.x0 && other.y0 >= self.y0 && other.x1 <= self.x1 && other.y1 <= self.y1
    }

    /// Expand by `pad` points on all sides.
    #[must_use]
    pub fn pad(&self, pad: f32) -> BBox {
        BBox::new(self.x0 - pad, self.y0 - pad, self.x1 + pad, self.y1 + pad)
    }

    /// Expand by `pad` points, clamped to `(0,0)-(max_w,max_h)`.
    #[must_use]
    pub fn pad_clamped(&self, pad: f32, max_w: f32, max_h: f32) -> BBox {
        BBox::new(
            (self.x0 - pad).max(0.0),
            (self.y0 - pad).max(0.0),
            (self.x1 + pad).min(max_w),
            (self.y1 + pad).min(max_h),
        )
    }

    /// Horizontal gap between boxes (0 when x-ranges overlap).
    #[must_use]
    pub fn h_gap(&self, other: &BBox) -> f32 {
        if self.x1 <= other.x0 {
            other.x0 - self.x1
        } else if other.x1 <= self.x0 {
            self.x0 - other.x1
        } else {
            0.0
        }
    }

    /// Vertical gap between boxes (0 when y-ranges overlap).
    #[must_use]
    pub fn v_gap(&self, other: &BBox) -> f32 {
        if self.y1 <= other.y0 {
            other.y0 - self.y1
        } else if other.y1 <= self.y0 {
            self.y0 - other.y1
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_of_unit_box() {
        let b = BBox::new(0.0, 0.0, 10.0, 5.0);
        assert!((b.area() - 50.0).abs() < 1e-6);
    }

    #[test]
    fn area_degenerate_is_zero() {
        assert_eq!(BBox::new(1.0, 1.0, 1.0, 5.0).area(), 0.0);
        assert_eq!(BBox::new(5.0, 5.0, 1.0, 1.0).area(), 16.0); // normalized
    }

    #[test]
    fn intersection_area_partial_overlap() {
        let a = BBox::new(0.0, 0.0, 10.0, 10.0);
        let b = BBox::new(5.0, 5.0, 15.0, 15.0);
        assert!((a.intersection_area(&b) - 25.0).abs() < 1e-6);
    }

    #[test]
    fn intersection_area_disjoint_is_zero() {
        let a = BBox::new(0.0, 0.0, 5.0, 5.0);
        let b = BBox::new(6.0, 6.0, 10.0, 10.0);
        assert_eq!(a.intersection_area(&b), 0.0);
        assert!(a.intersection(&b).is_none());
    }

    #[test]
    fn intersection_returns_box() {
        let a = BBox::new(0.0, 0.0, 10.0, 10.0);
        let b = BBox::new(2.0, 3.0, 8.0, 9.0);
        assert_eq!(a.intersection(&b), Some(BBox::new(2.0, 3.0, 8.0, 9.0)));
    }

    #[test]
    fn union_covers_both() {
        let a = BBox::new(0.0, 0.0, 5.0, 5.0);
        let b = BBox::new(3.0, 4.0, 10.0, 12.0);
        assert_eq!(a.union(&b), BBox::new(0.0, 0.0, 10.0, 12.0));
    }

    #[test]
    fn union_all_empty_is_none() {
        assert_eq!(BBox::union_all([]), None);
    }

    #[test]
    fn overlap_coefficient_containment_is_one() {
        let outer = BBox::new(0.0, 0.0, 10.0, 10.0);
        let inner = BBox::new(2.0, 2.0, 4.0, 4.0);
        assert!((outer.overlap(&inner) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn overlap_coefficient_partial() {
        let a = BBox::new(0.0, 0.0, 10.0, 10.0); // area 100
        let b = BBox::new(5.0, 0.0, 15.0, 10.0); // area 100, inter 50
        assert!((a.overlap(&b) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn overlap_empty_is_zero() {
        let a = BBox::zero();
        let b = BBox::new(0.0, 0.0, 5.0, 5.0);
        assert_eq!(a.overlap(&b), 0.0);
    }

    #[test]
    fn iou_identical_is_one() {
        let a = BBox::new(1.0, 2.0, 5.0, 6.0);
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn coordinate_flip() {
        // Page height 792: PDF y=100 (near bottom) -> top y=692.
        assert!((BBox::flip_y(100.0, 792.0) - 692.0).abs() < 1e-6);
        let b = BBox::from_pdf_coords(10.0, 100.0, 50.0, 120.0, 792.0);
        assert_eq!(b, BBox::new(10.0, 672.0, 50.0, 692.0));
    }

    #[test]
    fn same_line_criterion() {
        // Two spans on the same baseline: full vertical overlap.
        let a = BBox::new(0.0, 100.0, 50.0, 112.0);
        let b = BBox::new(60.0, 100.0, 110.0, 112.0);
        assert!(a.same_line(&b, 0.55));
        // Adjacent lines: no vertical overlap.
        let c = BBox::new(0.0, 114.0, 50.0, 126.0);
        assert!(!a.same_line(&c, 0.55));
    }

    #[test]
    fn pad_and_clamp() {
        let b = BBox::new(10.0, 10.0, 20.0, 20.0);
        assert_eq!(b.pad(2.0), BBox::new(8.0, 8.0, 22.0, 22.0));
        let c = BBox::new(1.0, 1.0, 5.0, 5.0).pad_clamped(8.0, 100.0, 100.0);
        assert_eq!(c, BBox::new(0.0, 0.0, 13.0, 13.0));
    }

    #[test]
    fn contains_point_and_centroid() {
        let b = BBox::new(0.0, 0.0, 10.0, 20.0);
        assert_eq!(b.centroid(), (5.0, 10.0));
        assert!(b.contains_point(5.0, 10.0));
        assert!(!b.contains_point(11.0, 10.0));
    }
}
