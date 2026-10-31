//! Pure-Rust PDF extractor (works anywhere, no C libraries).
//!
//! Parses content-stream text operators (`BT/ET`, `Tf`, `Tm`, `Tj`, `TJ`)
//! and vector path operators (`m`, `l`, `re`, `w`), flipping all coordinates
//! to top-left origin on emission.

use anyhow::{Context, Result};
use lopdf::{
    content::{Content, Operation},
    Document, Object, ObjectId,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;

use super::fonts::{parse_tounicode_cmap, FontDecoding};
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
        let (forms, images) = collect_xobjects(&doc, page_dict, &fonts);
        inputs.push(PageInput {
            page_index,
            origin_x: ml,
            top_y: mt,
            width: (mr - ml).max(1.0),
            height: (mt - mb).max(1.0),
            fonts,
            content_bytes,
            forms,
            images,
        });
    }

    let mut out: Vec<RawPageElements> = inputs
        .into_par_iter()
        .map(|input| {
            let content = Content::decode(&input.content_bytes).context("decode content stream")?;
            let mut state = WalkState::new(
                input.origin_x,
                input.top_y,
                input.fonts,
                input.forms,
                input.images,
            );
            for op in &content.operations {
                state.apply(op);
            }
            Ok(RawPageElements {
                page_index: input.page_index,
                width: input.width,
                height: input.height,
                spans: state.spans,
                vectors: state.vectors,
                image_bboxes: state.image_bboxes,
                filled_rects: state.filled_rects,
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
    forms: HashMap<String, Arc<FormXObject>>,
    images: HashMap<String, (u32, u32)>,
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
    decoding: FontDecoding,
}

/// Map content-stream font refs (`F1`) to font metadata + decoding.
fn font_map(doc: &Document, page: &lopdf::Dictionary) -> Result<BTreeMap<String, FontInfo>> {
    let resources = page
        .get(b"Resources")
        .map_err(|_| anyhow::anyhow!("no resources"))?;
    let resources = resolve_dict(doc, resources)?;
    font_map_from_resources(doc, resources)
}

/// Build the font map from a resource dictionary (page or Form XObject).
fn font_map_from_resources(
    doc: &Document,
    resources: &lopdf::Dictionary,
) -> Result<BTreeMap<String, FontInfo>> {
    let mut out = BTreeMap::new();
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
        out.insert(font_ref, font_info(doc, fd));
    }
    Ok(out)
}

/// Metadata + decoding tables for one font dictionary.
fn font_info(doc: &Document, fd: &lopdf::Dictionary) -> FontInfo {
    let subtype = fd
        .get(b"Subtype")
        .ok()
        .map(obj_name_string)
        .unwrap_or_default();
    // Type0 fonts carry the real face in the first descendant.
    let inner: &lopdf::Dictionary = if subtype == "Type0" {
        fd.get(b"DescendantFonts")
            .ok()
            .and_then(|value| resolve(doc, value).ok())
            .and_then(|value| match value {
                Object::Array(items) => items.first().and_then(|item| resolve(doc, item).ok()),
                _ => None,
            })
            .and_then(|value| match value {
                Object::Dictionary(inner) => Some(inner),
                _ => None,
            })
            .unwrap_or(fd)
    } else {
        fd
    };
    let base = inner
        .get(b"BaseFont")
        .ok()
        .map(obj_name_string)
        .unwrap_or_default();
    let base = strip_subset_prefix(&base);
    let lower = base.to_lowercase();
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
        decoding: font_decoding(doc, fd, &subtype),
    }
}

/// Strip `ABCDEF+` subset prefixes from BaseFont names.
fn strip_subset_prefix(base: &str) -> String {
    let bytes = base.as_bytes();
    if bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(|b| b.is_ascii_uppercase()) {
        base[7..].to_string()
    } else {
        base.to_string()
    }
}

/// Build decode tables: base encoding + Differences + ToUnicode + Identity.
fn font_decoding(doc: &Document, fd: &lopdf::Dictionary, subtype: &str) -> FontDecoding {
    let mut base_name: Option<String> = None;
    let mut differences: Vec<(u8, String)> = Vec::new();
    let mut identity = false;
    if let Some(enc) = fd.get(b"Encoding").ok().and_then(|o| resolve(doc, o).ok()) {
        match enc {
            Object::Name(name) => {
                let name = String::from_utf8_lossy(name).into_owned();
                if name == "Identity-H" || name == "Identity-V" {
                    identity = true;
                } else {
                    base_name = Some(name);
                }
            }
            Object::Dictionary(dict) => {
                base_name = dict
                    .get(b"BaseEncoding")
                    .ok()
                    .and_then(|o| resolve(doc, o).ok())
                    .map(obj_name_string);
                if let Some(Object::Array(items)) = dict
                    .get(b"Differences")
                    .ok()
                    .and_then(|o| resolve(doc, o).ok())
                {
                    differences = parse_differences(items);
                }
            }
            _ => {}
        }
    }
    let mut decoding = match (subtype, base_name) {
        (_, Some(name)) => FontDecoding::with_base_encoding(&name),
        ("Type1" | "MMType1", None) => FontDecoding::with_base_encoding("Standard"),
        _ => FontDecoding::default(),
    };
    if !differences.is_empty() {
        decoding.apply_differences(&differences);
    }
    if let Some(Object::Stream(stream)) =
        fd.get(b"ToUnicode").ok().and_then(|o| resolve(doc, o).ok())
    {
        if let Ok(bytes) = stream_bytes(stream) {
            decoding.apply_tounicode(&parse_tounicode_cmap(&bytes));
        }
    }
    if identity {
        decoding.set_two_byte();
    }
    decoding
}

/// Parse a `/Differences` array into (code, glyph-name) pairs.
fn parse_differences(items: &[Object]) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    let mut code: u32 = 0;
    for item in items {
        match item {
            Object::Integer(n) => code = (*n).max(0) as u32,
            Object::Name(name) => {
                if code <= 255 {
                    out.push((code as u8, String::from_utf8_lossy(name).into_owned()));
                }
                code += 1;
            }
            _ => {}
        }
    }
    out
}

