# Project Plan: `razorpdf` — Hybrid Geometric & Sub-Page VLM Document Parser

## 0. Agent Execution Directives (For Hermes)
You are tasked with building **`razorpdf`**, an ultra-fast, local-first Rust document parser that converts PDFs into clean, LLM-ready Markdown and grounded RAG chunks. 

### Core Engineering Rules
1. **Incremental Verification:** Do not proceed to Phase $N+1$ until `cargo check --workspace` and `cargo test --workspace` pass with zero warnings in Phase $N$.
2. **Self-Contained Testing:** Do not rely on external internet downloads for unit test PDFs. Build a programmatic PDF fixture generator in `crates/razorpdf-core/src/test_fixtures.rs` (using `lopdf` or `printpdf`) that creates single-column, multi-column, bordered-table, and borderless-table PDFs in memory.
3. **Zero-Hallucination Fast Path:** At least $90\%$ of digital PDF pages must be parsed deterministically using pure Rust spatial geometry in $< 3\text{ ms/page}$ without invoking an AI model.
4. **Selective Sub-Page VLM Fallback:** Never send a full page image to a Vision-Language Model (VLM) unless the entire page is a scanned bitmap. Only crop and send specific bounding boxes $B = (x_0, y_0, x_1, y_1)$ that fail deterministic geometric reconstruction.

---

## 1. System Architecture & Workspace Layout

```text
razorpdf/
├── Cargo.toml                      # Workspace root
├── plan.md                         # This specification
├── crates/
│   ├── razorpdf-core/              # Core Rust engine (Geometry, Triage, VLM, Output)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs           # Parser & VLM configuration
│   │       ├── models.rs           # Core IR, Bounding Boxes, Output types
│   │       ├── extractor/          # PDFium/lopdf raw char, font, & vector extraction
│   │       │   ├── mod.rs
│   │       │   ├── pdfium_backend.rs
│   │       │   └── lopdf_fallback.rs
│   │       ├── geometry/           # Spatial indexing, XY-Cut, deterministic tables
│   │       │   ├── mod.rs
│   │       │   ├── bbox.rs
│   │       │   ├── xy_cut.rs
│   │       │   ├── lines.rs
│   │       │   └── table_grid.rs
│   │       ├── triage/             # Complexity scoring & sub-page region detector
│   │       │   ├── mod.rs
│   │       │   └── scorer.rs
│   │       ├── vlm/                # Sub-page WebP cropper & async VLM router
│   │       │   ├── mod.rs
│   │       │   ├── cropper.rs
│   │       │   └── client.rs
│   │       ├── emitter/            # Markdown & Grounded JSON chunk synthesizer
│   │       │   ├── mod.rs
│   │       │   ├── markdown.rs
│   │       │   └── chunks.rs
│   │       └── test_fixtures.rs    # Synthetic PDF generator for offline tests
│   ├── razorpdf-cli/               # CLI binary + Built-in MCP Server
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── commands.rs
│   │       └── mcp_server.rs       # JSON-RPC stdio MCP server for AI agents
│   └── razorpdf-py/                # PyO3 zero-copy Python bindings
│       ├── Cargo.toml
│       ├── pyproject.toml
│       └── src/
│           └── lib.rs
└── benches/
    └── throughput.rs               # Criterion benchmarks
```

### Recommended Crate Dependencies (`Cargo.toml`)
* **PDF & Graphics:** `lopdf = "0.34"`, `pdfium-render = "0.8"`, `image = { version = "0.25", features = ["webp", "png"] }`, `printpdf = "0.7"` (dev-dependency for test generation).
* **Geometry & Concurrency:** `rstar = "0.12"` (R-tree spatial index), `rayon = "1.10"` (page-level data parallelism), `tokio = { version = "1", features = ["full"] }`.
* **Networking & Serialization:** `reqwest = { version = "0.12", features = ["json", "rustls-tls"] }`, `serde = { version = "1.0", features = ["derive"] }`, `serde_json = "1.0"`, `base64 = "0.22"`.
* **CLI & Error Handling:** `clap = { version = "4.5", features = ["derive"] }`, `anyhow = "1.0"`, `thiserror = "2.0"`, `tracing = "0.1"`, `tracing-subscriber = "0.3"`.
* **Token Counting:** `tiktoken-rs = "0.6"` (or fast heuristic character-to-token estimator with optional `tiktoken` feature).

