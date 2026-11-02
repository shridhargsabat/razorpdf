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
    pub elapsed_ms: u64,
    pub markdown: String,
    pub chunks: Vec<GroundedChunk>,
}
