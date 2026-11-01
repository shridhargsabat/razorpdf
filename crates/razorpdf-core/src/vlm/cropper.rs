//! Sub-page renderer: synthetic raster canvas + WebP crop/encode.
//!
//! Until the pdfium backend lands, regions crop from a deterministic
//! synthetic canvas (span bars + vector strokes + image placeholders) —
//! layout-faithful and sufficient for offline MockVLM tests.

use anyhow::{Context, Result};
use image::{codecs::webp::WebPEncoder, Rgb, RgbImage};
use serde::{Deserialize, Serialize};

use crate::models::{BBox, EmbeddedImage, ImageCodec, PixelLayout, RawPageElements};

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

/// Largest synthetic canvas in pixels (16 MP ≈ 48 MiB RGB): hostile
/// MediaBox dimensions (e.g. 1e7pt, which parses fine) would otherwise
/// abort on a terabyte-scale `RgbImage` allocation. Oversized pages are
/// scaled down preserving aspect ratio; VLM crops stay proportional.
const MAX_CANVAS_PIXELS: u64 = 16_777_216;

/// Render a deterministic synthetic canvas of the page at `dpi`.
///
/// White background; text spans become gray bars (darker when bold),
/// vectors become near-black strokes, image bboxes become boxed gray fills
/// — overlaid with real decoded pixels where the image encoding is
/// supported (JPEG, Flate/Raw RGB+Gray).
#[must_use]
pub fn render_synthetic_canvas(page: &RawPageElements, dpi: u32) -> RgbImage {
    let mut scale = dpi.max(1) as f32 / 72.0;
    let mut w = (page.width * scale).ceil().max(1.0);
    let mut h = (page.height * scale).ceil().max(1.0);
    let area = (w as f64) * (h as f64);
    if area > MAX_CANVAS_PIXELS as f64 {
        let shrink = (MAX_CANVAS_PIXELS as f64 / area).sqrt() as f32;
        scale *= shrink;
        w = (page.width * scale).ceil().max(1.0);
        h = (page.height * scale).ceil().max(1.0);
    }
    let (w, h) = (w as u32, h as u32);
    let mut img = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));

    for bbox in &page.image_bboxes {
        let (x0, y0, x1, y1) = rect_px(bbox, scale, w, h);
        fill_rect(&mut img, x0, y0, x1, y1, Rgb([205, 205, 205]));
        stroke_rect(&mut img, x0, y0, x1, y1, Rgb([120, 120, 120]));
    }
    // Real pixels paint over their placeholders; decode failures keep gray.
    for embedded in &page.embedded_images {
        paste_embedded(&mut img, embedded, scale);
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

/// Page-canvas source for VLM crops.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanvasSource {
    /// Deterministic synthetic canvas (spans/vectors/decoded images).
    /// Stable everywhere: the default for tests and benchmarks.
    #[default]
    #[serde(rename = "synthetic")]
    Synthetic,
    /// `pdftoppm` when present on PATH, else the synthetic canvas.
    #[serde(rename = "auto")]
    Auto,
}

/// Render a page canvas per `source`. Infallible: any rasterizer failure
/// falls back to the synthetic canvas.
#[must_use]
pub fn render_page_canvas(
    pdf: &[u8],
    page: &RawPageElements,
    dpi: u32,
    source: CanvasSource,
) -> RgbImage {
    if source == CanvasSource::Auto {
        if let Some(raster) = render_page_via_pdftoppm(pdf, page.page_index, dpi) {
            return raster;
        }
    }
    render_synthetic_canvas(page, dpi)
}

/// Max decoded pixels per image or raster page (bomb guard; 8k × 8k).
/// Mirrors the extractor's collection guard (defense in depth: embedded
/// images are public data and may come from anywhere).
const MAX_IMAGE_PIXELS: u64 = 67_108_864;

/// Render one page to RGB pixels via `pdftoppm` (poppler). Spills `pdf` to
/// a temp file, rasterizes the single page to PNG, decodes it. `None` when
/// pdftoppm is missing, times out (30 s), or fails.
pub fn render_page_via_pdftoppm(pdf: &[u8], page_index: usize, dpi: u32) -> Option<RgbImage> {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let tag = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    let input = dir.join(format!("razorpdf-{pid}.{tag}.pdf"));
    let prefix = dir.join(format!("razorpdf-{pid}.{tag}-page"));
    // pdftoppm appends `.png` to the whole prefix.
    let out_png = std::path::PathBuf::from(format!("{}.png", prefix.display()));
    struct Cleanup {
        paths: Vec<std::path::PathBuf>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for path in &self.paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let _cleanup = Cleanup {
        paths: vec![input.clone(), out_png.clone()],
    };
    // O_EXCL on both paths: the pid.tag names are predictable inside the
    // world-writable temp dir, so a planted symlink must fail closed
    // instead of redirecting our bytes (CWE-59).
    {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&input)
            .and_then(|mut f| f.write_all(pdf))
            .ok()?;
    }
    // Reserve the output path before pdftoppm runs so it cannot be a
    // planted symlink either (pdftoppm truncates and overwrites it).
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&out_png)
        .ok()?;
    let page_no = page_index.checked_add(1)?; // pdftoppm is 1-based
    let mut child = Command::new("pdftoppm")
        .args([
            "-png",
            "-r",
            &dpi.max(1).to_string(),
            "-f",
            &page_no.to_string(),
            "-l",
            &page_no.to_string(),
            "-singlefile",
        ])
        .arg(&input)
        .arg(&prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?; // missing binary → None
                // Bounded wait: poll 30 s, then kill (never hang the VLM path).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait(); // don't leak a zombie on the error path
                return None;
            }
        }
    }
    let png = std::fs::read(&out_png).ok()?;
    let decoded = image::load_from_memory(&png).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    if u64::from(w) * u64::from(h) > MAX_IMAGE_PIXELS {
        return None;
    }
    Some(decoded.to_rgb8())
}

