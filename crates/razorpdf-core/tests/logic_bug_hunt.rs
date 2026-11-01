//! Logic/boundary/error-path audit probes. Each test pins correct behavior
//! on a degenerate input; failures here are real bugs (fixed with minimal
//! patches, not test edits).

use std::sync::Arc;

use lopdf::{dictionary, Document, Object, Stream};
use razorpdf_core::extractor::extract_document;
use razorpdf_core::geometry::table_grid::detected_table_to_markdown;
use razorpdf_core::geometry::{DetectedTable, TableSource};
use razorpdf_core::models::BBox;
use razorpdf_core::models::{RawPageElements, TextSpan};

// Minimal one-page PDF with arbitrary content ops.
fn pdf_with_ops(ops: &[u8]) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let mut font = lopdf::Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type1".to_vec()));
    font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    let font_id = doc.add_object(Object::Dictionary(font));
    let mut fonts = lopdf::Dictionary::new();
    fonts.set("F1", Object::Reference(font_id));
    let mut resources = lopdf::Dictionary::new();
    resources.set("Font", Object::Dictionary(fonts));
    let content_id = doc.add_object(Object::Stream(Stream::new(dictionary! {}, ops.to_vec())));
    let mut page = lopdf::Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    );
    page.set("Resources", Object::Dictionary(resources));
    page.set("Contents", Object::Reference(content_id));
    let page_id = doc.add_object(Object::Dictionary(page));
    let mut pages = lopdf::Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    pages.set("Count", Object::Integer(1));
    let pages_id = doc.add_object(Object::Dictionary(pages));
    if let Ok(Object::Dictionary(d)) = doc.get_object_mut(page_id) {
        d.set("Parent", Object::Reference(pages_id));
    }
    let mut catalog = lopdf::Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();
    buf
}

fn empty_page() -> RawPageElements {
    RawPageElements {
        page_index: 0,
        width: 612.0,
        height: 792.0,
        spans: vec![],
        vectors: vec![],
        image_bboxes: vec![],
        filled_rects: vec![],
        embedded_images: vec![],
    }
}

// --- Boundary: degenerate content -----------------------------------------

#[test]
fn empty_content_stream_parses_to_empty_page() {
    let pdf = pdf_with_ops(b"");
    let pages = extract_document(&pdf, "empty.pdf").unwrap();
    assert_eq!(pages.len(), 1);
    assert!(pages[0].spans.is_empty());
}

#[test]
fn whitespace_only_content_parses() {
    let pdf = pdf_with_ops(b"  \n\t % a comment\n ");
    let pages = extract_document(&pdf, "ws.pdf").unwrap();
    assert!(pages[0].spans.is_empty());
}

#[test]
fn single_empty_text_show_yields_no_panic() {
    let pdf = pdf_with_ops(b"BT /F1 11 Tf 1 0 0 1 72 700 Tm () Tj ET\n");
    let pages = extract_document(&pdf, "emptystr.pdf").unwrap();
    assert_eq!(pages.len(), 1);
    for span in &pages[0].spans {
        let b = &span.bbox;
        assert!(
            b.x0.is_finite() && b.y0.is_finite() && b.x1.is_finite() && b.y1.is_finite(),
            "non-finite bbox from empty show: {b:?}"
        );
    }
}

#[test]
fn zero_font_size_text_does_not_break_extraction() {
    let pdf = pdf_with_ops(b"BT /F1 0 Tf 1 0 0 1 72 700 Tm (invisible) Tj ET\n");
    let pages = extract_document(&pdf, "zerofont.pdf").unwrap();
    assert_eq!(pages.len(), 1);
    for span in &pages[0].spans {
        let b = &span.bbox;
        assert!(
            b.x0.is_finite() && b.y0.is_finite() && b.x1.is_finite() && b.y1.is_finite(),
            "non-finite bbox from zero font size: {b:?}"
        );
    }
}

// --- Boundary: degenerate tables ------------------------------------------

fn table(rows: Vec<Vec<&str>>) -> DetectedTable {
    DetectedTable {
        bbox: BBox::new(0.0, 0.0, 10.0, 10.0),
        rows: rows
            .into_iter()
            .map(|r| r.into_iter().map(str::to_string).collect())
            .collect(),
        has_header: false,
        source: TableSource::VectorGrid,
    }
}

#[test]
fn empty_table_renders_empty_string() {
    assert_eq!(detected_table_to_markdown(&table(vec![])), "");
}

#[test]
fn single_cell_table_renders() {
    let md = detected_table_to_markdown(&table(vec![vec!["solo"]]));
    assert!(md.contains("solo"), "lost the only cell: {md:?}");
}

#[test]
fn ragged_rows_do_not_panic() {
    let md = detected_table_to_markdown(&table(vec![
        vec!["a", "b", "c"],
        vec!["lonely"],
        vec![],
        vec!["x", "y"],
    ]));
    assert!(md.contains("lonely"));
}

#[test]
fn empty_string_cells_do_not_break_columns() {
    let md = detected_table_to_markdown(&table(vec![vec!["", ""], vec!["", ""]]));
    assert_eq!(md.lines().count(), 4, "2 rows + header + sep: {md:?}");
}

// --- Boundary: full pipeline on degenerate pages ---------------------------

#[tokio::test]
async fn assemble_empty_page_yields_no_chunks_no_panic() {
    use razorpdf_core::assembler::assemble_document;
    use razorpdf_core::vlm::MockVlmProvider;
    use razorpdf_core::ParserConfig;

    let pdf = pdf_with_ops(b"");
    let doc = assemble_document(
        &pdf,
        "empty.pdf",
        &ParserConfig::default(),
        Arc::new(MockVlmProvider::new()),
    )
    .await
    .expect("empty page must assemble");
    assert!(doc.chunks.is_empty());
}