/// Pre-resolved XObjects: named forms + named image dimensions.
type XObjectMaps = (
    HashMap<String, Arc<FormXObject>>,
    HashMap<String, (u32, u32)>,
);

/// Pre-resolve a page's XObjects: forms (ops + scoped resources + matrix)
/// and images (dimensions; bboxes come from the CTM at `Do` time).
fn collect_xobjects(
    doc: &Document,
    page: &lopdf::Dictionary,
    page_fonts: &BTreeMap<String, FontInfo>,
) -> XObjectMaps {
    let resources = page
        .get(b"Resources")
        .ok()
        .and_then(|o| resolve(doc, o).ok())
        .and_then(|o| match o {
            Object::Dictionary(d) => Some(d),
            _ => None,
        });
    let Some(resources) = resources else {
        return (HashMap::new(), HashMap::new());
    };
    collect_xobjects_in(doc, resources, page_fonts, 0, &mut HashSet::new())
}

/// Recursive XObject collection with depth cap + visited set (cycle guard).
fn collect_xobjects_in(
    doc: &Document,
    resources: &lopdf::Dictionary,
    fallback_fonts: &BTreeMap<String, FontInfo>,
    depth: u8,
    visited: &mut HashSet<ObjectId>,
) -> XObjectMaps {
    let mut forms = HashMap::new();
    let mut images = HashMap::new();
    if depth > 4 {
        return (forms, images);
    }
    let xobjects = resources
        .get(b"XObject")
        .ok()
        .and_then(|o| resolve(doc, o).ok())
        .and_then(|o| match o {
            Object::Dictionary(d) => Some(d),
            _ => None,
        });
    let Some(xobjects) = xobjects else {
        return (forms, images);
    };
    for (key, val) in xobjects.iter() {
        if let Object::Reference(id) = val {
            if !visited.insert(*id) {
                continue;
            }
        }
        let Ok(obj) = resolve(doc, val) else {
            continue;
        };
        let Object::Stream(stream) = obj else {
            continue;
        };
        let subtype = stream
            .dict
            .get(b"Subtype")
            .ok()
            .map(obj_name_string)
            .unwrap_or_default();
        let name = String::from_utf8_lossy(key).into_owned();
        if subtype == "Image" {
            let width = stream.dict.get(b"Width").ok().and_then(obj_u32);
            let height = stream.dict.get(b"Height").ok().and_then(obj_u32);
            if let (Some(width), Some(height)) = (width, height) {
                images.insert(name, (width, height));
            }
            continue;
        }
        if subtype != "Form" {
            continue;
        }
        let matrix = stream
            .dict
            .get(b"Matrix")
            .ok()
            .map(parse_matrix_6)
            .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let form_resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|o| resolve(doc, o).ok())
            .and_then(|o| match o {
                Object::Dictionary(d) => Some(d),
                _ => None,
            })
            .unwrap_or(resources);
        let mut fonts = fallback_fonts.clone();
        if let Ok(own) = font_map_from_resources(doc, form_resources) {
            fonts.extend(own);
        }
        let Ok(content_bytes) = stream_bytes(stream) else {
            continue;
        };
        let Ok(content) = Content::decode(&content_bytes) else {
            continue;
        };
        let (nested_forms, nested_images) =
            collect_xobjects_in(doc, form_resources, &fonts, depth + 1, visited);
        forms.insert(
            name,
            Arc::new(FormXObject {
                matrix,
                fonts,
                ops: content.operations,
                forms: nested_forms,
                images: nested_images,
            }),
        );
    }
    (forms, images)
}

