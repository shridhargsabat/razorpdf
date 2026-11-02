# razorpdf v0.2.0 Implementation Plan: Next-Generation Hybrid PDF Engine

**Repository:** [https://github.com/shridhargsabat/razorpdf](https://github.com/shridhargsabat/razorpdf)  
**Base Version:** `0.1.6` (Rust 2021 Edition Workspace: `razorpdf-core`, `razorpdf`, `razorpdf-py`)  
**Target Version:** `0.2.0`  
**Execution Agent:** Agent Hermes  

## 1. Executive Architecture & Strategic Objectives

[razorpdf](https://github.com/shridhargsabat/razorpdf) currently combines deterministic Rust geometry (recursive XY-cut layout decomposition and R-tree vector table extraction via [rstar](https://crates.io/crates/rstar)) with selective sub-page WebP cropping ($S(R) \ge 0.45$) for Vision Language Models (VLMs). While this architecture achieves ~16,720 synthetic pages/sec and ~339 real-world pages/sec, outperforming heavy vision pipelines like [Docling](https://github.com/docling-project/docling) and [Marker](https://github.com/datalab-to/marker) as well as CPU heuristics like [PyMuPDF4LLM](https://github.com/pymupdf/pymupdf4llm) and [pdfplumber](https://github.com/jsvine/pdfplumber) requires closing six specific technical gaps:

1. **Corrupted CMap & Mathematical Glyph Triage:** Detecting garbled `ToUnicode` maps (Mojibake) and mathematical symbol fonts (`CMMI`, `CMSY`, `CambriaMath`) during extraction so they route to tight sub-page WebP crops rather than emitting corrupted text.
2. **Cross-Page Continuity Engine:** Stitching multi-page tables and split paragraphs across page boundaries using R-tree column coordinate alignment and header deduplication.
3. **Structure-Preserving Token Chunking & Visual Citations:** Repeating Markdown table headers across split chunks using [tiktoken-rs](https://github.com/zurawiki/tiktoken-rs) and rendering highlighted `[x0, y0, x1, y1]` citation overlays onto WebP previews.
4. **Content-Addressed VLM Caching & Local ONNX Micro-Tier:** Caching cropped WebP VLM responses via `blake3` digests and adding an optional local [ort (ONNX Runtime)](https://github.com/pykeio/ort) inference tier for sub-second offline formula and table recognition.
5. **Stateful Progressive-Disclosure MCP Server:** Expanding `razorpdf mcp` with table-of-contents inspection (`get_pdf_toc`), in-memory BM25 chunk retrieval (`search_pdf_chunks`), page-range reading (`read_pdf_page_range`), and on-demand visual region cropping (`render_pdf_region`).
6. **Python Zero-Copy / Schema Bindings & OmniDocBench Harness:** Exposing Pydantic/JSON-Schema extraction and visual citation rendering in `razorpdf-py`, validated against [OmniDocBench](https://github.com/opendatalab/OmniDocBench).

## 2. Workspace File Modification Matrix

| Crate / Directory | File Path | Action | Summary of Changes |
| :--- | :--- | :--- | :--- |
| **Root Workspace** | `Cargo.toml` | Modify | Bump version to `0.2.0`; add `blake3`, `unicode-normalization`, and optional `ort` workspace dependencies |
| **`razorpdf-core`** | `crates/razorpdf-core/src/models.rs` | Modify | Add `TriageReason::CorruptedCMapOrMojibake`, `TriageReason::MathGlyphCluster`, `PdfAnnotation`, `FormField`, and multi-page `ChunkSpan` |
| **`razorpdf-core`** | `crates/razorpdf-core/src/config.rs` | Modify | Add `enable_cross_page_stitching`, `enable_vlm_cache`, `vlm_cache_dir`, `repeat_table_headers_in_chunks`, and `json_schema` options |
| **`razorpdf-core`** | `crates/razorpdf-core/src/extractor/fonts.rs` | Modify | Implement `analyze_span_anomaly()` for CMap gibberish ratio and math font/subscript detection |
| **`razorpdf-core`** | `crates/razorpdf-core/src/extractor/lopdf_fallback.rs` | Modify | Extract `/Annots` (`/URI` links) and `/AcroForm` interactive field states; harden CID font width parsing |
| **`razorpdf-core`** | `crates/razorpdf-core/src/extractor/pdfium_backend.rs` | Modify | Extract hyperlink annotations and form widgets alongside glyph spans |
| **`razorpdf-core`** | `crates/razorpdf-core/src/triage/scorer.rs` | Modify | Integrate $p_{\text{font\_anomaly}}$ and $p_{\text{math\_density}}$ into `compute_region_triage_score()` |
| **`razorpdf-core`** | `crates/razorpdf-core/src/geometry/mod.rs` | Modify | Register new `cross_page` module |
| **`razorpdf-core`** | `crates/razorpdf-core/src/geometry/cross_page.rs` | **Create** | Implement `stitch_cross_page_tables()` and `merge_cross_page_paragraphs()` across consecutive pages |
| **`razorpdf-core`** | `crates/razorpdf-core/src/vlm/mod.rs` | Modify | Register `cache` and optional `onnx_tier` modules |
| **`razorpdf-core`** | `crates/razorpdf-core/src/vlm/cache.rs` | **Create** | Implement `Blake3VlmCache` for content-addressed sub-page WebP + prompt caching on disk |
| **`razorpdf-core`** | `crates/razorpdf-core/src/vlm/cropper.rs` | Modify | Add `render_citation_overlay()` drawing bounding-box highlights on cropped WebP pages |
| **`razorpdf-core`** | `crates/razorpdf-core/src/emitter/chunks.rs` | Modify | Implement structure-preserving table row splitting with repeated Markdown headers and exact `tiktoken-rs` counts |
| **`razorpdf-core`** | `crates/razorpdf-core/src/assembler.rs` | Modify | Wire cross-page stitching, VLM cache lookup, and optional JSON Schema extraction into the pipeline |
| **`razorpdf` (CLI/MCP)** | `crates/razorpdf/src/mcp_server.rs` | Modify | Add `get_pdf_toc`, `search_pdf_chunks`, `read_pdf_page_range`, and `render_pdf_region` MCP tools |
| **`razorpdf` (CLI/MCP)** | `crates/razorpdf/src/commands.rs` | Modify | Add `--cite-bbox`, `--schema`, and `--no-cache` CLI flags |
| **`razorpdf-py`** | `crates/razorpdf-py/src/lib.rs` | Modify | Expose `render_citation()`, `search_chunks()`, and `extract_schema()` with GIL release |
| **Benchmarks** | `scripts/bench_omnidoc.py` | **Create** | Automated evaluation harness for reading order, Table TEDS, and throughput on [OmniDocBench](https://github.com/opendatalab/OmniDocBench) |

## 3. Step-by-Step Implementation Phases

### Phase 1: Font/CMap Corruption Detection, Math Triage & Native Annotations

#### 1.1 Goal
Prevent silent gibberish output on PDFs with broken `ToUnicode` CMaps or unmapped Type3 fonts, accurately route inline/display mathematical equations to WebP micro-crops, and preserve interactive hyperlinks and form fields.

#### 1.2 Target Files & Implementation Details
1. **`crates/razorpdf-core/src/models.rs`**:
   * Extend `TriageReason` enum:
     ```rust
     #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
     pub enum TriageReason {
         OverlappingSpans,
         BorderlessIrregularTable,
         ScannedImageRegion,
         ComplexMathOrDiagram,
         CorruptedCMapOrMojibake,
         MathGlyphCluster,
     }
     ```
   * Add `PdfLinkAnnotation { pub bbox: BBox, pub uri: String }` and `FormFieldEntry { pub name: String, pub value: String, pub field_type: String, pub bbox: BBox }`.

2. **`crates/razorpdf-core/src/extractor/fonts.rs`**:
   * Implement `SpanAnomalyMetrics`:
     ```rust
     pub struct SpanAnomalyMetrics {
         pub mojibake_ratio: f32,
         pub math_glyph_ratio: f32,
         pub has_sub_or_superscript_offset: bool,
     }
     ```
   * Flag fonts matching `CMMI`, `CMSY`, `CMEX`, `MSBM`, `CambriaMath`, `LatinModernMath`, or `Symbol`.
   * Compute `mojibake_ratio` as the fraction of characters in a span that are `\u{FFFD}` (replacement char), C0/C1 control codes (excluding `\t`, `\n`, `\r`), Private Use Area codepoints (`\u{E000}..=\u{F8FF}`), or impossible consonant/symbol clusters.

3. **`crates/razorpdf-core/src/triage/scorer.rs`**:
   * Update `compute_region_triage_score()` so that:
     * Any region with `mojibake_ratio > 0.08` forces $S(R) = \max(S(R), 0.75)$ with `TriageReason::CorruptedCMapOrMojibake`.
     * Any region with `math_glyph_ratio > 0.15` or dense vertical baseline oscillations (subscripts/superscripts) forces $S(R) = \max(S(R), 0.55)$ with `TriageReason::MathGlyphCluster`, routing the tight equation bounding box to the VLM with a specialized LaTeX prompt (`"Convert this cropped mathematical expression into clean LaTeX wrapped in $$...$$ or $...$"`).

4. **`crates/razorpdf-core/src/extractor/lopdf_fallback.rs` & `pdfium_backend.rs`**:
   * Extract `/Subtype /Link` annotations with `/A -> /URI` and inject `[anchor text](uri)` into overlapping `TextSpan` items during Markdown emission.
   * Extract `/AcroForm` `/Fields` (`/FT /Tx`, `/Btn`, `/Ch`) so checkboxes and form inputs emit as structured Markdown (`- [x] Field: Value`).

#### 1.3 Phase 1 Verification
* Run `cargo test -p razorpdf-core extractor::` and `cargo test -p razorpdf-core triage::`.
* Add unit tests in `crates/razorpdf-core/src/triage/scorer.rs` verifying that synthetic spans containing `\u{FFFD}` or `CMMI10` font tags produce $S(R) \ge 0.55$.

### Phase 2: Cross-Page Continuity Engine (Multi-Page Tables & Paragraphs)

#### 2.1 Goal
Eliminate artificial table splits and broken sentences at page boundaries—a failure mode present in [PyMuPDF4LLM](https://github.com/pymupdf/pymupdf4llm) and [pdfplumber](https://github.com/jsvine/pdfplumber).

#### 2.2 Target Files & Implementation Details
1. **`crates/razorpdf-core/src/geometry/cross_page.rs` (New File)**:
   * Implement `stitch_across_pages(pages: &mut [PageLayout], config: &ParserConfig)`:
     ```rust
     pub struct CrossPageStitchStats {
         pub tables_stitched: usize,
         pub headers_deduplicated: usize,
         pub paragraphs_merged: usize,
     }
     ```
   * **Multi-Page R-Tree Table Stitching Algorithm**:
     1. Locate the last content block (ignoring stripped footers/page numbers) on page $N$ and the first content block (ignoring stripped running headers) on page $N+1$.
     2. If both blocks are `BlockKind::DeterministicTable`:
        * Compare column count $C_N$ and $C_{N+1}$, and compute average 1D horizontal Intersection-over-Union (IoU) across column X-intervals `[col_x0, col_x1]`.
        * If $C_N == C_{N+1}$ and mean column X-IoU $\ge 0.85$:
          * Check if row 0 of the table on page $N+1$ is a verbatim or normalized string match of row 0 (the header row) of the table on page $N$. If so, remove the duplicate header row on page $N+1$.
          * Check if the first remaining row on page $N+1$ has empty leading cells (e.g., column 0 is blank while column 1+ continues text from the bottom row of page $N$). Merge those continuation cells into the final row of page $N$.
          * Append all remaining rows from page $N+1$ into the table on page $N$ and record both page bounding boxes in `ChunkSpan`.
   * **Cross-Page Paragraph & Hyphenation Re-Flow**:
     1. If the final block on page $N$ is `BlockKind::Paragraph` ending without terminal punctuation (`.`, `?`, `!`, `:`, `;`, `"`, `”`) and the first non-header block on page $N+1$ is `BlockKind::Paragraph` starting with a lowercase letter (or matching font size within $\pm 0.5\text{pt}$):
     2. De-hyphenate words split by a trailing `-` at the end of page $N$ and concatenate the spans into a unified cross-page paragraph block.

2. **`crates/razorpdf-core/src/assembler.rs`**:
   * Invoke `geometry::cross_page::stitch_across_pages(&mut page_layouts, &self.config)` immediately after parallel per-page XY-cut and R-tree table extraction completes, prior to Markdown and RAG chunk emission.

#### 2.3 Phase 2 Verification
* Add integration test `test_cross_page_table_stitching_and_header_dedup` in `crates/razorpdf-core/src/geometry/cross_page.rs` testing a 2-page financial invoice table with repeated headers and a split description cell.
* Run `cargo test -p razorpdf-core geometry::cross_page`.

### Phase 3: Structure-Preserving Token Chunking, Visual Citations & Schema Mode

#### 3.1 Goal
Ensure every emitted `GroundedChunk` is self-contained for vector retrieval (never emitting orphaned table rows without column headers) and can be visually verified with a single function call.

#### 3.2 Target Files & Implementation Details
1. **`crates/razorpdf-core/src/models.rs`**:
   * Update `GroundedChunk`:
     ```rust
     #[derive(Debug, Clone, Serialize, Deserialize)]
     pub struct PageBBox {
         pub page: u32,
         pub bbox: BBox,
     }

     #[derive(Debug, Clone, Serialize, Deserialize)]
     pub struct GroundedChunk {
         pub chunk_id: String,
         pub text: String,
         pub token_count: usize,
         pub page: u32,
         pub bbox: BBox,
         pub page_spans: Vec<PageBBox>,
         pub section_path: Vec<String>,
         pub block_kind: String,
         pub triage_score: f32,
     }
     ```

2. **`crates/razorpdf-core/src/emitter/chunks.rs`**:
   * Use `tiktoken-rs` (`cl100k_base` or `o200k_base` singleton initialized via `std::sync::OnceLock` so BPE initialization adds zero per-call overhead) to compute exact token counts.
   * **Header-Preserving Table Chunk Splitter**:
     * When a `DeterministicTable` or VLM table exceeds `config.max_chunk_tokens` (default `512`):
     * Extract the Markdown header row (`| Col1 | Col2 |...`) and alignment row (`| --- | --- |...`) and compute their token cost $T_{\text{header}}$.
     * Greedily pack table body rows until $T_{\text{header}} + \sum T_{\text{row}} \le \text{max\_chunk\_tokens}$.
     * Prepend the Markdown header and separator rows to **every** emitted table sub-chunk, and compute the tight union `BBox` of the included rows for that sub-chunk.

3. **`crates/razorpdf-core/src/vlm/cropper.rs`**:
   * Implement `render_citation_overlay(canvas: &CanvasSource, page: u32, bbox: BBox, padding_pt: f32) -> anyhow::Result<Vec<u8>>`:
     * Renders the page (or crops a padded context window around `bbox`), draws a semi-transparent highlight fill (`rgba(255, 215, 0, 0.22)`) and a `2px` border (`rgba(220, 38, 38, 0.95)`) around `[x0, y0, x1, y1]`, and encodes the result as WebP bytes.

#### 3.3 Phase 3 Verification
* Verify that a 120-row table split across 3 chunks produces 3 valid Markdown tables, each starting with the original column header row and exact `token_count <= max_chunk_tokens`.
* Run `cargo test -p razorpdf-core emitter::chunks`.

### Phase 4: Content-Addressed VLM Cache (`blake3`) & Optional ONNX Micro-Tier

#### 4.1 Goal
Make repeated parses, CI runs, and incremental document edits instantaneous by caching sub-page WebP VLM completions via `blake3`, and support optional local ONNX inference.

#### 4.2 Target Files & Implementation Details
1. **Root `Cargo.toml` & `crates/razorpdf-core/Cargo.toml`**:
   * Add `blake3 = "1.5"` to workspace dependencies.
2. **`crates/razorpdf-core/src/vlm/cache.rs` (New File)**:
   * Compute cache key:
     ```rust
     pub fn compute_vlm_cache_key(webp_bytes: &[u8], model: &str, prompt: &str) -> String {
         let mut hasher = blake3::Hasher::new();
         hasher.update(b"razorpdf-vlm-v1:");
         hasher.update(model.as_bytes());
         hasher.update(b":");
         hasher.update(prompt.as_bytes());
         hasher.update(b":");
         hasher.update(webp_bytes);
         hasher.finalize().to_hex().to_string()
     }
     ```
   * Store and retrieve cached JSON completions under `~/.cache/razorpdf/vlm/<first_2_hex>/<full_hex>.json` (configurable via `VlmConfig::cache_dir` or `RAZORPDF_CACHE_DIR` env var).
3. **`crates/razorpdf-core/src/vlm/client.rs`**:
   * Check `Blake3VlmCache::get(&key)` before dispatching `reqwest` HTTP calls to Ollama/OpenAI, and write successful responses atomically via temp-file rename.

#### 4.3 Phase 4 Verification
* Add unit test `test_blake3_vlm_cache_hit_skips_network` in `crates/razorpdf-core/src/vlm/cache.rs`.
* Run `cargo test -p razorpdf-core vlm::`.

### Phase 5: Stateful Progressive-Disclosure MCP Server (`razorpdf mcp`)

#### 5.1 Goal
Transform `razorpdf mcp` from a whole-file text dumper into an intelligent, context-preserving document server for Claude Desktop, Cursor, and autonomous agents.

#### 5.2 Target Files & Implementation Details
1. **`crates/razorpdf/src/mcp_server.rs`**:
   * Add an LRU in-memory document cache (`Arc<Mutex<HashMap<PathBuf, Arc<ParsedDocument>>>>` keyed by file path + modification timestamp `mtime`) so multiple MCP tool calls on the same PDF reuse the parsed `ParsedDocument` in $<1\text{ms}$.
   * Register 4 new MCP tools alongside `parse_pdf`, `inspect_pdf_triage`, and `convert_to_markdown`:
     1. **`get_pdf_toc`**:
        * **Input**: `{ "path": "string" }`
        * **Output**: Document page count, metadata, nested heading tree (`H1..H4` with page numbers and `chunk_id`s), and per-page complexity scores $S(R)$.
     2. **`search_pdf_chunks`**:
        * **Input**: `{ "path": "string", "query": "string", "top_k": 5 }`
        * **Output**: Ranks `GroundedChunk` items using BM25 term-frequency / inverse-document-frequency scoring (plus exact substring/regex boost) in pure Rust, returning the top-$K$ chunks with exact `[x0, y0, x1, y1]`, `page`, and `section_path`.
     3. **`read_pdf_page_range`**:
        * **Input**: `{ "path": "string", "start_page": 1, "end_page": 5 }`
        * **Output**: Clean Markdown and grounded chunks restricted to `start_page..=end_page`.
     4. **`render_pdf_region`**:
        * **Input**: `{ "path": "string", "page": 1, "bbox": [x0, y0, x1, y1], "highlight": true }`
        * **Output**: Calls `render_citation_overlay()` and returns a native MCP image content block (`"type": "image", "mimeType": "image/webp", "data": "<base64>"`) so multimodal agents can visually inspect signatures, stamps, or diagrams on demand.

#### 5.3 Phase 5 Verification
* Test JSON-RPC `tools/list` and `tools/call` payloads in `crates/razorpdf/src/mcp_server.rs` for all 7 MCP tools.
* Run `cargo test -p razorpdf`.

### Phase 6: Python Bindings Expansion (`razorpdf-py`) & OmniDocBench Harness

#### 6.1 Goal
Expose all new capabilities to Python users with full GIL release and provide a reproducible benchmark script comparing `razorpdf` against industry baselines.

#### 6.2 Target Files & Implementation Details
1. **`crates/razorpdf-py/src/lib.rs`**:
   * Update `parse_file` and `to_markdown` to accept `stitch_pages: bool = True`, `repeat_table_headers: bool = True`, and `use_cache: bool = True`.
   * Add `search_pdf(path: &str, query: &str, top_k: usize) -> PyResult<Vec<PyObject>>` releasing the Python GIL via `py.allow_threads(|| ...)`.
   * Add `render_citation(path: &str, page: u32, bbox: (f32, f32, f32, f32)) -> PyResult<Cow<[u8]>>` returning raw WebP bytes directly usable in Jupyter, Streamlit, or FastAPI.
2. **`scripts/bench_omnidoc.py` (New File)**:
   * Extend the benchmarking suite alongside `scripts/bench_wild.py` to evaluate throughput (pages/sec), Peak RSS memory, reading-order Normalized Edit Distance, and Table TEDS score on a local directory of PDFs or [OmniDocBench](https://github.com/opendatalab/OmniDocBench) samples, outputting a Markdown comparison scorecard to `reports/omnidoc_scorecard.md`.

## 4. Agent Hermes Execution Rules & Quality Gates

1. **Strict Sequential Execution**: Complete Phases 1 through 6 in order. Do not advance to the next phase until all unit and integration tests for the current phase pass.
2. **Zero Warnings & Clean Formatting**:
   * Run `cargo fmt --all -- --check`
   * Run `cargo clippy --workspace --exclude razorpdf-py --all-targets -- -D warnings`
   * Run `cargo test --workspace --exclude razorpdf-py`
3. **Performance Guardrail**:
   * Deterministic parsing throughput on clean pages must remain $>10,000\text{ pages/sec}$ in `cargo bench` and $>300\text{ pages/sec}$ in `python3 scripts/bench_wild.py`. Never initialize `tiktoken-rs` BPE ranks inside a tight loop; always use a module-level `OnceLock`.

## 5. Copy-Paste `/goal` Prompt for Agent Hermes

```text
/goal Upgrade the `razorpdf` repository (Rust workspace: `crates/razorpdf-core`, `crates/razorpdf`, `crates/razorpdf-py`) from v0.1.6 to v0.2.0 by executing the 6-phase implementation plan in `razorpdf_v02_implementation_plan.md`.

Execute the following phases sequentially, running `cargo test --workspace --exclude razorpdf-py` and `cargo clippy --workspace --exclude razorpdf-py -- -D warnings` after each phase to guarantee zero regressions:

1. Phase 1 (Font/CMap Anomaly, Math Triage & Annotations):
   - In `crates/razorpdf-core/src/models.rs`, add `CorruptedCMapOrMojibake` and `MathGlyphCluster` to `TriageReason`, plus `PdfLinkAnnotation` and `FormFieldEntry`.
   - In `crates/razorpdf-core/src/extractor/fonts.rs`, implement `analyze_span_anomaly()` to detect Unicode replacement/PUA/control character gibberish (`mojibake_ratio`) and mathematical symbol fonts (`CMMI`, `CMSY`, `CMEX`, `CambriaMath`, `Symbol`) + sub/superscript baseline shifts.
   - In `crates/razorpdf-core/src/triage/scorer.rs`, update `compute_region_triage_score()` so regions with `mojibake_ratio > 0.08` or `math_glyph_ratio > 0.15` automatically exceed the VLM threshold and route to WebP cropping.
   - In `crates/razorpdf-core/src/extractor/lopdf_fallback.rs` and `pdfium_backend.rs`, extract `/Link` `/URI` annotations and `/AcroForm` field values into Markdown output.

2. Phase 2 (Cross-Page Continuity Engine):
   - Create `crates/razorpdf-core/src/geometry/cross_page.rs` and wire it into `crates/razorpdf-core/src/assembler.rs`.
   - Implement multi-page R-tree table stitching: merge `BlockKind::DeterministicTable` blocks across page N bottom and page N+1 top when column counts match and horizontal column X-interval IoU >= 0.85, automatically deduplicating repeated header rows and merging split continuation cells.
   - Implement cross-page paragraph re-flow and end-of-page de-hyphenation when page N ends without terminal punctuation.

3. Phase 3 (Structure-Preserving Token Chunking & Visual Citations):
   - In `crates/razorpdf-core/src/emitter/chunks.rs`, cache the `tiktoken-rs` BPE tokenizer with `std::sync::OnceLock` and implement structure-preserving table splitting that prepends the Markdown `| Header |` and `| --- |` rows to every split table sub-chunk when a table exceeds `max_chunk_tokens`.
   - Add `page_spans: Vec<PageBBox>` to `GroundedChunk` in `models.rs`.
   - In `crates/razorpdf-core/src/vlm/cropper.rs`, implement `render_citation_overlay()` to draw highlighted `[x0, y0, x1, y1]` bounding boxes onto WebP page previews.

4. Phase 4 (Content-Addressed Blake3 VLM Cache):
   - Add `blake3` to workspace dependencies in `Cargo.toml`.
   - Create `crates/razorpdf-core/src/vlm/cache.rs` implementing a disk-backed content-addressed cache keyed by `blake3(model + prompt + webp_bytes)` and integrate it into `crates/razorpdf-core/src/vlm/client.rs`.

5. Phase 5 (Stateful Progressive-Disclosure MCP Server):
   - In `crates/razorpdf/src/mcp_server.rs`, add an in-memory `mtime`-aware `ParsedDocument` cache and implement 4 new MCP tools: `get_pdf_toc`, `search_pdf_chunks` (pure-Rust BM25 + regex ranking with `[x0, y0, x1, y1]` coordinates), `read_pdf_page_range`, and `render_pdf_region` (returning base64 `image/webp` MCP content blocks).

6. Phase 6 (Python Bindings & Benchmark Verification):
   - Expose `search_chunks`, `render_citation`, and new config flags in `crates/razorpdf-py/src/lib.rs` with `py.allow_threads` GIL release.
   - Create `scripts/bench_omnidoc.py` to evaluate reading order, Table TEDS, and throughput, and update `README.md` and `plan.md` to document all v0.2.0 features.
```
