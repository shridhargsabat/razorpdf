//! Synthetic in-memory PDF generator for offline tests.
//!
//! Builds single-page fixtures with `lopdf` (no `printpdf` needed):
//! - `single_col_clean`: headings + paragraphs
//! - `two_col_academic`: full-width title + two text columns
//! - `bordered_table`: vector grid + cell text
//! - `messy_overlap_table`: overlapping spans + irregular borderless islands
//! - `mixed_paper_pdf(n)`: N-page fast-path mix for benches
//!
//! Layouts are authored in top-left-origin points and converted to PDF
//! bottom-left coordinates during emission.

use lopdf::{dictionary, Document, Object, Stream};

/// US Letter page size in points.
pub const PAGE_W: f32 = 612.0;
pub const PAGE_H: f32 = 792.0;

/// Escape a literal PDF string.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

/// Emit one text span. `y_top` is the bbox top in top-left coords; the
/// baseline is placed so extraction recovers exactly this bbox.
fn text(font: &str, size: f32, x: f32, y_top: f32, content: &str) -> String {
    let y_pdf = PAGE_H - y_top - size;
    format!(
        "BT /{font} {size} Tf 1 0 0 1 {x:.2} {y_pdf:.2} Tm ({}) Tj ET\n",
        esc(content)
    )
}

/// Emit a horizontal vector rule at top-left `y_top` from `x0` to `x1`.
fn hline(x0: f32, x1: f32, y_top: f32, w: f32) -> String {
    let y = PAGE_H - y_top;
    format!("{w} w {x0:.2} {y:.2} m {x1:.2} {y:.2} l S\n")
}

/// Emit a vertical vector rule at `x` spanning top-left `y0_top..y1_top`.
fn vline(x: f32, y0_top: f32, y1_top: f32, w: f32) -> String {
    let a = PAGE_H - y0_top;
    let b = PAGE_H - y1_top;
    format!("{w} w {x:.2} {a:.2} m {x:.2} {b:.2} l S\n")
}

/// Wrap content streams in an N-page PDF with shared Helvetica fonts.
fn build_pdf_multi(page_contents: Vec<String>) -> Vec<u8> {
    build_pdf_multi_full(
        page_contents,
        vec![
            FontSpec::helvetica("F1", false),
            FontSpec::helvetica("F2", true),
        ],
        Vec::new(),
    )
}

/// A font resource: subtype, face, optional encoding + ToUnicode CMap.
struct FontSpec {
    resource: &'static str,
    subtype: &'static str,
    base_font: &'static str,
    encoding: Option<lopdf::Dictionary>,
    to_unicode: Option<Vec<u8>>,
}

impl FontSpec {
    fn helvetica(resource: &'static str, bold: bool) -> Self {
        Self {
            resource,
            subtype: "Type1",
            base_font: if bold { "Helvetica-Bold" } else { "Helvetica" },
            encoding: None,
            to_unicode: None,
        }
    }
}

/// Build a `/Differences` encoding dictionary from (code, glyph-name) pairs.
fn differences_dict(entries: &[(i64, &str)]) -> lopdf::Dictionary {
    let mut items = Vec::with_capacity(entries.len() * 2);
    for (code, name) in entries {
        items.push(Object::Integer(*code));
        items.push(Object::Name(name.as_bytes().to_vec()));
    }
    let mut dict = lopdf::Dictionary::new();
    dict.set("Type", Object::Name(b"Encoding".to_vec()));
    dict.set("Differences", Object::Array(items));
    dict
}

/// A page-level XObject: Form (ops + matrix) or Image (pixels).
enum XObjectSpec {
    Form {
        name: &'static str,
        matrix: [f32; 6],
        content: String,
    },
    Image {
        name: &'static str,
        width: i64,
        height: i64,
        data: Vec<u8>,
    },
}

