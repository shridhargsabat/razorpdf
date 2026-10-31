//! Phase 3 verification gates: triage fast/vlm routing over fixtures.

use razorpdf_core::{
    extractor::lopdf_fallback, geometry, models::TriageReason, test_fixtures, triage, ParserConfig,
};

/// Gate 3a: clean pages score below threshold with zero fallback regions.
#[test]
fn gate3_clean_pages_take_fast_path() {
    let config = ParserConfig::default();
    for (name, pdf) in [
        ("single", test_fixtures::single_col_clean_pdf()),
        ("two_col", test_fixtures::two_col_academic_pdf()),
        ("bordered", test_fixtures::bordered_table_pdf()),
    ] {
        let page = lopdf_fallback::extract_page(&pdf, 0).expect("extract");
        let verdict = triage::score_page(&page, &config);
        assert!(
            verdict.score < config.triage_threshold,
            "{name}: S={} must be < {}",
            verdict.score,
            config.triage_threshold
        );
        assert!(verdict.fallback_regions.is_empty(), "{name}: no regions");
        assert!(!verdict.is_scanned);
    }

    // The bordered page IS a table — and still geometrically clean.
    let pdf = test_fixtures::bordered_table_pdf();
    let page = lopdf_fallback::extract_page(&pdf, 0).expect("extract");
    let tables = geometry::detect_vector_tables(&page.vectors, &page.spans, &page.filled_rects);
    assert_eq!(tables.len(), 1);
}

/// Gate 3b: the messy page triggers exactly one region on the table bbox.
#[test]
fn gate3_messy_triggers_table_region() {
    let config = ParserConfig::default();
    let pdf = test_fixtures::messy_overlap_table_pdf();
    let page = lopdf_fallback::extract_page(&pdf, 0).expect("extract");
    let verdict = triage::score_page(&page, &config);

    assert!(
        verdict.score >= config.triage_threshold,
        "page S={} must trip {}",
        verdict.score,
        config.triage_threshold
    );
    // 0.40 * (30/33) + 0.35 * 1.0
    assert!(
        (verdict.score - 0.7136).abs() < 0.002,
        "S={}",
        verdict.score
    );
    assert_eq!(verdict.fallback_regions.len(), 1);

    let region = &verdict.fallback_regions[0];
    assert!(matches!(
        region.reason,
        TriageReason::BorderlessIrregularTable
    ));
    assert!((region.score - 0.75).abs() < 1e-6, "S={}", region.score);

    // Exact island-run bbox: x 56..473.45, y 190..265.
    let content = &region.content_bbox;
    assert!((content.x0 - 56.0).abs() < 0.5, "x0={}", content.x0);
    assert!((content.y0 - 190.0).abs() < 0.1, "y0={}", content.y0);
    assert!((content.x1 - 473.45).abs() < 0.5, "x1={}", content.x1);
    assert!((content.y1 - 265.0).abs() < 0.1, "y1={}", content.y1);

    // Crop bbox = content padded by 8pt, clamped to the page.
    let padded = content.pad_clamped(8.0, page.width, page.height);
    assert_eq!(region.bbox, padded);
}