---

## 2. Core Data Structures (`crates/razorpdf-core/src/models.rs`)

Implement the following foundational Intermediate Representation (IR) types:

```rust
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
    Heading { level: u8 },
    Paragraph,
    CodeBlock,
    ListItem { ordered: bool, depth: u8 },
    DeterministicTable { rows: Vec<Vec<String>>, has_header: bool },
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
    pub breadcrumb: Vec<String>, // e.g., ["1. Introduction", "1.2 Architecture"]
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
```

---

## 3. Mathematical & Algorithmic Specifications

### 3.1 Coordinate Normalization & Bounding Box Algebra (`bbox.rs`)
PDF internal coordinates put $(0,0)$ at the bottom-left. Immediately upon extraction, transform all coordinates to top-left origin where $y_{\text{top}} = H_{\text{page}} - y_{\text{pdf}}$.
Define the following methods on `BBox`:
* **Area:** $\text{Area}(B) = \max(0.0, x_1 - x_0) \times \max(0.0, y_1 - y_0)$
* **Intersection Area:**
  $$I(B_a, B_b) = \max(0.0, \min(a.x_1, b.x_1) - \max(a.x_0, b.x_0)) \times \max(0.0, \min(a.y_1, b.y_1) - \max(a.y_0, b.y_0))$$
* **Overlap Coefficient (Szymkiewicz–Simpson):**
  $$\text{Overlap}(B_a, B_b) = \frac{I(B_a, B_b)}{\min(\text{Area}(B_a), \text{Area}(B_b))}$$
* **Vertical Line Merge Criterion:** Two `TextSpan` boxes $B_a$ and $B_b$ belong to the same horizontal text line if their vertical overlap satisfies:
  $$\frac{\min(a.y_1, b.y_1) - \max(a.y_0, b.y_0)}{\min(a.y_1 - a.y_0, b.y_1 - b.y_0)} > 0.55$$

### 3.2 Recursive XY-Cut for Multi-Column Reading Order (`xy_cut.rs`)
To correctly order multi-column academic papers, financial reports, and invoices without mixing left and right columns:
1. Project all merged line bounding boxes onto the horizontal ($X$) and vertical ($Y$) axes to create 1D occupancy histograms with bin resolution $\Delta = 2.0\text{ pt}$.
2. Find valleys (contiguous bins with zero occupancy).
3. **Horizontal Split ($Y$-Cut):** If a horizontal valley exists with height $\ge \tau_y$ (where $\tau_y = 1.2 \times \text{median\_line\_height}$), split the region into top and bottom sub-blocks and recurse on each.
4. **Vertical Split ($X$-Cut):** If a vertical valley exists with width $\ge \tau_x$ (where $\tau_x = 2.5 \times \text{median\_font\_size}$), split the region into left and right columns and recurse on each (ordered left-to-right).
5. **Base Case:** When no valid split exists, sort remaining lines top-to-bottom by $y_0$, then left-to-right by $x_0$.

### 3.3 Deterministic Vector Table Reconstruction (`table_grid.rs`)
Before considering VLM fallback, inspect `RawPageElements.vectors`:
1. Filter vector segments into horizontal lines ($|y_1 - y_0| < 1.5\text{ pt}$, length $> 15\text{ pt}$) and vertical lines ($|x_1 - x_0| < 1.5\text{ pt}$, length $> 10\text{ pt}$).
2. Cluster intersecting horizontal and vertical segments using an R-tree (`rstar`).
3. If a cluster has $\ge 3$ horizontal lines and $\ge 2$ vertical lines forming closed cells:
   * Construct a 2D grid of cell bounding boxes $C_{r,c}$.
   * Query the R-tree for all `TextSpan` items whose centroid $(c_x, c_y)$ falls inside $C_{r,c}$.
   * Emit a `BlockKind::DeterministicTable` directly in Rust.