/// Wrap content streams in an N-page PDF with explicit fonts + XObjects.
fn build_pdf_multi_full(
    page_contents: Vec<String>,
    fonts: Vec<FontSpec>,
    xobjects: Vec<XObjectSpec>,
) -> Vec<u8> {
    assert!(!page_contents.is_empty(), "need at least one page");
    let mut doc = Document::with_version("1.7");

    let mut font_map = lopdf::Dictionary::new();
    for spec in &fonts {
        let mut font_dict = lopdf::Dictionary::new();
        font_dict.set("Type", Object::Name(b"Font".to_vec()));
        font_dict.set("Subtype", Object::Name(spec.subtype.as_bytes().to_vec()));
        font_dict.set("BaseFont", Object::Name(spec.base_font.as_bytes().to_vec()));
        if let Some(encoding) = &spec.encoding {
            font_dict.set("Encoding", Object::Dictionary(encoding.clone()));
        }
        if let Some(cmap) = &spec.to_unicode {
            let cmap_id = doc.add_object(Object::Stream(Stream::new(dictionary! {}, cmap.clone())));
            font_dict.set("ToUnicode", Object::Reference(cmap_id));
        }
        let font_id = doc.add_object(Object::Dictionary(font_dict));
        font_map.set(spec.resource, Object::Reference(font_id));
    }
    let mut resources = lopdf::Dictionary::new();
    resources.set("Font", Object::Dictionary(font_map));
    if !xobjects.is_empty() {
        let mut xobject_map = lopdf::Dictionary::new();
        for spec in &xobjects {
            match spec {
                XObjectSpec::Form {
                    name,
                    matrix,
                    content,
                } => {
                    let mut form_dict = lopdf::Dictionary::new();
                    form_dict.set("Type", Object::Name(b"XObject".to_vec()));
                    form_dict.set("Subtype", Object::Name(b"Form".to_vec()));
                    form_dict.set(
                        "BBox",
                        Object::Array(vec![
                            Object::Integer(0),
                            Object::Integer(0),
                            Object::Integer(PAGE_W as i64),
                            Object::Integer(PAGE_H as i64),
                        ]),
                    );
                    form_dict.set(
                        "Matrix",
                        Object::Array(matrix.iter().map(|m| Object::Real(*m)).collect()),
                    );
                    form_dict.set("Resources", Object::Dictionary(resources.clone()));
                    let form_id = doc.add_object(Object::Stream(Stream::new(
                        form_dict,
                        content.clone().into_bytes(),
                    )));
                    xobject_map.set(*name, Object::Reference(form_id));
                }
                XObjectSpec::Image {
                    name,
                    width,
                    height,
                    data,
                } => {
                    let mut img_dict = lopdf::Dictionary::new();
                    img_dict.set("Type", Object::Name(b"XObject".to_vec()));
                    img_dict.set("Subtype", Object::Name(b"Image".to_vec()));
                    img_dict.set("Width", Object::Integer(*width));
                    img_dict.set("Height", Object::Integer(*height));
                    img_dict.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
                    img_dict.set("BitsPerComponent", Object::Integer(8));
                    let img_id =
                        doc.add_object(Object::Stream(Stream::new(img_dict, data.clone())));
                    xobject_map.set(*name, Object::Reference(img_id));
                }
            }
        }
        resources.set("XObject", Object::Dictionary(xobject_map));
    }

    let mediabox = Object::Array(vec![
        Object::Integer(0),
        Object::Integer(0),
        Object::Integer(PAGE_W as i64),
        Object::Integer(PAGE_H as i64),
    ]);
    let mut page_ids = Vec::with_capacity(page_contents.len());
    for content in page_contents {
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            content.into_bytes(),
        )));
        let mut page = lopdf::Dictionary::new();
        page.set("Type", Object::Name(b"Page".to_vec()));
        page.set("MediaBox", mediabox.clone());
        page.set("Resources", Object::Dictionary(resources.clone()));
        page.set("Contents", Object::Reference(content_id));
        // Parent is patched after the Pages node exists.
        page_ids.push(doc.add_object(Object::Dictionary(page)));
    }

    let mut pages_node = lopdf::Dictionary::new();
    pages_node.set("Type", Object::Name(b"Pages".to_vec()));
    pages_node.set(
        "Kids",
        Object::Array(page_ids.iter().map(|id| Object::Reference(*id)).collect()),
    );
    pages_node.set("Count", Object::Integer(page_ids.len() as i64));
    let pages_id = doc.add_object(Object::Dictionary(pages_node));

    for page_id in &page_ids {
        if let Ok(Object::Dictionary(page_dict)) = doc.get_object_mut(*page_id) {
            page_dict.set("Parent", Object::Reference(pages_id));
        }
    }

    let mut catalog = lopdf::Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("fixture pdf serialize");
    buf
}

fn single_col_content() -> String {
    let mut c = String::new();
    c.push_str(&text("F2", 20.0, 72.0, 72.0, "RazorPDF Benchmark Document"));
    c.push_str(&text("F2", 14.0, 72.0, 110.0, "1. Introduction"));
    for (i, y) in (0..8).map(|i| (i, 132.0 + i as f32 * 15.0)) {
        c.push_str(&text(
            "F1",
            11.0,
            72.0,
            y,
            &format!("This is body line {i} of the introduction with sample text."),
        ));
    }
    c.push_str(&text("F2", 14.0, 72.0, 262.0, "2. Method"));
    for (i, y) in (0..6).map(|i| (i, 284.0 + i as f32 * 15.0)) {
        c.push_str(&text(
            "F1",
            11.0,
            72.0,
            y,
            &format!("Method body line {i} describes the geometric pipeline."),
        ));
    }
    c
}

