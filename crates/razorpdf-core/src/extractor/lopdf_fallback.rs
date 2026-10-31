//! Pure-Rust PDF extractor (works anywhere, no C libraries).
//!
//! Parses content-stream text operators (`BT/ET`, `Tf`, `Tm`, `Tj`, `TJ`)
//! and vector path operators (`m`, `l`, `re`, `w`), flipping all coordinates
//! to top-left origin on emission.

use anyhow::{Context, Result};
use lopdf::{content::Content, Document, Object, ObjectId};
use std::collections::BTreeMap;
use std::io::Read;

use crate::models::{BBox, RawPageElements, TextSpan, VectorSegment};

/// Extract every page of a PDF into raw elements.
///
/// Page inputs are gathered sequentially (cheap borrows), then content
/// streams are decoded and walked in parallel with rayon.
pub fn extract_document(pdf: &[u8], _source_path: &str) -> Result<Vec<RawPageElements>> {
    use rayon::prelude::*;

    let doc = Document::load_mem(pdf).context("parse pdf bytes")?;
    let pages = doc.get_pages();
    let mut order: Vec<(u32, ObjectId)> = pages.into_iter().collect();
    order.sort_by_key(|(n, _)| *n);

    let mut inputs = Vec::with_capacity(order.len());
    for (page_index, (_, id)) in order.iter().enumerate() {
        let page_obj = doc.get_object(*id).context("page object")?;
        let page_dict = resolve_dict(&doc, page_obj).context("page dict")?;
        let (ml, mb, mr, mt) = media_box(page_dict);
        let fonts = font_map(&doc, page_dict).unwrap_or_default();
        let content_bytes = page_content(&doc, page_dict).context("page contents")?;
        inputs.push(PageInput {
            page_index,
            origin_x: ml,
            top_y: mt,
            width: (mr - ml).max(1.0),
            height: (mt - mb).max(1.0),
            fonts,
            content_bytes,
        });
    }

    let mut out: Vec<RawPageElements> = inputs
        .into_par_iter()
        .map(|input| {
            let content = Content::decode(&input.content_bytes).context("decode content stream")?;
            let mut state = WalkState::new(input.origin_x, input.top_y, input.fonts);
            for op in &content.operations {
                state.apply(op);
            }
            Ok(RawPageElements {
                page_index: input.page_index,
                width: input.width,
                height: input.height,
                spans: state.spans,
                vectors: state.vectors,
                image_bboxes: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    out.sort_by_key(|p| p.page_index);
    Ok(out)
}

/// Owned per-page extractor inputs (all `Send` for rayon).
struct PageInput {
    page_index: usize,
    origin_x: f32,
    top_y: f32,
    width: f32,
    height: f32,
    fonts: BTreeMap<String, FontInfo>,
    content_bytes: Vec<u8>,
}

/// Extract a single page (0-based `page_index`) from a PDF.
pub fn extract_page(pdf: &[u8], page_index: usize) -> Result<RawPageElements> {
    let pages = extract_document(pdf, "")?;
    pages
        .into_iter()
        .nth(page_index)
        .context("page index out of range")
}

// ---------------------------------------------------------------------------
// Page resource helpers
// ---------------------------------------------------------------------------

/// Follow a possible `Reference` to the underlying object.
fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> Result<&'a Object> {
    match obj {
        Object::Reference(id) => doc.get_object(*id).context("dangling reference"),
        other => Ok(other),
    }
}

fn resolve_dict<'a>(doc: &'a Document, obj: &'a Object) -> Result<&'a lopdf::Dictionary> {
    match resolve(doc, obj)? {
        Object::Dictionary(d) => Ok(d),
        _ => anyhow::bail!("expected dictionary"),
    }
}

/// MediaBox `[l b r t]`; defaults to US Letter on malformed input.
fn media_box(page: &lopdf::Dictionary) -> (f32, f32, f32, f32) {
    let fallback = (0.0, 0.0, 612.0, 792.0);
    let Ok(arr) = page.get(b"MediaBox") else {
        return fallback;
    };
    let Object::Array(items) = arr else {
        return fallback;
    };
    if items.len() != 4 {
        return fallback;
    }
    let nums: Vec<f32> = items.iter().filter_map(obj_f32).collect();
    if nums.len() != 4 {
        return fallback;
    }
    (nums[0], nums[1], nums[2], nums[3])
}

#[derive(Debug, Clone, Default)]
struct FontInfo {
    name: String,
    is_bold: bool,
    is_italic: bool,
    is_monospace: bool,
}

/// Map content-stream font refs (`F1`) to font metadata.
fn font_map(doc: &Document, page: &lopdf::Dictionary) -> Result<BTreeMap<String, FontInfo>> {
    let mut out = BTreeMap::new();
    let resources = page
        .get(b"Resources")
        .map_err(|_| anyhow::anyhow!("no resources"))?;
    let resources = resolve_dict(doc, resources)?;
    let font_dict = resources
        .get(b"Font")
        .map_err(|_| anyhow::anyhow!("no fonts"))?;
    let font_dict = resolve_dict(doc, font_dict)?;
    for (key, val) in font_dict.iter() {
        let font_ref = String::from_utf8_lossy(key).into_owned();
        let Ok(font_obj) = resolve(doc, val) else {
            continue;
        };
        let Object::Dictionary(fd) = font_obj else {
            continue;
        };
        let base = fd
            .get(b"BaseFont")
            .ok()
            .map(obj_name_string)
            .unwrap_or_default();
        let lower = base.to_lowercase();
        out.insert(
            font_ref,
            FontInfo {
                name: if base.is_empty() {
                    "Unknown".to_string()
                } else {
                    base
                },
                is_bold: lower.contains("bold") || lower.contains("black"),
                is_italic: lower.contains("italic") || lower.contains("oblique"),
                is_monospace: lower.contains("courier")
                    || lower.contains("mono")
                    || lower.contains("consolas")
                    || lower.contains("menlo"),
            },
        );
    }
    Ok(out)
}

/// Concatenate (and decompress when needed) a page's content streams.
fn page_content(doc: &Document, page: &lopdf::Dictionary) -> Result<Vec<u8>> {
    let contents = page.get(b"Contents").context("no contents")?;
    match resolve(doc, contents)? {
        Object::Stream(s) => stream_bytes(s),
        Object::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                match resolve(doc, item)? {
                    Object::Stream(s) => out.extend_from_slice(&stream_bytes(s)?),
                    _ => anyhow::bail!("contents array item is not a stream"),
                }
            }
            Ok(out)
        }
        _ => anyhow::bail!("unsupported contents type"),
    }
}

/// Raw bytes of a content stream, decoded through its full `/Filter` chain
/// in order (e.g. `[ /ASCII85Decode /FlateDecode ]` as emitted by reportlab).
fn stream_bytes(stream: &lopdf::Stream) -> Result<Vec<u8>> {
    let mut data = stream.content.clone();
    for filter in filters_of(&stream.dict) {
        match filter.as_str() {
            "FlateDecode" | "Fl" => {
                let mut decoder = flate2::read::ZlibDecoder::new(&data[..]);
                let mut buf = Vec::new();
                decoder
                    .read_to_end(&mut buf)
                    .context("inflate content stream")?;
                data = buf;
            }
            "ASCII85Decode" | "A85" => {
                data = ascii85_decode(&data).context("ascii85 content stream")?;
            }
            other => anyhow::bail!("unsupported stream filter: {other}"),
        }
    }
    Ok(data)
}

/// Filter names in decode order (`/Filter` Name or Array thereof).
fn filters_of(dict: &lopdf::Dictionary) -> Vec<String> {
    let Ok(filter) = dict.get(b"Filter") else {
        return Vec::new();
    };
    match filter {
        Object::Name(name) => vec![String::from_utf8_lossy(name).into_owned()],
        Object::Array(items) => items
            .iter()
            .filter_map(|f| match f {
                Object::Name(name) => Some(String::from_utf8_lossy(name).into_owned()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Decode Adobe ASCII85 (`z` short form, `y` space form omitted per PDF spec
/// — PDF uses `z` only; whitespace ignored; `<~`/`~>` markers stripped).
fn ascii85_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut input = data;
    if let Some(stripped) = input.strip_prefix(b"<~") {
        input = stripped;
    }
    if let Some(end) = input.windows(2).position(|w| w == b"~>") {
        input = &input[..end];
    }
    let mut out = Vec::with_capacity(input.len());
    let mut group: [u64; 5] = [0; 5];
    let mut len = 0usize;
    let flush = |group: &[u64; 5], len: usize, out: &mut Vec<u8>| {
        let mut value = 0u64;
        for (i, digit) in group.iter().enumerate() {
            let digit = if i < len { *digit } else { 84 };
            value = value * 85 + digit;
        }
        let bytes = (value as u32).to_be_bytes();
        let emit = if len == 5 { 4 } else { len.saturating_sub(1) };
        out.extend_from_slice(&bytes[..emit]);
    };
    for &byte in input {
        if byte.is_ascii_whitespace() || byte == 0 {
            continue;
        }
        if byte == b'z' {
            if len != 0 {
                anyhow::bail!("stray ASCII85 'z' inside group");
            }
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&byte) {
            anyhow::bail!("invalid ASCII85 byte: {byte:#x}");
        }
        group[len] = u64::from(byte - b'!');
        len += 1;
        if len == 5 {
            flush(&group, 5, &mut out);
            len = 0;
        }
    }
    if len == 1 {
        anyhow::bail!("dangling ASCII85 group");
    }
    if len > 0 {
        flush(&group, len, &mut out);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Content-stream walker
// ---------------------------------------------------------------------------

struct WalkState {
    origin_x: f32,
    top_y: f32,
    fonts: BTreeMap<String, FontInfo>,
    font_ref: String,
    font_size: f32,
    tx: f32,
    ty: f32,
    leading: f32,
    line_width: f32,
    cur_x: f32,
    cur_y: f32,
    has_point: bool,
    spans: Vec<TextSpan>,
    vectors: Vec<VectorSegment>,
}

impl WalkState {
    fn new(origin_x: f32, top_y: f32, fonts: BTreeMap<String, FontInfo>) -> Self {
        Self {
            origin_x,
            top_y,
            fonts,
            font_ref: String::new(),
            font_size: 12.0,
            tx: 0.0,
            ty: 0.0,
            leading: 0.0,
            line_width: 1.0,
            cur_x: 0.0,
            cur_y: 0.0,
            has_point: false,
            spans: Vec::new(),
            vectors: Vec::new(),
        }
    }

    fn apply(&mut self, op: &lopdf::content::Operation) {
        match op.operator.as_str() {
            "BT" => {
                self.tx = 0.0;
                self.ty = 0.0;
            }
            "ET" => {}
            "Tf" => {
                if op.operands.len() >= 2 {
                    self.font_ref = obj_name_string(&op.operands[0])
                        .trim_start_matches('/')
                        .to_string();
                    if let Some(size) = obj_f32(&op.operands[1]) {
                        self.font_size = size;
                        if self.leading == 0.0 {
                            self.leading = size;
                        }
                    }
                }
            }
            "Tm" => {
                if op.operands.len() >= 6 {
                    if let (Some(e), Some(f)) = (obj_f32(&op.operands[4]), obj_f32(&op.operands[5]))
                    {
                        self.tx = e;
                        self.ty = f;
                    }
                }
            }
            "Td" => {
                if op.operands.len() >= 2 {
                    self.tx += obj_f32(&op.operands[0]).unwrap_or(0.0);
                    self.ty += obj_f32(&op.operands[1]).unwrap_or(0.0);
                }
            }
            "TD" => {
                if op.operands.len() >= 2 {
                    let dy = obj_f32(&op.operands[1]).unwrap_or(0.0);
                    self.tx += obj_f32(&op.operands[0]).unwrap_or(0.0);
                    self.ty += dy;
                    self.leading = -dy;
                }
            }
            "T*" => {
                self.ty -= self.leading;
            }
            "TL" => {
                if let Some(v) = op.operands.first().and_then(obj_f32) {
                    self.leading = v;
                }
            }
            "Tj" => {
                if let Some(obj) = op.operands.first() {
                    let text = obj_text_string(obj);
                    self.emit_text(&text);
                }
            }
            "TJ" => {
                if let Some(Object::Array(items)) = op.operands.first() {
                    let mut text = String::new();
                    for item in items {
                        match item {
                            Object::String(_, _) => text.push_str(&obj_text_string(item)),
                            // Large negative kerns usually separate words.
                            n if obj_f32(n).is_some_and(|k| k < -150.0)
                                && !text.ends_with(char::is_whitespace) =>
                            {
                                text.push(' ');
                            }
                            _ => {}
                        }
                    }
                    self.emit_text(&text);
                }
            }
            "'" => {
                self.ty -= self.leading;
                if let Some(obj) = op.operands.first() {
                    let text = obj_text_string(obj);
                    self.emit_text(&text);
                }
            }
            "\"" => {
                self.ty -= self.leading;
                if op.operands.len() >= 3 {
                    let text = obj_text_string(&op.operands[2]);
                    self.emit_text(&text);
                }
            }
            "w" => {
                if let Some(v) = op.operands.first().and_then(obj_f32) {
                    self.line_width = v;
                }
            }
            "m" => {
                if op.operands.len() >= 2 {
                    self.cur_x = obj_f32(&op.operands[0]).unwrap_or(0.0);
                    self.cur_y = obj_f32(&op.operands[1]).unwrap_or(0.0);
                    self.has_point = true;
                }
            }
            "l" => {
                if op.operands.len() >= 2 && self.has_point {
                    let x = obj_f32(&op.operands[0]).unwrap_or(0.0);
                    let y = obj_f32(&op.operands[1]).unwrap_or(0.0);
                    self.emit_vector(self.cur_x, self.cur_y, x, y);
                    self.cur_x = x;
                    self.cur_y = y;
                }
            }
            "re" if op.operands.len() >= 4 => {
                let x = obj_f32(&op.operands[0]).unwrap_or(0.0);
                let y = obj_f32(&op.operands[1]).unwrap_or(0.0);
                let w = obj_f32(&op.operands[2]).unwrap_or(0.0);
                let h = obj_f32(&op.operands[3]).unwrap_or(0.0);
                self.emit_vector(x, y, x + w, y);
                self.emit_vector(x + w, y, x + w, y + h);
                self.emit_vector(x + w, y + h, x, y + h);
                self.emit_vector(x, y + h, x, y);
            }
            _ => {}
        }
    }

    fn emit_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let info = self.fonts.get(&self.font_ref).cloned().unwrap_or_default();
        let font_name = if info.name.is_empty() {
            "Unknown".to_string()
        } else {
            info.name.clone()
        };
        let w = estimate_width(text, self.font_size);
        let h = self.font_size;
        let bbox = BBox::new(
            self.tx - self.origin_x,
            self.top_y - (self.ty + h),
            self.tx + w - self.origin_x,
            self.top_y - self.ty,
        );
        self.spans.push(TextSpan {
            text: text.to_string(),
            bbox,
            font_name,
            font_size: self.font_size,
            is_bold: info.is_bold,
            is_italic: info.is_italic,
            is_monospace: info.is_monospace,
        });
        // Advance the text position for subsequent Tj in the same BT block.
        self.tx += w;
    }

    fn emit_vector(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let (lx, hx) = (x0.min(x1), x0.max(x1));
        let (ly, hy) = (y0.min(y1), y0.max(y1));
        self.vectors.push(VectorSegment {
            x0: lx - self.origin_x,
            y0: self.top_y - hy,
            x1: hx - self.origin_x,
            y1: self.top_y - ly,
            stroke_width: self.line_width,
        });
    }
}

/// Average-advance width estimate: `0.55 * size` per char.
fn estimate_width(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * 0.55
}

fn obj_f32(obj: &Object) -> Option<f32> {
    match obj {
        Object::Integer(v) => Some(*v as f32),
        Object::Real(v) => Some(*v),
        _ => None,
    }
}

/// Decode a text-showing operand to a Rust string.
fn obj_text_string(obj: &Object) -> String {
    match obj {
        Object::String(bytes, _) => String::from_utf8_lossy(bytes).into_owned(),
        Object::Name(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        _ => String::new(),
    }
}

/// Decode a name operand (font refs, BaseFont) to a Rust string.
fn obj_name_string(obj: &Object) -> String {
    match obj {
        Object::Name(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Object::String(bytes, _) => String::from_utf8_lossy(bytes).into_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures;

    /// Gate 1: all four synthetic PDFs extract accurate coordinates.
    #[test]
    fn extracts_single_col_clean() {
        let pdf = test_fixtures::single_col_clean_pdf();
        let pages = extract_document(&pdf, "single_col_clean.pdf").expect("extract");
        assert_eq!(pages.len(), 1);
        let p = &pages[0];
        assert!((p.width - 612.0).abs() < 1e-3);
        assert!((p.height - 792.0).abs() < 1e-3);
        assert_eq!(p.spans.len(), 17, "span count");
        assert!(p.vectors.is_empty());

        let title = &p.spans[0];
        assert_eq!(title.text, "RazorPDF Benchmark Document");
        assert!((title.font_size - 20.0).abs() < 1e-3);
        assert!(title.is_bold);
        assert!((title.bbox.x0 - 72.0).abs() < 0.05);
        assert!((title.bbox.y0 - 72.0).abs() < 0.05);
        assert!((title.bbox.height() - 20.0).abs() < 0.05);
    }

    #[test]
    fn extracts_two_col_academic() {
        let pdf = test_fixtures::two_col_academic_pdf();
        let p = extract_page(&pdf, 0).expect("extract");
        assert_eq!(p.spans.len(), 33);
        assert!(p.vectors.is_empty());

        let left = p
            .spans
            .iter()
            .filter(|s| s.text.starts_with("Left line"))
            .count();
        let right = p
            .spans
            .iter()
            .filter(|s| s.text.starts_with("Right line"))
            .count();
        assert_eq!(left, 16);
        assert_eq!(right, 16);

        let l0 = p
            .spans
            .iter()
            .find(|s| s.text == "Left line 00 abcdef")
            .unwrap();
        let r0 = p
            .spans
            .iter()
            .find(|s| s.text == "Right line 00 ghijkl")
            .unwrap();
        assert!((l0.bbox.x0 - 72.0).abs() < 0.05);
        assert!((r0.bbox.x0 - 330.0).abs() < 0.05);
        assert!((l0.bbox.y0 - r0.bbox.y0).abs() < 0.05, "same row");
        // Column gutter survives width estimation.
        assert!(l0.bbox.x1 < r0.bbox.x0, "gutter must be non-empty");
    }

    #[test]
    fn extracts_bordered_table() {
        let pdf = test_fixtures::bordered_table_pdf();
        let p = extract_page(&pdf, 0).expect("extract");
        assert_eq!(p.spans.len(), 13, "title + 3 header + 9 cells");
        assert_eq!(p.vectors.len(), 9, "5H + 4V grid lines");

        assert!(p.spans.iter().any(|s| s.text == "Apples"));
        assert!(p.spans.iter().any(|s| s.text == "3.10"));

        let h_count = p
            .vectors
            .iter()
            .filter(|v| (v.y1 - v.y0).abs() < 1.5)
            .count();
        let v_count = p
            .vectors
            .iter()
            .filter(|v| (v.x1 - v.x0).abs() < 1.5)
            .count();
        assert_eq!(h_count, 5);
        assert_eq!(v_count, 4);
    }

    #[test]
    fn extracts_messy_overlap_table() {
        let pdf = test_fixtures::messy_overlap_table_pdf();
        let p = extract_page(&pdf, 0).expect("extract");
        assert_eq!(p.spans.len(), 33, "title + 2 para + 30 stacked islands");
        assert!(p.vectors.is_empty());

        // Every island text appears exactly twice (stacked double-strike).
        for r in 0..5 {
            for text in [
                format!("Item{r}"),
                format!("{} pcs", r + 1),
                format!("{}.99", (r + 1) * 7),
            ] {
                let hits: Vec<&TextSpan> = p.spans.iter().filter(|s| s.text == text).collect();
                assert_eq!(hits.len(), 2, "{text} must be double-struck");
                assert!(
                    hits[0].bbox.overlap(&hits[1].bbox) > 0.9,
                    "{text} layers must heavily overlap"
                );
            }
        }
    }

    #[test]
    fn ascii85_round_trip_vectors() {
        // "Hello" per the Adobe reference example framing.
        assert_eq!(ascii85_decode(b"87cURDZ").unwrap(), b"Hello");
        assert_eq!(ascii85_decode(b"z").unwrap(), vec![0, 0, 0, 0]);
        assert_eq!(ascii85_decode(b"<~87cURDZ~>").unwrap(), b"Hello");
        assert!(ascii85_decode(b"v").is_err()); // dangling 1-char group
        assert!(ascii85_decode(b"\x01").is_err());
    }
}