### 3.4 Smart Triage & Sub-Page Complexity Scoring (`triage/scorer.rs`)
For every candidate block or page region $R$, compute a deterministic complexity score $S(R) \in [0.0, 1.0]$:

$$S(R) = \min\left(1.0, \; 0.40 \cdot \rho_{\text{overlap}} + 0.35 \cdot \rho_{\text{tab\_entropy}} + 0.25 \cdot \rho_{\text{vector\_noise}}\right)$$

Where:
* $\rho_{\text{overlap}}$: Fraction of `TextSpan` pairs inside $R$ where $\text{Overlap}(B_i, B_j) > 0.25$ (indicates broken PDF kerning, watermarks over tables, or stacked text layers).
* $\rho_{\text{tab\_entropy}}$: Alignment entropy for borderless tables. If $\ge 4$ consecutive lines each contain $\ge 3$ horizontally separated text islands (gaps $> 3 \times \text{font\_size}$), compute column $x_0$ variance across rows. If columns do not cleanly align into deterministic vertical buckets (e.g., merged multi-span header cells without vector grid lines), set $\rho_{\text{tab\_entropy}} = 1.0$.
* $\rho_{\text{vector\_noise}}$: Ratio of non-axis-aligned vector segments to text spans (detects embedded charts, flowcharts, and vector diagrams).
* **Scanned Page Rule:** If total character count on a page is $< 30$ and `image_bboxes` covers $> 50\%$ of page area, mark the entire page as `TriageReason::ScannedImageRegion` with $S = 1.0$.

**Triage Decision Threshold:**
* If $S(R) < 0.45$: Process via **Fast Geometric Path** ($0$ VLM tokens).
* If $S(R) \ge 0.45$: Convert block into `BlockKind::VlmFallbackRegion`, pad its bounding box $B_R$ by $8\text{ pt}$ on all sides, and queue for sub-page cropping.

---

## 4. Sub-Page Cropping & Async VLM Router (`crates/razorpdf-core/src/vlm/`)

1. **Sub-Page Renderer (`cropper.rs`):**
   * Render only the page containing the fallback region at $150\text{--}200\text{ DPI}$.
   * Crop the exact pixel rectangle corresponding to the padded bounding box $B_R$:
     $$\text{px}_0 = \left\lfloor \frac{x_0}{W_{\text{page}}} \cdot W_{\text{img}} \right\rfloor, \quad \text{py}_0 = \left\lfloor \frac{y_0}{H_{\text{page}}} \cdot H_{\text{img}} \right\rfloor$$
   * Encode the cropped sub-image into an in-memory WebP buffer (quality $80$).
2. **VLM Client (`client.rs`):**
   * Support any OpenAI-compatible Vision endpoint (`OPENAI_API_KEY` / `RAZORPDF_VLM_URL` / Ollama local `http://localhost:11434/v1/chat/completions` / Gemini / OpenRouter).
   * Execute sub-page requests concurrently across the document using a bounded `tokio::sync::Semaphore` (default max concurrency $= 8$).
   * **Strict System Prompt:**
     > "Convert the provided document region image into clean Markdown. If it is a table, output a valid Markdown table (or HTML `<table>` if cells use `rowspan`/`colspan`). If it is a diagram or chart, output a concise Mermaid.js block followed by a bulleted summary of data points. Output ONLY the raw Markdown without code fences or commentary."

---

## 5. Phased Implementation Roadmap

### Phase 1: Workspace Bootstrap & Synthetic PDF Generator
* **Tasks:**
  1. Initialize the Cargo workspace with `razorpdf-core`, `razorpdf-cli`, and `razorpdf-py`.
  2. Implement `models.rs` and `geometry/bbox.rs` with full unit tests for intersection, union, overlap, and coordinate flipping.
  3. Build `test_fixtures.rs` using `lopdf` to programmatically generate 4 in-memory test PDFs:
     * `single_col_clean.pdf` (Headings + paragraphs)
     * `two_col_academic.pdf` (Title spanning top + 2 text columns below)
     * `bordered_table.pdf` (Vector grid lines + cell text)
     * `messy_overlap_table.pdf` (Overlapping spans + irregular multi-column gaps to trigger VLM triage)
  4. Implement `extractor/lopdf_fallback.rs` (pure Rust extractor that works anywhere without external C libraries) and optional `pdfium_backend.rs` behind a feature flag.
