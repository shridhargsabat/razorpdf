//! Synthetic in-memory PDF generator for offline tests.
//!
//! Builds four single-page fixtures with `lopdf` (no `printpdf` needed):
//! - `single_col_clean`: headings + paragraphs
//! - `two_col_academic`: full-width title + two text columns
//! - `bordered_table`: vector grid + cell text
//! - `messy_overlap_table`: overlapping spans + irregular borderless islands
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

/// Wrap a content stream in a single-page PDF with Helvetica fonts.
fn build_pdf(content: String) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");

    let font_reg_id = doc.add_object(Object::Dictionary(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    }));
    let font_bold_id = doc.add_object(Object::Dictionary(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Bold",
    }));
    let content_id = doc.add_object(Object::Stream(Stream::new(
        dictionary! {},
        content.into_bytes(),
    )));

    let mut font_map = lopdf::Dictionary::new();
    font_map.set("F1", Object::Reference(font_reg_id));
    font_map.set("F2", Object::Reference(font_bold_id));
    let mut resources = lopdf::Dictionary::new();
    resources.set("Font", Object::Dictionary(font_map));

    let mut page = lopdf::Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(PAGE_W as i64),
            Object::Integer(PAGE_H as i64),
        ]),
    );
    page.set("Resources", Object::Dictionary(resources));
    page.set("Contents", Object::Reference(content_id));
    // Parent is patched after the Pages node exists.
    let page_id = doc.add_object(Object::Dictionary(page));

    let mut pages = lopdf::Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    pages.set("Count", Object::Integer(1));
    let pages_id = doc.add_object(Object::Dictionary(pages));

    if let Ok(Object::Dictionary(page_dict)) = doc.get_object_mut(page_id) {
        page_dict.set("Parent", Object::Reference(pages_id));
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

/// Clean single-column document: title + 2 sections. 17 spans, 0 vectors.
pub fn single_col_clean_pdf() -> Vec<u8> {
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
    build_pdf(c)
}

/// Academic two-column layout: title + 16 lines per column. 33 spans.
pub fn two_col_academic_pdf() -> Vec<u8> {
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
    build_pdf(c)
}

/// Bordered 4x3 sales table with vector grid. 13 spans, 9 vectors.
pub fn bordered_table_pdf() -> Vec<u8> {
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
    build_pdf(c)
}

/// Messy page: overlapping spans + irregular borderless islands.
/// 20 spans, 0 vectors. Designed to trip triage scoring (Phase 3).
pub fn messy_overlap_table_pdf() -> Vec<u8> {
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
    // Overlapping duplicate pair (1pt x-offset => overlap ~= 0.99).
    c.push_str(&text("F1", 11.0, 72.0, 150.0, "Total amount due 42.00"));
    c.push_str(&text("F1", 11.0, 73.0, 150.0, "Total amount due 42.00"));
    // Irregular borderless islands: misaligned x0 across rows.
    let jitter = [
        [0.0, 0.0, 0.0],
        [14.0, -22.0, 10.0],
        [-10.0, 18.0, -14.0],
        [8.0, 10.0, 22.0],
        [-16.0, -8.0, 6.0],
    ];
    for (r, j) in jitter.iter().enumerate() {
        let y = 190.0 + r as f32 * 16.0;
        c.push_str(&text("F1", 11.0, 72.0 + j[0], y, &format!("Item{r}")));
        c.push_str(&text(
            "F1",
            11.0,
            250.0 + j[1],
            y,
            &format!("{} pcs", r + 1),
        ));
        c.push_str(&text(
            "F1",
            11.0,
            420.0 + j[2],
            y,
            &format!("{}.99", (r + 1) * 7),
        ));
    }
    build_pdf(c)
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
}