/// Paste an embedded image's decoded pixels over its placeholder rect
/// (axis-aligned; rotation is approximated by the bbox union, which is
/// what VLM context needs). No-op on any decode failure.
fn paste_embedded(img: &mut RgbImage, embedded: &EmbeddedImage, scale: f32) {
    use image::GenericImage as _;
    let (w, h) = (img.width(), img.height());
    let (x0, y0, x1, y1) = rect_px(&embedded.bbox, scale, w, h);
    let (rw, rh) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if rw == 0 || rh == 0 {
        return;
    }
    let Some(decoded) = decode_embedded(embedded) else {
        return;
    };
    let resized = image::imageops::resize(&decoded, rw, rh, image::imageops::FilterType::Triangle);
    let _ = img.copy_from(&resized, x0, y0);
}

/// Decode an embedded image to RGB pixels. `None` on any failure or guard
/// trip (caller keeps the gray placeholder).
fn decode_embedded(image: &EmbeddedImage) -> Option<RgbImage> {
    if image.width == 0 || image.height == 0 {
        return None;
    }
    if u64::from(image.width) * u64::from(image.height) > MAX_IMAGE_PIXELS {
        return None;
    }
    let coded: &[u8] = &image.bytes;
    match image.codec {
        ImageCodec::Jpeg => decode_jpeg(coded),
        ImageCodec::Flate => {
            let expected = expected_len(image.layout, image.width, image.height)?;
            let raw = inflate_exact(coded, expected)?;
            samples_to_rgb(&raw, image.layout, image.width, image.height)
        }
        ImageCodec::Raw => {
            let expected = expected_len(image.layout, image.width, image.height)?;
            if coded.len() != expected {
                return None;
            }
            samples_to_rgb(coded, image.layout, image.width, image.height)
        }
    }
}

/// Exact inflated/raw byte count for a layout (checked arithmetic).
/// The exactness doubles as a predictor guard: PNG-predictor streams carry
/// an extra filter byte per row and fail here → placeholder, not garbage.
fn expected_len(layout: PixelLayout, w: u32, h: u32) -> Option<usize> {
    let (w, h) = (w as usize, h as usize);
    match layout {
        PixelLayout::Rgb8 => w.checked_mul(h)?.checked_mul(3),
        PixelLayout::Gray8 => w.checked_mul(h),
        PixelLayout::Gray1 => w.checked_add(7)?.checked_div(8)?.checked_mul(h),
    }
}

/// Inflate zlib bytes, accepting only the exact expected length (bounded
/// growth: one byte of slack to detect over-long streams).
fn inflate_exact(coded: &[u8], expected: usize) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut decoder = flate2::read::ZlibDecoder::new(coded);
    let mut buf = Vec::new();
    decoder
        .by_ref()
        .take(expected as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() != expected {
        return None;
    }
    Some(buf)
}

/// Sample bytes → RGB. PDF image rows run top-to-bottom like the canvas.
fn samples_to_rgb(samples: &[u8], layout: PixelLayout, w: u32, h: u32) -> Option<RgbImage> {
    match layout {
        PixelLayout::Rgb8 => RgbImage::from_raw(w, h, samples.to_vec()),
        PixelLayout::Gray8 => {
            let gray = image::GrayImage::from_raw(w, h, samples.to_vec())?;
            Some(image::DynamicImage::ImageLuma8(gray).to_rgb8())
        }
        PixelLayout::Gray1 => {
            let stride = (w as usize).div_ceil(8);
            let mut rgb = RgbImage::new(w, h);
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let byte = *samples.get(y * stride + x / 8)?;
                    let bit = (byte >> (7 - (x % 8))) & 1;
                    let v = if bit == 1 { 255u8 } else { 0u8 };
                    rgb.put_pixel(x as u32, y as u32, Rgb([v, v, v]));
                }
            }
            Some(rgb)
        }
    }
}

