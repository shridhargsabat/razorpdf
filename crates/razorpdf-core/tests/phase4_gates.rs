//! Phase 4 verification gates: hybrid assembly + grounded chunks.

use std::sync::Arc;

use razorpdf_core::{
    assembler, test_fixtures,
    vlm::{MockVlmProvider, NoVlmProvider},
    ParserConfig,
};

/// Gate 4a: every chunk has non-empty text, valid coords, true breadcrumbs.
#[tokio::test]
async fn gate4_chunks_are_grounded() {
    let pdf = test_fixtures::single_col_clean_pdf();
    let doc = assembler::assemble_document(
        &pdf,
        "single.pdf",
        &ParserConfig::default(),
        Arc::new(MockVlmProvider::new()),
    )
    .await
    .expect("assemble");

    assert_eq!(doc.total_pages, 1);
    assert_eq!(doc.fast_path_pages, 1);
    assert_eq!(doc.vlm_fallback_regions, 0);
    assert_eq!(doc.chunks.len(), 17);

    for chunk in &doc.chunks {
        assert!(!chunk.markdown.trim().is_empty(), "{}", chunk.chunk_id);
        assert!(chunk.token_estimate > 0, "{}", chunk.chunk_id);
        assert!(!chunk.parsed_by_vlm);
        let b = &chunk.bbox;
        assert!(b.x0 <= b.x1 && b.y0 <= b.y1, "{}", chunk.chunk_id);
        assert!(b.x0 >= 0.0 && b.x1 <= 612.0, "{}", chunk.chunk_id);
        assert!(b.y0 >= 0.0 && b.y1 <= 792.0, "{}", chunk.chunk_id);
    }

    assert!(doc.chunks[0].breadcrumb.is_empty());
    assert_eq!(doc.chunks[0].markdown, "# RazorPDF Benchmark Document\n\n");
    assert_eq!(
        doc.chunks[1].breadcrumb,
        vec!["RazorPDF Benchmark Document"]
    );
    assert_eq!(doc.chunks[1].markdown, "### 1. Introduction\n\n");
    assert_eq!(
        doc.chunks[2].breadcrumb,
        vec!["RazorPDF Benchmark Document", "1. Introduction"]
    );
    let method = doc
        .chunks
        .iter()
        .find(|c| c.markdown.contains("2. Method"))
        .expect("method heading");
    assert_eq!(method.breadcrumb, vec!["RazorPDF Benchmark Document"]);

    // ParsedDocument JSON round-trips with chunks intact.
    let json = serde_json::to_string(&doc).expect("serialize");
    let back: razorpdf_core::ParsedDocument = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.chunks.len(), 17);
    assert_eq!(back.markdown, doc.markdown);
}

/// Gate 4b: messy regions resolve through the provider; failures degrade.
#[tokio::test]
async fn gate4_messy_region_resolves_with_mock() {
    let pdf = test_fixtures::messy_overlap_table_pdf();
    let doc = assembler::assemble_document(
        &pdf,
        "messy.pdf",
        &ParserConfig::default(),
        Arc::new(MockVlmProvider::new()),
    )
    .await
    .expect("assemble");

    assert_eq!(doc.vlm_fallback_regions, 1);
    assert_eq!(doc.fast_path_pages, 0);
    assert!(doc.markdown.contains("mock_col_a"));
    assert!(doc.markdown.contains("Messy Invoice Scan"));

    // Title + 2 paragraphs + 1 resolved region.
    assert_eq!(doc.chunks.len(), 4);
    let resolved: Vec<_> = doc.chunks.iter().filter(|c| c.parsed_by_vlm).collect();
    assert_eq!(resolved.len(), 1);
    assert!(resolved[0].markdown.contains("mock_col_a"));
    assert_eq!(resolved[0].breadcrumb, vec!["Messy Invoice Scan"]);
    assert!((resolved[0].bbox.x0 - 56.0).abs() < 0.5);
    assert!((resolved[0].bbox.y0 - 190.0).abs() < 0.1);
}

/// Gate 4c: without a provider the region keeps raw text (nothing dropped).
#[tokio::test]
async fn gate4_null_vlm_keeps_raw_text() {
    let pdf = test_fixtures::messy_overlap_table_pdf();
    let doc = assembler::assemble_document(
        &pdf,
        "messy.pdf",
        &ParserConfig::default(),
        Arc::new(NoVlmProvider),
    )
    .await
    .expect("assemble");

    assert_eq!(doc.vlm_fallback_regions, 1);
    assert!(doc.markdown.contains("Item0"));
    assert!(doc.chunks.iter().all(|c| !c.parsed_by_vlm));
    assert!(doc.chunks.iter().all(|c| !c.markdown.trim().is_empty()));
}