* **Verification Gate 1:** Run `cargo test --package razorpdf-core` and verify that all 4 synthetic PDFs extract raw `TextSpan` and `VectorSegment` coordinates accurately.

### Phase 2: Geometric Engine (XY-Cut, Headings & Deterministic Tables)
* **Tasks:**
  1. Implement horizontal line merging and font-size distribution analysis (compute median body font size $\mu_{\text{font}}$; classify lines with $\text{size} \ge 1.15 \cdot \mu_{\text{font}}$ or bold isolated lines as `BlockKind::Heading`).
  2. Implement `xy_cut.rs` to recursively partition multi-column pages into proper reading order.
  3. Implement `table_grid.rs` using `rstar` to detect vector grids and snap text spans into Markdown tables.
* **Verification Gate 2:** Unit tests must prove that `two_col_academic.pdf` outputs Column 1 completely before Column 2, and `bordered_table.pdf` produces an exact Markdown table with $0$ VLM calls.

### Phase 3: Smart Triage Scorer & Sub-Page Cropper
* **Tasks:**
  1. Implement `triage/scorer.rs` with the mathematical formula for $S(R)$.
  2. Implement `vlm/cropper.rs` to crop a `BBox` region from a rendered page or synthetic raster canvas and encode it as a base64 WebP data URI.
  3. Add a `MockVlmProvider` trait implementation for deterministic offline unit testing.
* **Verification Gate 3:** Verify that `single_col_clean.pdf` and `bordered_table.pdf` score $S(R) < 0.45$ (Fast Path), while `messy_overlap_table.pdf` triggers `BlockKind::VlmFallbackRegion` on the exact table bounding box.

### Phase 4: Output Synthesizer (Markdown + Grounded RAG Chunks)
* **Tasks:**
  1. Implement `emitter/markdown.rs` to stitch fast-path blocks and resolved VLM fallback regions into a single document in reading order.
  2. Implement `emitter/chunks.rs` to track heading hierarchy breadcrumbs (`H1 > H2 > H3`), attach `[page, x0, y0, x1, y1]` bounding boxes, and compute token counts per chunk.
* **Verification Gate 4:** Verify JSON serialization of `ParsedDocument` and ensure every `GroundedChunk` contains non-empty text, valid coordinates, and accurate heading breadcrumbs.

### Phase 5: CLI, Built-in MCP Server & Python Bindings
* **Tasks:**
  1. Build `razorpdf-cli` with subcommands:
     * `razorpdf parse <input.pdf> [-o out.md] [--json out.json] [--vlm-url ...]`
     * `razorpdf inspect <input.pdf>` (Prints per-page complexity scores $S(R)$ and triage decisions in a terminal table)
     * `razorpdf mcp` (Starts a Model Context Protocol JSON-RPC 2.0 server over `stdin`/`stdout` exposing tools: `parse_pdf`, `inspect_pdf_layout`, and `extract_pdf_tables`).
  2. Build `razorpdf-py` using `PyO3` exposing `razorpdf.parse_file(path, vlm_url=None) -> dict` and `razorpdf.to_markdown(path) -> str`.
* **Verification Gate 5:** Run an integration test that spawns `razorpdf mcp`, sends a JSON-RPC `tools/call` request for `parse_pdf`, and asserts a valid structured response.

### Phase 6: Benchmark Suite & Launch Assets
* **Tasks:**
  1. Create `benches/throughput.rs` measuring pages/second on 100-page synthetic multi-column documents.
  2. Generate a clean `README.md` featuring architecture ASCII diagrams, benchmark comparison tables, CLI usage examples, MCP configuration snippet for Claude Code / OpenCode / Hermes, and Python snippet.
* **Verification Gate 6:** Full `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` pass cleanly.