/// Decode JPEG bytes after a header-only dimension pre-check (bomb guard).
fn decode_jpeg(coded: &[u8]) -> Option<RgbImage> {
    use std::io::Cursor;
    let dims = image::codecs::jpeg::JpegDecoder::new(Cursor::new(coded))
        .ok()
        .map(|decoder| {
            use image::ImageDecoder as _;
            decoder.dimensions()
        })?;
    if u64::from(dims.0) * u64::from(dims.1) > MAX_IMAGE_PIXELS {
        return None;
    }
    image::load_from_memory(coded).ok().map(|d| d.to_rgb8())
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
            embedded_images: Vec::new(),
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

    fn embedded(
        codec: ImageCodec,
        layout: PixelLayout,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    ) -> EmbeddedImage {
        EmbeddedImage {
            bbox: BBox::zero(),
            width,
            height,
            codec,
            layout,
            bytes,
        }
    }

    fn flate_of(raw: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(raw).expect("flate");
        enc.finish().expect("finish")
    }

    #[test]
    fn flate_rgb_decodes_exact() {
        let raw = vec![
            255, 0, 0, 0, 255, 0, // R G
            0, 0, 255, 255, 255, 255, // B W
        ];
        let img = embedded(ImageCodec::Flate, PixelLayout::Rgb8, 2, 2, flate_of(&raw));
        let rgb = decode_embedded(&img).expect("decode");
        assert_eq!(rgb.get_pixel(0, 0), &Rgb([255, 0, 0]));
        assert_eq!(rgb.get_pixel(1, 0), &Rgb([0, 255, 0]));
        assert_eq!(rgb.get_pixel(0, 1), &Rgb([0, 0, 255]));
        assert_eq!(rgb.get_pixel(1, 1), &Rgb([255, 255, 255]));
    }

    #[test]
    fn gray1_bits_expand_msb_first() {
        // 0b10110000 → W B W W B B B B.
        let img = embedded(ImageCodec::Raw, PixelLayout::Gray1, 8, 1, vec![0b1011_0000]);
        let rgb = decode_embedded(&img).expect("gray1");
        let (w, b) = (Rgb([255, 255, 255]), Rgb([0, 0, 0]));
        assert_eq!(rgb.get_pixel(0, 0), &w);
        assert_eq!(rgb.get_pixel(1, 0), &b);
        assert_eq!(rgb.get_pixel(2, 0), &w);
        assert_eq!(rgb.get_pixel(3, 0), &w);
        assert_eq!(rgb.get_pixel(4, 0), &b);
        assert_eq!(rgb.get_pixel(7, 0), &b);
    }

    #[test]
    fn corrupt_and_predictor_streams_fall_back() {
        // Garbage flate → None.
        let bad = embedded(ImageCodec::Flate, PixelLayout::Rgb8, 2, 2, vec![1, 2, 3, 4]);
        assert!(decode_embedded(&bad).is_none());
        // Predictor-shaped (filter byte per row): 6 bytes ≠ 4 → None.
        let pred = embedded(
            ImageCodec::Flate,
            PixelLayout::Gray8,
            2,
            2,
            flate_of(&[0, 1, 2, 0, 3, 4]),
        );
        assert!(decode_embedded(&pred).is_none());
        // Truncated JPEG → None.
        let jpg = embedded(
            ImageCodec::Jpeg,
            PixelLayout::Rgb8,
            4,
            4,
            vec![0xFF, 0xD8, 0x00],
        );
        assert!(decode_embedded(&jpg).is_none());
    }

    #[test]
    fn canvas_pastes_real_pixels() {
        let pdf = crate::test_fixtures::image_pixels_pdf();
        let page = crate::extractor::extract_page(&pdf, 0).expect("extract");
        let img = render_synthetic_canvas(&page, 72);
        // ImRGB rect x 72..172, y 342..392: R / W / C quadrants.
        assert_eq!(img.get_pixel(74, 344), &Rgb([255, 0, 0]));
        assert_eq!(img.get_pixel(170, 344), &Rgb([255, 255, 255]));
        assert_eq!(img.get_pixel(74, 390), &Rgb([0, 255, 255]));
        // JPEG red block center (lossy tolerance).
        let px = img.get_pixel(320, 272);
        assert!(
            px[0] > 180 && px[1] < 120 && px[2] < 120,
            "jpeg red: {px:?}"
        );
        // Raw gray ramp: dark corner → bright corner.
        let dark = img.get_pixel(73, 173);
        let bright = img.get_pixel(91, 191);
        assert!(dark[0] < 40, "dark: {dark:?}");
        assert!(bright[0] > 215, "bright: {bright:?}");
        // CMYK placeholder stays gray.
        assert_eq!(img.get_pixel(310, 182), &Rgb([205, 205, 205]));
    }

    #[test]
    fn pdftoppm_rasterizes_when_present() {
        let pdf = crate::test_fixtures::single_col_clean_pdf();
        let Some(img) = render_page_via_pdftoppm(&pdf, 0, 72) else {
            // Absent binary: verify that's really why (spawn failure).
            assert!(
                std::process::Command::new("pdftoppm").output().is_err(),
                "pdftoppm exists but render failed"
            );
            return;
        };
        assert_eq!((img.width(), img.height()), (612, 792));
        let nonwhite = img.pixels().filter(|p| **p != Rgb([255, 255, 255])).count();
        assert!(nonwhite > 1000, "expected text pixels, got {nonwhite}");
    }
}