#[test]
fn lines_and_tables_survive_zero_spans() {
    use razorpdf_core::geometry::lines::merge_spans_to_lines;
    use razorpdf_core::geometry::table_grid::detect_all_tables;

    let page = empty_page();
    let lines = merge_spans_to_lines(&page.spans);
    assert!(lines.is_empty());
    let tables = detect_all_tables(&page.vectors, &page.spans, &lines, 11.0, &page.filled_rects);
    assert!(tables.is_empty());
}

#[test]
fn triage_scores_empty_page_without_nan() {
    use razorpdf_core::triage::scorer::score_page;
    use razorpdf_core::ParserConfig;

    let page = empty_page();
    let verdict = score_page(&page, &ParserConfig::default());
    assert!(verdict.score.is_finite(), "non-finite page score");
    assert!(
        verdict.fallback_regions.iter().all(|r| r.score.is_finite()),
        "non-finite triage score on empty page"
    );
}

#[tokio::test]
async fn single_span_page_flows_end_to_end() {
    use razorpdf_core::assembler::assemble_document;
    use razorpdf_core::vlm::MockVlmProvider;
    use razorpdf_core::ParserConfig;

    let pdf = pdf_with_ops(b"BT /F1 11 Tf 1 0 0 1 72 700 Tm (one) Tj ET\n");
    let doc = assemble_document(
        &pdf,
        "one.pdf",
        &ParserConfig::default(),
        Arc::new(MockVlmProvider::new()),
    )
    .await
    .expect("single span must assemble");
    assert!(!doc.chunks.is_empty(), "single span must produce a chunk");
    assert!(doc.chunks.iter().any(|c| c.markdown.contains("one")));
}

// --- Error path: nonsense boxes ----------------------------------------------

#[test]
fn inverted_and_zero_boxes_do_not_poison_geometry() {
    let bad = BBox {
        x0: 50.0,
        y0: 50.0,
        x1: 10.0,
        y1: 10.0,
    };
    assert_eq!(bad.width(), 0.0);
    assert_eq!(bad.height(), 0.0);
    let zero = BBox::new(5.0, 5.0, 5.0, 5.0);
    assert_eq!(bad.vertical_containment_ratio(&zero), 0.0);
    assert_eq!(bad.horizontal_overlap_ratio(&zero), 0.0);
    assert_eq!(zero.vertical_containment_ratio(&zero), 0.0);
}

// --- Resource safety: temp file handling ------------------------------------

#[test]
#[cfg(unix)]
fn pdftoppm_spill_refuses_to_follow_planted_symlinks() {
    use razorpdf_core::vlm::cropper::render_page_via_pdftoppm;
    use std::os::unix::fs::symlink;

    // The spill path is predictable (`razorpdf-{pid}.{tag}.pdf`, tag from a
    // process-global counter starting at 0) inside the world-writable temp
    // dir: a local attacker can pre-plant symlinks and get arbitrary files
    // overwritten with PDF bytes (CWE-59). The spill must use O_EXCL.
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let victim = dir.join(format!("razorpdf-symlink-victim-{pid}.txt"));
    std::fs::write(&victim, b"DO NOT TOUCH").unwrap();
    let mut farm = Vec::new();
    for tag in 0..128u64 {
        let link = dir.join(format!("razorpdf-{pid}.{tag}.pdf"));
        // Another tag may already exist from a parallel caller; take the
        // first free one deterministically instead of failing.
        if std::fs::symlink_metadata(&link).is_ok() {
            continue;
        }
        symlink(&victim, &link).unwrap();
        farm.push(link);
    }
    assert!(
        !farm.is_empty(),
        "symlink farm must plant at least one link"
    );

    let pdf = pdf_with_ops(b"BT /F1 11 Tf 1 0 0 1 72 700 Tm (x) Tj ET\n");
    let result = render_page_via_pdftoppm(&pdf, 0, 72);

    let victim_after = std::fs::read(&victim).unwrap();
    for link in &farm {
        let _ = std::fs::remove_file(link);
    }
    let _ = std::fs::remove_file(&victim);

    assert_eq!(
        victim_after, b"DO NOT TOUCH",
        "spill followed a planted symlink and clobbered the target"
    );
    assert!(
        result.is_none(),
        "spill onto an occupied path must fail closed, not render"
    );
}

// --- Error path: hostile floats must not poison coordinates ------------------

#[test]
fn extreme_tm_values_yield_finite_span_bboxes() {
    // 1e999 overflows f32 -> inf; inf-inf arithmetic below must never
    // surface NaN coords (serde_json renders NaN as null: corrupt output).
    for tm in [
        "1e999 0 0 1e999 72 700 Tm",
        "-1e999 0 0 1e999 72 700 Tm",
        "0 0 0 0 72 700 Tm",
    ] {
        let ops = format!("BT /F1 11 Tf {tm} (x) Tj ET\n");
        let pdf = pdf_with_ops(ops.as_bytes());
        let pages = extract_document(&pdf, "tm.pdf").unwrap();
        for span in &pages[0].spans {
            let b = &span.bbox;
            assert!(
                b.x0.is_finite() && b.y0.is_finite() && b.x1.is_finite() && b.y1.is_finite(),
                "non-finite bbox from {tm}: {b:?}"
            );
        }
    }
}

#[test]
fn span_with_empty_text_has_sane_bbox() {
    let s = TextSpan {
        text: String::new(),
        bbox: BBox::new(72.0, 100.0, 72.0, 100.0),
        font_name: "Helvetica".to_string(),
        font_size: 11.0,
        is_bold: false,
        is_italic: false,
        is_monospace: false,
    };
    assert_eq!(s.bbox.width(), 0.0);
}
