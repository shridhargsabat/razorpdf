//! Sub-page renderer: region math is live; raster rendering + WebP
//! encoding land in Phase 3.

use anyhow::Result;

use crate::models::BBox;

/// Pixel rectangle `(x, y, w, h)` for a region on a rendered page image.
///
/// ```text
/// px0 = floor(x0 / Wpage * Wimg), py0 = floor(y0 / Hpage * Himg)
/// ```
#[must_use]
pub fn region_to_pixels(
    region: &BBox,
    page_w: f32,
    page_h: f32,
    img_w: u32,
    img_h: u32,
) -> (u32, u32, u32, u32) {
    let px0 = ((region.x0 / page_w * img_w as f32).floor() as u32).min(img_w);
    let py0 = ((region.y0 / page_h * img_h as f32).floor() as u32).min(img_h);
    let px1 = ((region.x1 / page_w * img_w as f32).ceil() as u32).min(img_w);
    let py1 = ((region.y1 / page_h * img_h as f32).ceil() as u32).min(img_h);
    (px0, py0, px1.saturating_sub(px0), py1.saturating_sub(py0))
}

/// Crop a region and encode it as WebP (Phase 3).
pub fn crop_region_to_webp(
    _img_w: u32,
    _img_h: u32,
    _region: &BBox,
    _page_w: f32,
    _page_h: f32,
) -> Result<Vec<u8>> {
    anyhow::bail!("sub-page WebP cropper ships in Phase 3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_math_matches_spec() {
        // Letter page at 150 DPI => 1275x1650 px.
        let r = BBox::new(72.0, 120.0, 480.0, 220.0);
        let (x, y, w, h) = region_to_pixels(&r, 612.0, 792.0, 1275, 1650);
        assert_eq!(x, (72.0f32 / 612.0 * 1275.0).floor() as u32);
        assert_eq!(y, (120.0f32 / 792.0 * 1650.0).floor() as u32);
        assert!(w > 800 && h > 200);
    }
}
