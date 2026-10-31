//! Sub-page renderer: synthetic raster canvas + WebP crop/encode.
//!
//! Until the pdfium backend lands, regions crop from a deterministic
//! synthetic canvas (span bars + vector strokes + image placeholders) —
//! layout-faithful and sufficient for offline MockVLM tests.

use anyhow::{Context, Result};
use image::{codecs::webp::WebPEncoder, Rgb, RgbImage};

use crate::models::{BBox, RawPageElements};

/// Pixel rectangle `(x, y, w, h)` for a region on a rendered page image.
///
/// ```text
/// px0 = floor(x0 / Wpage * Wimg), py0 = floor(y0 / Hpage * Himg)
/// ```
///
/// A small epsilon absorbs float dust so exact point boundaries (e.g.
/// `111/792*792`) do not spill into a neighboring pixel row.
#[must_use]
pub fn region_to_pixels(
    region: &BBox,
    page_w: f32,
    page_h: f32,
    img_w: u32,
    img_h: u32,
) -> (u32, u32, u32, u32) {
    const EPS: f32 = 1e-4;
    let px0 = ((region.x0 / page_w * img_w as f32 + EPS).floor() as u32).min(img_w);
    let py0 = ((region.y0 / page_h * img_h as f32 + EPS).floor() as u32).min(img_h);
    let px1 = ((region.x1 / page_w * img_w as f32 - EPS).ceil() as u32).min(img_w);
    let py1 = ((region.y1 / page_h * img_h as f32 - EPS).ceil() as u32).min(img_h);
    (px0, py0, px1.saturating_sub(px0), py1.saturating_sub(py0))
}

/// Render a deterministic synthetic canvas of the page at `dpi`.
///
/// White background; text spans become gray bars (darker when bold),
/// vectors become near-black strokes, image bboxes become boxed gray fills.
#[must_use]
pub fn render_synthetic_canvas(page: &RawPageElements, dpi: u32) -> RgbImage {
    let scale = dpi.max(1) as f32 / 72.0;
    let w = (page.width * scale).ceil().max(1.0) as u32;
    let h = (page.height * scale).ceil().max(1.0) as u32;
    let mut img = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));

    for bbox in &page.image_bboxes {
        let (x0, y0, x1, y1) = rect_px(bbox, scale, w, h);
        fill_rect(&mut img, x0, y0, x1, y1, Rgb([205, 205, 205]));
        stroke_rect(&mut img, x0, y0, x1, y1, Rgb([120, 120, 120]));
    }
    for span in &page.spans {
        let shade = if span.is_bold { 45u8 } else { 95u8 };
        let (x0, y0, x1, y1) = rect_px(&span.bbox, scale, w, h);
        fill_rect(&mut img, x0, y0, x1, y1, Rgb([shade, shade, shade]));
    }
    for seg in &page.vectors {
        let x0 = ((seg.x0.min(seg.x1) * scale).floor() as u32).min(w.saturating_sub(1));
        let x1 = ((seg.x0.max(seg.x1) * scale).ceil() as u32).min(w.saturating_sub(1));
        let y0 = ((seg.y0.min(seg.y1) * scale).floor() as u32).min(h.saturating_sub(1));
        let y1 = ((seg.y0.max(seg.y1) * scale).ceil() as u32).min(h.saturating_sub(1));
        let thick = (seg.stroke_width * scale).round().max(1.0) as u32;
        draw_line(&mut img, x0, y0, x1, y1, thick.min(4), Rgb([25, 25, 25]));
    }
    img
}

/// Crop a region from a page-sized image (at least 1×1 px).
#[must_use]
pub fn crop_region(img: &RgbImage, region: &BBox, page_w: f32, page_h: f32) -> RgbImage {
    let (x, y, w, h) = region_to_pixels(region, page_w, page_h, img.width(), img.height());
    image::imageops::crop_imm(img, x, y, w.max(1), h.max(1)).to_image()
}

/// Encode an image as WebP.
///
/// Currently lossless (`image` 0.25 has no lossy encoder; a `libwebp`
/// backend can honor `quality` later). Synthetic canvases are flat fills
/// and compress well losslessly.
pub fn encode_webp(img: &RgbImage, quality: u8) -> Result<Vec<u8>> {
    debug_assert!(quality <= 100);
    let mut buf = Vec::new();
    WebPEncoder::new_lossless(&mut buf)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .context("webp encode")?;
    Ok(buf)
}

/// Crop a region from a page-sized image and encode it as WebP.
pub fn crop_region_to_webp(
    img: &RgbImage,
    region: &BBox,
    page_w: f32,
    page_h: f32,
    quality: u8,
) -> Result<Vec<u8>> {
    encode_webp(&crop_region(img, region, page_w, page_h), quality)
}

/// Crop a region and encode it as a base64 WebP data URI for VLM requests.
pub fn crop_region_to_data_uri(
    img: &RgbImage,
    region: &BBox,
    page_w: f32,
    page_h: f32,
    quality: u8,
) -> Result<String> {
    use base64::Engine as _;
    let webp = crop_region_to_webp(img, region, page_w, page_h, quality)?;
    Ok(format!(
        "data:image/webp;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&webp)
    ))
}