/// Clean single-column document: title + 2 sections. 17 spans, 0 vectors.
pub fn single_col_clean_pdf() -> Vec<u8> {
    build_pdf_multi(vec![single_col_content()])
}

fn two_col_content() -> String {
    let mut c = String::new();
    c.push_str(&text(
        "F2",
        18.0,
        150.0,
        60.0,
        "Advances in Geometric Parsing",
    ));
    for r in 0..16 {
        let y = 110.0 + r as f32 * 14.0;
        c.push_str(&text(
            "F1",
            10.0,
            72.0,
            y,
            &format!("Left line {r:02} abcdef"),
        ));
        c.push_str(&text(
            "F1",
            10.0,
            330.0,
            y,
            &format!("Right line {r:02} ghijkl"),
        ));
    }
    c
}

/// Academic two-column layout: title + 16 lines per column. 33 spans.
pub fn two_col_academic_pdf() -> Vec<u8> {
    build_pdf_multi(vec![two_col_content()])
}

fn bordered_content() -> String {
    let mut c = String::new();
    c.push_str(&text("F2", 16.0, 72.0, 60.0, "Quarterly Sales"));
    let xs = [72.0, 200.0, 340.0, 480.0];
    let ys = [120.0, 145.0, 170.0, 195.0, 220.0];
    for &y in &ys {
        c.push_str(&hline(xs[0], xs[3], y, 0.5));
    }
    for &x in &xs {
        c.push_str(&vline(x, ys[0], ys[4], 0.5));
    }
    let header = ["Name", "Qty", "Price"];
    for (col, h) in header.iter().enumerate() {
        c.push_str(&text("F2", 11.0, xs[col] + 6.0, ys[0] + 7.0, h));
    }
    let rows = [
        ["Apples", "12", "1.20"],
        ["Bread", "3", "2.50"],
        ["Milk", "2", "3.10"],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (col, cell) in row.iter().enumerate() {
            c.push_str(&text("F1", 10.0, xs[col] + 6.0, ys[r + 1] + 7.0, cell));
        }
    }
    c
}

/// Bordered 4x3 sales table with vector grid. 13 spans, 9 vectors.
pub fn bordered_table_pdf() -> Vec<u8> {
    build_pdf_multi(vec![bordered_content()])
}

fn messy_content() -> String {
    let mut c = String::new();
    c.push_str(&text("F2", 16.0, 72.0, 60.0, "Messy Invoice Scan"));
    c.push_str(&text(
        "F1",
        11.0,
        72.0,
        100.0,
        "Invoice total section follows below.",
    ));
    c.push_str(&text(
        "F1",
        11.0,
        72.0,
        116.0,
        "Values may overlap due to bad kerning.",
    ));
    // Irregular borderless islands: misaligned x0 across rows, each island
    // double-struck with a 1.2pt-offset copy (overlap ~= 0.96) like a
    // watermark over the table.
    let jitter = [
        [0.0, 0.0, 0.0],
        [14.0, -22.0, 10.0],
        [-10.0, 18.0, -14.0],
        [8.0, 10.0, 22.0],
        [-16.0, -8.0, 6.0],
    ];
    for (r, j) in jitter.iter().enumerate() {
        let y = 190.0 + r as f32 * 16.0;
        for dx in [0.0, 1.2] {
            c.push_str(&text("F1", 11.0, 72.0 + j[0] + dx, y, &format!("Item{r}")));
            c.push_str(&text(
                "F1",
                11.0,
                250.0 + j[1] + dx,
                y,
                &format!("{} pcs", r + 1),
            ));
            c.push_str(&text(
                "F1",
                11.0,
                420.0 + j[2] + dx,
                y,
                &format!("{}.99", (r + 1) * 7),
            ));
        }
    }
    c
}

/// Messy page: stacked double-strike spans over irregular borderless islands.
/// 33 spans (title + 2 para + 5 rows x 3 islands x 2 layers), 0 vectors.
/// Trips triage: full overlap involvement + column entropy (S = 0.75).
pub fn messy_overlap_table_pdf() -> Vec<u8> {
    build_pdf_multi(vec![messy_content()])
}