/// Parse a 6-element matrix array (identity fallback per component).
fn parse_matrix_6(obj: &Object) -> [f32; 6] {
    match obj {
        Object::Array(items) if items.len() == 6 => {
            let mut m = [0.0f32; 6];
            for (i, item) in items.iter().enumerate() {
                let identity = if i == 0 || i == 3 { 1.0 } else { 0.0 };
                m[i] = obj_f32(item).unwrap_or(identity);
            }
            m
        }
        _ => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    }
}

fn obj_u32(obj: &Object) -> Option<u32> {
    match obj {
        Object::Integer(v) => Some((*v).max(0) as u32),
        _ => None,
    }
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

/// Pre-resolved Form XObject with its own resource namespace.
struct FormXObject {
    matrix: [f32; 6],
    fonts: BTreeMap<String, FontInfo>,
    ops: Vec<Operation>,
    forms: HashMap<String, Arc<FormXObject>>,
    images: HashMap<String, (u32, u32)>,
}

/// Graphics state saved by `q`, restored by `Q`.
#[derive(Clone, Copy)]
struct GraphicsState {
    ctm: [f32; 6],
    line_width: f32,
}

/// Resource namespace saved across `Do` (forms have scoped resources).
struct ResourceScope {
    fonts: BTreeMap<String, FontInfo>,
    forms: HashMap<String, Arc<FormXObject>>,
    images: HashMap<String, (u32, u32)>,
}

/// Max `Do` recursion depth (cycle guard on top of the visited set).
const MAX_FORM_DEPTH: u8 = 8;

/// Transform a point by a 6-element matrix (row-vector convention).
fn transform_point(m: &[f32; 6], x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// Concatenate: CTM' = M × CTM (3×3 row-vector multiply).
fn concat_matrix(ctm: [f32; 6], m: [f32; 6]) -> [f32; 6] {
    [
        m[0] * ctm[0] + m[1] * ctm[2],
        m[0] * ctm[1] + m[1] * ctm[3],
        m[2] * ctm[0] + m[3] * ctm[2],
        m[2] * ctm[1] + m[3] * ctm[3],
        m[4] * ctm[0] + m[5] * ctm[2] + ctm[4],
        m[4] * ctm[1] + m[5] * ctm[3] + ctm[5],
    ]
}

struct WalkState {
    origin_x: f32,
    top_y: f32,
    fonts: BTreeMap<String, FontInfo>,
    font_ref: String,
    font_size: f32,
    tx: f32,
    ty: f32,
    /// Line-matrix origin: Td/T* offset from here (not from the Tj cursor).
    tlx: f32,
    tly: f32,
    leading: f32,
    line_width: f32,
    cur_x: f32,
    cur_y: f32,
    has_point: bool,
    pending_rects: Vec<[f32; 4]>,
    spans: Vec<TextSpan>,
    vectors: Vec<VectorSegment>,
    filled_rects: Vec<BBox>,
    fallback_decoding: FontDecoding,
    ctm: [f32; 6],
    tm_lin: [f32; 4],
    gstack: Vec<GraphicsState>,
    forms: HashMap<String, Arc<FormXObject>>,
    images: HashMap<String, (u32, u32)>,
    scope_stack: Vec<ResourceScope>,
    image_bboxes: Vec<BBox>,
}

impl WalkState {
    fn new(
        origin_x: f32,
        top_y: f32,
        fonts: BTreeMap<String, FontInfo>,
        forms: HashMap<String, Arc<FormXObject>>,
        images: HashMap<String, (u32, u32)>,
    ) -> Self {
        Self {
            origin_x,
            top_y,
            fonts,
            font_ref: String::new(),
            font_size: 12.0,
            tx: 0.0,
            ty: 0.0,
            tlx: 0.0,
            tly: 0.0,
            leading: 0.0,
            line_width: 1.0,
            cur_x: 0.0,
            cur_y: 0.0,
            has_point: false,
            pending_rects: Vec::new(),
            spans: Vec::new(),
            vectors: Vec::new(),
            filled_rects: Vec::new(),
            fallback_decoding: FontDecoding::default(),
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            tm_lin: [1.0, 0.0, 0.0, 1.0],
            gstack: Vec::new(),
            forms,
            images,
            scope_stack: Vec::new(),
            image_bboxes: Vec::new(),
        }
    }

    fn apply(&mut self, op: &lopdf::content::Operation) {
        self.apply_with_depth(op, 0);
    }

    fn apply_with_depth(&mut self, op: &lopdf::content::Operation, depth: u8) {
        match op.operator.as_str() {
            "BT" => {
                // BT resets the text matrix (but not the font/size).
                self.tx = 0.0;
                self.ty = 0.0;
                self.tlx = 0.0;
                self.tly = 0.0;
                self.tm_lin = [1.0, 0.0, 0.0, 1.0];
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
                    let a = obj_f32(&op.operands[0]).unwrap_or(1.0);
                    let b = obj_f32(&op.operands[1]).unwrap_or(0.0);
                    let c = obj_f32(&op.operands[2]).unwrap_or(0.0);
                    let d = obj_f32(&op.operands[3]).unwrap_or(1.0);
                    if let (Some(e), Some(f)) = (obj_f32(&op.operands[4]), obj_f32(&op.operands[5]))
                    {
                        self.tm_lin = [a, b, c, d];
                        self.tx = e;
                        self.ty = f;
                        self.tlx = e;
                        self.tly = f;
                    }
                }
            }
            "Td" => {
                if op.operands.len() >= 2 {
                    let (dx, dy) = self.transform_text_offset(
                        obj_f32(&op.operands[0]).unwrap_or(0.0),
                        obj_f32(&op.operands[1]).unwrap_or(0.0),
                    );
                    self.tlx += dx;
                    self.tly += dy;
                    self.tx = self.tlx;
                    self.ty = self.tly;
                }
            }
            "TD" => {
                if op.operands.len() >= 2 {
                    let raw_dy = obj_f32(&op.operands[1]).unwrap_or(0.0);
                    let (dx, dy) =
                        self.transform_text_offset(obj_f32(&op.operands[0]).unwrap_or(0.0), raw_dy);
                    self.tlx += dx;
                    self.tly += dy;
                    self.tx = self.tlx;
                    self.ty = self.tly;
                    self.leading = -raw_dy;
                }
            }
            "T*" => {
                let (dx, dy) = self.transform_text_offset(0.0, -self.leading);
                self.tlx += dx;
                self.tly += dy;
                self.tx = self.tlx;
                self.ty = self.tly;
            }
            "TL" => {
                if let Some(v) = op.operands.first().and_then(obj_f32) {
                    self.leading = v;
                }
            }
            "Tj" => {
                if let Some(obj) = op.operands.first() {
                    let text = self.decode_operand(obj);
                    self.emit_text(&text);
                }
            }
            "TJ" => {
                if let Some(Object::Array(items)) = op.operands.first() {
                    let mut text = String::new();
                    for item in items {
                        match item {
                            Object::String(_, _) => text.push_str(&self.decode_operand(item)),
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
                    let text = self.decode_operand(obj);
                    self.emit_text(&text);
                }
            }
            "\"" => {
                self.ty -= self.leading;
                if op.operands.len() >= 3 {
                    let text = self.decode_operand(&op.operands[2]);
                    self.emit_text(&text);
                }
            }
            "q" => {
                self.gstack.push(GraphicsState {
                    ctm: self.ctm,
                    line_width: self.line_width,
                });
            }
            "Q" => {
                if let Some(saved) = self.gstack.pop() {
                    self.ctm = saved.ctm;
                    self.line_width = saved.line_width;
                }
            }
            "cm" => {
                if op.operands.len() >= 6 {
                    let mut m = [0.0f32; 6];
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        m[i] = obj_f32(operand).unwrap_or(0.0);
                    }
                    self.ctm = concat_matrix(self.ctm, m);
                }
            }
            "Do" => {
                if depth < MAX_FORM_DEPTH {
                    if let Some(name) = op.operands.first().map(obj_name_string) {
                        self.invoke_xobject(name.trim_start_matches('/'), depth);
                    }
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
                    // A new subpath abandons pure-rect tracking (under-counts
                    // exotic mixed paths rather than leaking stale rects).
                    self.pending_rects.clear();
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
                // Deferred: stroke vs fill is decided by the paint op.
                self.pending_rects.push([x, y, w, h]);
            }
            "S" | "s" => {
                let rects = std::mem::take(&mut self.pending_rects);
                self.stroke_rects(&rects);
            }
            "f" | "F" | "f*" => {
                let rects = std::mem::take(&mut self.pending_rects);
                self.fill_rects(&rects);
            }
            "B" | "B*" | "b" | "b*" => {
                // Drain once, apply to both: fill must not starve stroke.
                let rects = std::mem::take(&mut self.pending_rects);
                self.fill_rects(&rects);
                self.stroke_rects(&rects);
            }
            "n" => {
                self.pending_rects.clear();
            }
            _ => {}
        }
    }

    /// Text-space offset → user-space offset through the Tm linear part.
    fn transform_text_offset(&self, dx: f32, dy: f32) -> (f32, f32) {
        let [a, b, c, d] = self.tm_lin;
        (a * dx + c * dy, b * dx + d * dy)
    }

    /// Effective font size: Tf × |CTM × Tm × x-axis| (exact under any mix
    /// of text scaling and CTM transforms; 1.0 scale when both identity).
    fn effective_scale(&self) -> f32 {
        let [a, b, _, _] = self.tm_lin;
        let rx = self.ctm[0] * a + self.ctm[2] * b;
        let ry = self.ctm[1] * a + self.ctm[3] * b;
        rx.hypot(ry).max(0.001)
    }

    /// Execute a Form XObject (scoped resources + matrix) or record an image.
    fn invoke_xobject(&mut self, name: &str, depth: u8) {
        if self.images.contains_key(name) {
            // Images render into the unit square mapped by the CTM.
            let (x0, y0) = transform_point(&self.ctm, 0.0, 0.0);
            let (x1, y1) = transform_point(&self.ctm, 1.0, 1.0);
            let (lx, hx) = (x0.min(x1), x0.max(x1));
            let (ly, hy) = (y0.min(y1), y0.max(y1));
            self.image_bboxes.push(BBox::new(
                lx - self.origin_x,
                self.top_y - hy,
                hx - self.origin_x,
                self.top_y - ly,
            ));
            return;
        }
        let form = self.forms.get(name).cloned();
        let Some(form) = form else {
            return;
        };
        self.scope_stack.push(ResourceScope {
            fonts: std::mem::take(&mut self.fonts),
            forms: std::mem::take(&mut self.forms),
            images: std::mem::take(&mut self.images),
        });
        self.fonts.clone_from(&form.fonts);
        self.forms.clone_from(&form.forms);
        self.images.clone_from(&form.images);
        let saved_ctm = self.ctm;
        self.ctm = concat_matrix(self.ctm, form.matrix);
        for op in &form.ops {
            self.apply_with_depth(op, depth + 1);
        }
        self.ctm = saved_ctm;
        if let Some(scope) = self.scope_stack.pop() {
            self.fonts = scope.fonts;
            self.forms = scope.forms;
            self.images = scope.images;
        }
    }

    /// Decode a text-showing operand through the current font (NFKC-normalized).
    fn decode_operand(&self, obj: &Object) -> String {
        let bytes = obj_text_bytes(obj);
        if bytes.is_empty() {
            return String::new();
        }
        self.fonts
            .get(&self.font_ref)
            .map(|info| &info.decoding)
            .unwrap_or(&self.fallback_decoding)
            .decode(&bytes)
    }

    fn emit_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // Borrow font metadata: cloning FontInfo per span copies its whole
        // decoding tables (HashMap + 256-entry byte map).
        let font_name: String;
        let (is_bold, is_italic, is_monospace): (bool, bool, bool);
        match self.fonts.get(&self.font_ref) {
            Some(info) => {
                font_name = if info.name.is_empty() {
                    "Unknown".to_string()
                } else {
                    info.name.clone()
                };
                is_bold = info.is_bold;
                is_italic = info.is_italic;
                is_monospace = info.is_monospace;
            }
            None => {
                font_name = "Unknown".to_string();
                is_bold = false;
                is_italic = false;
                is_monospace = false;
            }
        };
        let w = estimate_width(text, self.font_size);
        let h = self.font_size;
        // Text-space run/rise through Tm, then CTM, then flip + union.
        let [a, b, c, d] = self.tm_lin;
        let (mut lx, mut ly) = (f32::INFINITY, f32::INFINITY);
        let (mut hx, mut hy) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for (ox, oy) in [
            (0.0, 0.0),
            (a * w, b * w),
            (c * h, d * h),
            (a * w + c * h, b * w + d * h),
        ] {
            let (ux, uy) = transform_point(&self.ctm, self.tx + ox, self.ty + oy);
            lx = lx.min(ux);
            ly = ly.min(uy);
            hx = hx.max(ux);
            hy = hy.max(uy);
        }
        let bbox = BBox::new(
            lx - self.origin_x,
            self.top_y - hy,
            hx - self.origin_x,
            self.top_y - ly,
        );
        self.spans.push(TextSpan {
            text: text.to_string(),
            bbox,
            font_name,
            font_size: self.font_size * self.effective_scale(),
            is_bold,
            is_italic,
            is_monospace,
        });
        // Advance along the run direction for subsequent Tj in the block.
        self.tx += a * w;
        self.ty += b * w;
    }

    fn emit_vector(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let (ax, ay) = transform_point(&self.ctm, x0, y0);
        let (bx, by) = transform_point(&self.ctm, x1, y1);
        let (lx, hx) = (ax.min(bx), ax.max(bx));
        let (ly, hy) = (ay.min(by), ay.max(by));
        self.vectors.push(VectorSegment {
            x0: lx - self.origin_x,
            y0: self.top_y - hy,
            x1: hx - self.origin_x,
            y1: self.top_y - ly,
            stroke_width: self.line_width,
        });
    }

    /// Stroke the given `re` rects as 4 segments each.
    fn stroke_rects(&mut self, rects: &[[f32; 4]]) {
        for &[x, y, w, h] in rects {
            self.emit_vector(x, y, x + w, y);
            self.emit_vector(x + w, y, x + w, y + h);
            self.emit_vector(x + w, y + h, x, y + h);
            self.emit_vector(x, y + h, x, y);
        }
    }

    /// Record the given `re` rects as filled bboxes (CTM-transformed).
    fn fill_rects(&mut self, rects: &[[f32; 4]]) {
        for &[x, y, w, h] in rects {
            let (ax, ay) = transform_point(&self.ctm, x, y);
            let (bx, by) = transform_point(&self.ctm, x + w, y + h);
            let (lx, hx) = (ax.min(bx), ax.max(bx));
            let (ly, hy) = (ay.min(by), ay.max(by));
            if hx - lx < 0.5 || hy - ly < 0.5 {
                continue; // degenerate sliver, not a shaded cell
            }
            self.filled_rects.push(BBox::new(
                lx - self.origin_x,
                self.top_y - hy,
                hx - self.origin_x,
                self.top_y - ly,
            ));
        }
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

/// Raw bytes of a text-showing operand.
fn obj_text_bytes(obj: &Object) -> Vec<u8> {
    match obj {
        Object::String(bytes, _) => bytes.clone(),
        Object::Name(bytes) => bytes.clone(),
        _ => Vec::new(),
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

    #[test]
    fn extracts_tounicode_font() {
        let pdf = test_fixtures::tounicode_font_pdf();
        let p = extract_page(&pdf, 0).expect("extract");
        assert_eq!(p.spans.len(), 2);
        // 0x02 via Differences + ToUnicode (U+FB01, NFKC → "fi").
        assert_eq!(p.spans[0].text, "figures");
        // 0x13 via Differences + AGL alone (U+FB02, NFKC → "fl").
        assert_eq!(p.spans[1].text, "aflow");
    }

    #[test]
    fn matrix_concat_orders_transforms() {
        let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let scale2 = [2.0, 0.0, 0.0, 2.0, 0.0, 0.0];
        let shift = [1.0, 0.0, 0.0, 1.0, 10.0, 0.0];
        assert_eq!(concat_matrix(id, scale2), scale2);
        // cm scale, then cm translate: the shift applies in scaled space.
        let both = concat_matrix(concat_matrix(id, scale2), shift);
        assert_eq!(both, [2.0, 0.0, 0.0, 2.0, 20.0, 0.0]);
        assert_eq!(transform_point(&both, 5.0, 5.0), (30.0, 10.0));
    }

    #[test]
    fn matrix_parse_falls_back_to_identity() {
        assert_eq!(
            parse_matrix_6(&Object::Array(vec![
                Object::Integer(2),
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(2),
                Object::Integer(5),
                Object::Integer(6),
            ])),
            [2.0, 0.0, 0.0, 2.0, 5.0, 6.0]
        );
        assert_eq!(
            parse_matrix_6(&Object::Name(b"nope".to_vec())),
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        );
    }

    #[test]
    fn extracts_xobject_form() {
        let pdf = test_fixtures::xobject_form_pdf();
        let p = extract_page(&pdf, 0).expect("extract");
        assert_eq!(p.spans.len(), 3);

        let direct = &p.spans[0];
        assert_eq!(direct.text, "Direct text");
        assert!((direct.bbox.x0 - 72.0).abs() < 0.05);
        assert!((direct.bbox.y0 - 100.0).abs() < 0.05);

        // Form text at form-space (10,700) + cm translate (100,50).
        let form = p.spans.iter().find(|s| s.text == "Form text").unwrap();
        assert!((form.bbox.x0 - 110.0).abs() < 0.05);
        assert!((form.bbox.y0 - 31.0).abs() < 0.05);

        // Rotated span: corners (300,400)..(289,424.2) in user space.
        let spun = p.spans.iter().find(|s| s.text == "Spun").unwrap();
        assert!((spun.bbox.x0 - 289.0).abs() < 0.1);
        assert!((spun.bbox.y0 - 367.8).abs() < 0.1);

        // Image unit square through scale(10) + translate(72,500).
        assert_eq!(p.image_bboxes.len(), 1);
        let img = &p.image_bboxes[0];
        assert!((img.x0 - 72.0).abs() < 0.05);
        assert!((img.y0 - 282.0).abs() < 0.05);
        assert!((img.x1 - 82.0).abs() < 0.05);
        assert!((img.y1 - 292.0).abs() < 0.05);
    }

    #[test]
    fn extracts_td_relative_lines_without_drift() {
        let pdf = test_fixtures::td_relative_pdf();
        let p = extract_page(&pdf, 0).expect("extract");
        assert_eq!(p.spans.len(), 4);
        for span in &p.spans {
            assert!((span.bbox.x0 - 72.0).abs() < 0.05, "{:?}", span.text);
        }
        assert!((p.spans[0].bbox.y0 - 101.0).abs() < 0.05);
        assert!((p.spans[1].bbox.y0 - 113.0).abs() < 0.05);
        assert!((p.spans[2].bbox.y0 - 181.0).abs() < 0.05);
        assert!((p.spans[3].bbox.y0 - 193.0).abs() < 0.05);
    }

    #[test]
    fn extracts_rect_strokes_and_fills() {
        let pdf = test_fixtures::fill_stroke_pdf();
        let p = extract_page(&pdf, 0).expect("extract");
        assert_eq!(p.vectors.len(), 8);
        assert_eq!(p.filled_rects.len(), 2);
        assert!((p.filled_rects[0].x0 - 200.0).abs() < 0.05);
        assert!((p.filled_rects[0].y0 - 142.0).abs() < 0.05);
        assert!((p.filled_rects[1].x0 - 330.0).abs() < 0.05);
        assert!((p.filled_rects[1].y1 - 192.0).abs() < 0.05);
    }
}
