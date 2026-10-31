//! Core Intermediate Representation (IR) types for razorpdf.
//!
//! All bounding boxes use top-left origin: (0,0) is the top-left of the page,
//! with `x0 <= x1` and `y0 <= y1`, in PDF points (1/72 inch).

use serde::{Deserialize, Serialize};

/// Normalized or Point-based Bounding Box: (x0, y0, x1, y1)
/// Origin (0,0) is top-left of the page; x0 <= x1 and y0 <= y1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSpan {
    pub text: String,
    pub bbox: BBox,
    pub font_name: String,
    pub font_size: f32,
    pub is_bold: bool,
    pub is_italic: bool,
    pub is_monospace: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct VectorSegment {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub stroke_width: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPageElements {
    pub page_index: usize,
    pub width: f32,
    pub height: f32,
    pub spans: Vec<TextSpan>,
    pub vectors: Vec<VectorSegment>,
    pub image_bboxes: Vec<BBox>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlockKind {
    Heading {
        level: u8,
    },
    Paragraph,
    CodeBlock,
    ListItem {
        ordered: bool,
        depth: u8,
    },
    DeterministicTable {
        rows: Vec<Vec<String>>,
        has_header: bool,
    },
    /// Triggered when geometric triage detects an unresolvable table/figure/scan
    VlmFallbackRegion {
        reason: TriageReason,
        complexity_score: f32,
        resolved_markdown: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TriageReason {
    OverlappingSpans,
    BorderlessIrregularTable,
    ScannedImageRegion,
    ComplexMathOrDiagram,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutBlock {
    pub id: String,
    pub page_index: usize,
    pub bbox: BBox,
    pub kind: BlockKind,
    pub raw_text: String,
    pub reading_order: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundedChunk {
    pub chunk_id: String,
    pub page_index: usize,
    pub bbox: BBox,
    /// e.g., ["1. Introduction", "1.2 Architecture"]
    pub breadcrumb: Vec<String>,
    pub markdown: String,
    pub token_estimate: usize,
    pub parsed_by_vlm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedDocument {
    pub source_path: String,
    pub total_pages: usize,
    pub fast_path_pages: usize,
    pub vlm_fallback_regions: usize,
    pub elapsed_ms: u128,
    pub markdown: String,
    pub chunks: Vec<GroundedChunk>,
}