/// Custom-encoded font page: TeX-style ligature codes resolved through
/// Differences + ToUnicode (F3) and Differences + AGL alone (F4).
/// 2 spans, 0 vectors.
pub fn tounicode_font_pdf() -> Vec<u8> {
    let mut content = String::new();
    content.push_str(&text("F3", 11.0, 72.0, 100.0, "\x02gures"));
    content.push_str(&text("F4", 11.0, 72.0, 120.0, "a\x13ow"));
    build_pdf_multi_full(
        vec![content],
        vec![
            FontSpec::helvetica("F1", false),
            FontSpec::helvetica("F2", true),
            FontSpec {
                resource: "F3",
                subtype: "Type1",
                base_font: "TeXSubset",
                encoding: Some(differences_dict(&[(2, "fi")])),
                to_unicode: Some(
                    b"1 begincodespacerange\n<00> <FF>\nendcodespacerange\n1 beginbfchar\n<02> <FB01>\nendbfchar\n"
                        .to_vec(),
                ),
            },
            FontSpec {
                resource: "F4",
                subtype: "Type1",
                base_font: "TeXSubsetTwo",
                encoding: Some(differences_dict(&[(0x13, "fl")])),
                to_unicode: None,
            },
        ],
        Vec::new(),
    )
}

/// Form XObject + image + rotated text: translated form content, image bbox
/// from the CTM, rotation-tolerant span bbox. 3 spans, 1 image, 0 vectors.
pub fn xobject_form_pdf() -> Vec<u8> {
    let mut content = String::new();
    content.push_str(&text("F1", 11.0, 72.0, 100.0, "Direct text"));
    content.push_str("q 1 0 0 1 100 50 cm /Fm0 Do Q\n");
    content.push_str("q 10 0 0 10 72 500 cm /Im0 Do Q\n");
    content.push_str("BT /F1 11 Tf 0 1 -1 0 300 400 Tm (Spun) Tj ET\n");
    build_pdf_multi_full(
        vec![content],
        vec![
            FontSpec::helvetica("F1", false),
            FontSpec::helvetica("F2", true),
        ],
        vec![
            XObjectSpec::Form {
                name: "Fm0",
                matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                content: "BT /F1 11 Tf 1 0 0 1 10 700 Tm (Form text) Tj ET\n".to_string(),
            },
            XObjectSpec::Image {
                name: "Im0",
                width: 1,
                height: 1,
                data: vec![0],
            },
        ],
    )
}

/// Relative line positioning: Tm once, then T*/TD between Tj ops.
/// Lines must not drift right (line-matrix origin, not the Tj cursor).
/// 4 spans, 0 vectors.
pub fn td_relative_pdf() -> Vec<u8> {
    let mut content = String::new();
    content.push_str(
        "BT /F1 11 Tf 12 TL 1 0 0 1 72 680 Tm (Line one here) Tj T* (Line two here) Tj ET\n",
    );
    content.push_str("BT /F1 11 Tf 1 0 0 1 72 600 Tm (Row A) Tj 0 -12 TD (Row B) Tj ET\n");
    build_pdf_multi(vec![content])
}

/// Stroke vs fill: `re` defers to the paint op (S strokes, f fills, B both).
/// 8 vectors (2 stroked rects), 2 filled rects.
pub fn fill_stroke_pdf() -> Vec<u8> {
    let content = "72 600 100 50 re S\n200 600 100 50 re f\n330 600 100 50 re B\n";
    build_pdf_multi(vec![content.to_string()])
}

/// N-page mixed-content paper cycling two-column, single-column, and
/// bordered-table pages (all fast-path; for benches and `razorpdf bench`).
/// Panics when `pages == 0`.
pub fn mixed_paper_pdf(pages: usize) -> Vec<u8> {
    assert!(pages > 0, "need at least one page");
    build_pdf_multi(
        (0..pages)
            .map(|i| match i % 3 {
                0 => two_col_content(),
                1 => single_col_content(),
                _ => bordered_content(),
            })
            .collect(),
    )
}

/// All fixtures as `(name, bytes)` pairs.
pub fn all_fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("single_col_clean", single_col_clean_pdf()),
        ("two_col_academic", two_col_academic_pdf()),
        ("bordered_table", bordered_table_pdf()),
        ("messy_overlap_table", messy_overlap_table_pdf()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_build_non_empty() {
        for (name, bytes) in all_fixtures() {
            assert!(!bytes.is_empty(), "{name} is empty");
            assert!(
                bytes.starts_with(b"%PDF-"),
                "{name} missing PDF header: {:?}",
                &bytes[..8.min(bytes.len())]
            );
        }
    }

    #[test]
    fn multipage_extracts_in_order() {
        let pdf = mixed_paper_pdf(7);
        let pages = crate::extractor::extract_document(&pdf, "mixed").expect("extract");
        assert_eq!(pages.len(), 7);
        for (i, page) in pages.iter().enumerate() {
            assert_eq!(page.page_index, i);
        }
        assert_eq!(pages[0].spans.len(), 33); // two-col
        assert_eq!(pages[1].spans.len(), 17); // single
        assert_eq!(pages[2].spans.len(), 13); // bordered
    }
}