/// Clamped pixel rect for a point-space bbox.
fn rect_px(bbox: &BBox, scale: f32, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let x0 = ((bbox.x0 * scale).floor() as u32).min(w.saturating_sub(1));
    let y0 = ((bbox.y0 * scale).floor() as u32).min(h.saturating_sub(1));
    let x1 = ((bbox.x1 * scale).ceil() as u32).min(w).max(x0 + 1);
    let y1 = ((bbox.y1 * scale).ceil() as u32).min(h).max(y0 + 1);
    (x0, y0, x1, y1)
}

fn fill_rect(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, color: Rgb<u8>) {
    for y in y0..y1.min(img.height()) {
        for x in x0..x1.min(img.width()) {
            img.put_pixel(x, y, color);
        }
    }
}

fn stroke_rect(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, color: Rgb<u8>) {
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (w, h) = (img.width(), img.height());
    for x in x0..x1.min(w) {
        if y0 < h {
            img.put_pixel(x, y0, color);
        }
        if y1.saturating_sub(1) < h {
            img.put_pixel(x, y1 - 1, color);
        }
    }
    for y in y0..y1.min(h) {
        if x0 < w {
            img.put_pixel(x0, y, color);
        }
        if x1.saturating_sub(1) < w {
            img.put_pixel(x1 - 1, y, color);
        }
    }
}

/// Bresenham line with square-brush thickness.
fn draw_line(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, thick: u32, color: Rgb<u8>) {
    let (mut x0, mut y0) = (x0 as i64, y0 as i64);
    let (x1, y1) = (x1 as i64, y1 as i64);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let r = thick as i64 / 2;
    loop {
        for oy in -r..=r {
            for ox in -r..=r {
                let (px, py) = (x0 + ox, y0 + oy);
                if px >= 0 && py >= 0 {
                    let (px, py) = (px as u32, py as u32);
                    if px < img.width() && py < img.height() {
                        img.put_pixel(px, py, color);
                    }
                }
            }
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{TextSpan, VectorSegment};

    fn span(x0: f32, y0: f32, x1: f32, y1: f32, bold: bool) -> TextSpan {
        TextSpan {
            text: "x".to_string(),
            bbox: BBox::new(x0, y0, x1, y1),
            font_name: "Helvetica".to_string(),
            font_size: 11.0,
            is_bold: bold,
            is_italic: false,
            is_monospace: false,
        }
    }

    fn sample_page() -> RawPageElements {
        RawPageElements {
            page_index: 0,
            width: 612.0,
            height: 792.0,
            spans: vec![span(72.0, 100.0, 200.0, 111.0, false)],
            vectors: vec![VectorSegment {
                x0: 72.0,
                y0: 150.0,
                x1: 300.0,
                y1: 150.0,
                stroke_width: 1.0,
            }],
            image_bboxes: Vec::new(),
            filled_rects: Vec::new(),
        }
    }

    #[test]
    fn pixel_math_matches_spec() {
        // Letter page at 150 DPI => 1275x1650 px.
        let r = BBox::new(72.0, 120.0, 480.0, 220.0);
        let (x, y, w, h) = region_to_pixels(&r, 612.0, 792.0, 1275, 1650);
        // 72/612*1275 = 150, 120/792*1650 = 250 (±1px float tolerance).
        assert!((x as f32 - 150.0).abs() <= 1.0, "x={x}");
        assert!((y as f32 - 250.0).abs() <= 1.0, "y={y}");
        assert!(w > 800 && h > 200);
    }

    #[test]
    fn synthetic_canvas_draws_bars_and_rules() {
        let img = render_synthetic_canvas(&sample_page(), 72);
        assert_eq!((img.width(), img.height()), (612, 792));
        // Inside the span bar: gray.
        assert_eq!(img.get_pixel(100, 105), &Rgb([95, 95, 95]));
        // On the vector rule: near-black.
        assert_eq!(img.get_pixel(150, 150), &Rgb([25, 25, 25]));
        // Untouched corner: white.
        assert_eq!(img.get_pixel(10, 10), &Rgb([255, 255, 255]));
    }

    #[test]
    fn crop_matches_region_math() {
        let img = render_synthetic_canvas(&sample_page(), 72);
        let region = BBox::new(72.0, 100.0, 200.0, 111.0);
        let crop = crop_region(&img, &region, 612.0, 792.0);
        assert_eq!((crop.width(), crop.height()), (128, 11));
        assert_eq!(crop.get_pixel(5, 5), &Rgb([95, 95, 95]));
    }

    #[test]
    fn webp_bytes_and_data_uri_shape() {
        let img = render_synthetic_canvas(&sample_page(), 72);
        let region = BBox::new(0.0, 0.0, 612.0, 792.0);
        let webp = crop_region_to_webp(&img, &region, 612.0, 792.0, 80).expect("webp");
        assert!(webp.len() > 12);
        assert_eq!(&webp[0..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");
        let uri = crop_region_to_data_uri(&img, &region, 612.0, 792.0, 80).expect("uri");
        assert!(uri.starts_with("data:image/webp;base64,"));
    }
}
