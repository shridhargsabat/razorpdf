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
    /// Filled rectangles (`re` + fill paint): shaded cells, heatmaps.
    pub filled_rects: Vec<BBox>,
    /// Placed raster images with decodable pixels (a subset of
    /// `image_bboxes`: unsupported encodings keep their bbox only).
    pub embedded_images: Vec<EmbeddedImage>,
    /// Hyperlink annotations (`/Subtype /Link` with `/A → /URI`) in
    /// top-left point space.
    pub link_annotations: Vec<PdfLinkAnnotation>,
    /// Interactive form field states (`/AcroForm /Fields`).
    pub form_fields: Vec<FormFieldEntry>,
}

/// A hyperlink annotation: clickable region plus destination URI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfLinkAnnotation {
    pub bbox: BBox,
    pub uri: String,
}

/// One interactive form field (`/Tx` text, `/Btn` button/checkbox,
/// `/Ch` choice) with its current value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormFieldEntry {
    pub name: String,
    pub value: String,
    pub field_type: String,
    pub bbox: BBox,
    /// 0-based page index (catalog orphans without widgets land on 0).
    pub page: u32,
}

/// Stream codec of an embedded image's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageCodec {
    /// DCTDecode: bytes are a complete JPEG stream.
    Jpeg,
    /// FlateDecode: inflate, then read samples per `PixelLayout`.
    Flate,
    /// No filter: raw samples per `PixelLayout`.
    Raw,
}

/// Sample layout of (inflated) image bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelLayout {
    /// 8-bit RGB triplets (`3 * w * h` bytes).
    Rgb8,
    /// 8-bit gray samples (`w * h` bytes).
    Gray8,
    /// 1-bit gray samples, MSB first (`ceil(w / 8) * h` bytes, 1 = white).
    Gray1,
}

/// A placed raster image with lazily decodable pixels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedImage {
    /// Placed bbox in top-left point space (axis-aligned union of the
    /// CTM-mapped unit square).
    pub bbox: BBox,
    pub width: u32,
    pub height: u32,
    pub codec: ImageCodec,
    pub layout: PixelLayout,
    /// Stream bytes with ASCII armor stripped (still coded per `codec`).
    pub bytes: Vec<u8>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriageReason {
    OverlappingSpans,
    BorderlessIrregularTable,
    ScannedImageRegion,
    ComplexMathOrDiagram,
    CorruptedCMapOrMojibake,
    MathGlyphCluster,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutBlock {
    pub id: String,
    pub page_index: usize,
    pub bbox: BBox,
    pub kind: BlockKind,
    pub raw_text: String,
    pub reading_order: usize,
    /// Page/bbox provenance spans (first entry is always this block's own
    /// page; cross-page stitching appends continuation pages).
    pub page_spans: Vec<PageBBox>,
}

/// One page/bbox provenance span for stitched content.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PageBBox {
    pub page: u32,
    pub bbox: BBox,
}

/// One bottom-of-page footnote definition linked from body text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FootnoteItem {
    /// Citation marker as printed (`1`, `[1]`, `*`, `†`).
    pub marker: String,
    /// Footnote body text (marker stripped).
    pub text: String,
    /// 0-based page index of the definition.
    pub page: u32,
    pub bbox: BBox,
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
    /// Page/bbox provenance (first entry is the chunk's own page; table
    /// sub-chunks and stitched content append continuation pages).
    pub page_spans: Vec<PageBBox>,
    /// Footnotes cited by this chunk's markdown (`[^marker]` refs).
    pub footnotes: Vec<FootnoteItem>,
    /// Coarse layout kind from the originating block
    /// (`heading`/`paragraph`/`code`/`list`/`table`/`vlm_region`).
    pub block_kind: String,
    /// Triage complexity: VLM-region score for `vlm_region` chunks, else
    /// the owning page's triage score.
    pub triage_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedDocument {
    pub source_path: String,
    pub total_pages: usize,
    pub fast_path_pages: usize,
    pub vlm_fallback_regions: usize,
    pub elapsed_ms: u64,
    pub markdown: String,
    pub chunks: Vec<GroundedChunk>,
    /// Interactive form field states across all pages (`/AcroForm`).
    pub form_fields: Vec<FormFieldEntry>,
    /// Bottom-of-page footnote definitions in page order.
    pub footnotes: Vec<FootnoteItem>,
    /// Merged text lines (post-LaTeX-rewrite) for tight schema provenance.
    pub text_lines: Vec<LineCandidate>,
}

/// One text line retained for tight schema-extraction provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineCandidate {
    pub text: String,
    pub page: u32,
    pub bbox: BBox,
}

/// One JSON-Schema property matched to document content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaFieldMatch {
    pub field_name: String,
    pub value: serde_json::Value,
    pub confidence: f32,
    pub page: u32,
    pub bbox: BBox,
    /// "acroform" | "inline_kv" | "spatial_kv" | "table_column".
    pub source: String,
}

/// Structured extraction result with per-field provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaExtractionResult {
    pub data: serde_json::Value,
    pub provenance: Vec<SchemaFieldMatch>,
    pub overall_confidence: f32,
}

/// Structure-of-Arrays chunk batch for Arrow/Polars/LanceDB ingestion.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnarChunkBatch {
    pub chunk_id: Vec<String>,
    pub page: Vec<u32>,
    pub bbox_x0: Vec<f32>,
    pub bbox_y0: Vec<f32>,
    pub bbox_x1: Vec<f32>,
    pub bbox_y1: Vec<f32>,
    pub token_count: Vec<usize>,
    pub triage_score: Vec<f32>,
    pub block_kind: Vec<String>,
    pub section_path: Vec<String>,
    pub text: Vec<String>,
}

/// Revision status of one diffed block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffStatus {
    Unchanged,
    Modified,
    Added,
    Removed,
}

/// One aligned block comparison between two document revisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockDiff {
    pub status: DiffStatus,
    pub section_path: String,
    pub block_kind: String,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub old_page: Option<u32>,
    pub new_page: Option<u32>,
    pub old_bbox: Option<BBox>,
    pub new_bbox: Option<BBox>,
    /// Token Jaccard similarity in [0.0, 1.0] (1.0 when unchanged).
    pub similarity: f32,
    /// Word-level (or table-row-level) delta summary.
    pub delta_summary: String,
}

/// Structural comparison of two document revisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentDiff {
    pub old_path: String,
    pub new_path: String,
    pub blocks: Vec<BlockDiff>,
    pub unchanged: usize,
    pub modified: usize,
    pub added: usize,
    pub removed: usize,
}
