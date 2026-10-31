//! Phase 2 verification gates over the synthetic fixtures.

use razorpdf_core::{extractor::lopdf_fallback, geometry, test_fixtures, ParserConfig};

/// Gate 2a: two-column PDF reads column 1 fully before column 2.
#[test]
fn gate2_two_col_reading_order() {
    let pdf = test_fixtures::two_col_academic_pdf();
    let pages = lopdf_fallback::extract_document(&pdf, "two_col").expect("extract");
    assert_eq!(pages.len(), 1);

    let lines = geometry::merge_spans_to_lines(&pages[0].spans);
    assert_eq!(lines.len(), 33, "title + 32 column lines");

    let order = geometry::reading_order(&lines, &ParserConfig::default());
    let texts: Vec<String> = order.iter().map(|&i| lines[i].text.clone()).collect();

    assert_eq!(texts[0], "Advances in Geometric Parsing");
    for r in 0..16 {
        assert_eq!(texts[1 + r], format!("Left line {r:02} abcdef"));
        assert_eq!(texts[17 + r], format!("Right line {r:02} ghijkl"));
    }
}

/// Gate 2b: bordered table yields an exact Markdown table, 0 VLM calls.
#[test]
fn gate2_bordered_table_exact_markdown() {
    let pdf = test_fixtures::bordered_table_pdf();
    let page = lopdf_fallback::extract_page(&pdf, 0).expect("extract");

    let tables = geometry::detect_vector_tables(&page.vectors, &page.spans);
    assert_eq!(tables.len(), 1);
    assert!(tables[0].has_header);

    let md = geometry::table_grid::detected_table_to_markdown(&tables[0]);
    let expected = "\
| Name | Qty | Price |\n\
| --- | --- | --- |\n\
| Apples | 12 | 1.20 |\n\
| Bread | 3 | 2.50 |\n\
| Milk | 2 | 3.10 |\n";
    assert_eq!(md, expected);
}

/// Gate 2c: single-column headings classified by font-size analysis.
#[test]
fn gate2_single_col_headings() {
    let pdf = test_fixtures::single_col_clean_pdf();
    let page = lopdf_fallback::extract_page(&pdf, 0).expect("extract");

    let lines = geometry::merge_spans_to_lines(&page.spans);
    assert_eq!(lines.len(), 17);

    let median = geometry::median_font_size(&lines);
    assert!((median - 11.0).abs() < 1e-6);

    let headings: Vec<&str> = lines
        .iter()
        .filter(|l| geometry::classify_heading(l, median))
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        headings,
        vec![
            "RazorPDF Benchmark Document",
            "1. Introduction",
            "2. Method",
        ]
    );
}
