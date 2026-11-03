# Changelog

All notable changes to razorpdf are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.3.0] - 2026-11-03

Six-phase flagship upgrade: spatial schema extraction, footnote graphs,
deterministic math-to-LaTeX, columnar export, a structural diff engine,
and a 10-tool MCP server with full Python typing.

### Added — Phase 1: Deterministic spatial JSON Schema extractor

- `extractor::schema::extract_with_schema()` with 4-tier matching:
  AcroForm fields (0.98), inline `Label: value` lines (0.85), spatial
  right-of/below chunk adjacency (0.70), and table header-to-row mapping
  for arrays of objects (0.90), all with `[x0, y0, x1, y1]` provenance.
- `SchemaFieldMatch` / `SchemaExtractionResult` models; `page` on
  `FormFieldEntry`; `form_fields` collected on `ParsedDocument`.

### Added — Phase 2: Running header/footer stripper & footnote linker

- `geometry::footnotes::process_pages()`: digit-blind repeat detection
  strips running bands (≥3 pages, ±6pt); bottom-zone `1`/`[1]`/`*`/`†`
  definitions become `FootnoteItem`s with `[^n]` body links.
- `footnotes` on `ParsedDocument` and `GroundedChunk`; definitions
  appended to document Markdown.

### Added — Phase 3: Deterministic math-to-LaTeX reconstructor

- `geometry::math_ast::reconstruct_latex_from_spans()`: math-glyph
  mapping, `_{sub}`/`^{sup}` baseline grouping, `\frac{}{}` bar
  detection; lines at confidence ≥ 0.80 emit `$...$`/`$$...$$` and skip
  VLM (`MathGlyphCluster` regions dropped at ≥70% line overlap).

### Added — Phase 4: Columnar export & structural diff engine

- `emitter::columnar::{chunks_to_jsonl, chunks_to_columnar_batch()}`
  (`ColumnarChunkBatch` Structure-of-Arrays for Arrow/Polars/LanceDB).
- `emitter::diff::{diff_documents, to_markdown_diff()}` classifying
  Unchanged/Modified/Added/Removed blocks with dual bboxes, Jaccard
  alignment, and word/table-row delta summaries.

### Added — Phase 5: 10-tool MCP server & CLI subcommands

- MCP tools `extract_pdf_schema`, `diff_pdfs`, and
  `extract_pdf_annotations_and_forms` (10 total, mtime-cache backed).
- CLI: `razorpdf schema --schema <path|json>`, `razorpdf diff <old>
  <new> [--json]`, `razorpdf jsonl`.

### Added — Phase 6: Python API, PEP 561 stubs & release verification

- `extract_schema`, `diff_pdfs`, `to_columnar_dict`, `to_jsonl`
  bindings (GIL released via `py.detach()`); `__init__.pyi` +
  `py.typed` shipped in the wheel; `tests/test_py_smoke_v03.py`.

## [0.2.0] - 2026-11-03

Six-phase hybrid-engine upgrade: corrupt-font triage, cross-page
continuity, structure-preserving chunking, a content-addressed VLM cache,
a progressive-disclosure MCP server, and expanded Python bindings.

### Added — Phase 1: Font/CMap anomaly, math triage & annotations

- `TriageReason::CorruptedCMapOrMojibake` and `TriageReason::MathGlyphCluster`.
- `extractor::fonts::analyze_span_anomaly()`: per-span `mojibake_ratio`
  (U+FFFD / Private Use Area / C0-C1 controls) and `math_glyph_ratio`
  (CMMI, CMSY, CMEX, MSBM, CambriaMath, LatinModernMath, Symbol families
  plus math-symbol codepoints).
- Triage floors: spans with mojibake ratio > 0.08 force S(R) >= 0.75;
  math ratio > 0.15 or sub/superscript baseline oscillation forces
  S(R) >= 0.55, routing tight equation boxes to the VLM.
- `/Link` `/URI` hyperlink extraction emitted as Markdown anchors and
  `/AcroForm` field values emitted as `- [x] Field: Value` checklists.

### Added — Phase 2: Cross-page continuity engine

- New `geometry::cross_page::stitch_across_pages()`, wired into the
  assembler after per-page XY-cut and R-tree table extraction.
- Multi-page table stitching: merges trailing/leading
  `BlockKind::DeterministicTable` blocks when column counts match and
  mean column X-interval IoU >= 0.85, with repeated-header dedup and
  split-continuation-cell fold-up.
- Cross-page paragraph re-flow with end-of-page de-hyphenation when
  page N ends without terminal punctuation.
- `PageBBox` provenance spans on stitched blocks.

### Added — Phase 3: Structure-preserving chunking & visual citations

- Table chunk splitting at `max_chunk_tokens` (default 512) that
  prepends the `| Header |` and `| --- |` rows to every sub-chunk, with
  exact token counts from a `OnceLock`-cached tiktoken-rs BPE tokenizer.
- `page_spans: Vec<PageBBox>` on `GroundedChunk` for multi-page provenance.
- `vlm::cropper::render_citation_overlay()`: highlighted `[x0, y0, x1, y1]`
  citation boxes rendered onto WebP page crops.

### Added — Phase 4: Content-addressed Blake3 VLM cache

- `blake3 = "1.5"` workspace dependency; new `vlm::cache::Blake3VlmCache`
  keyed by `blake3("razorpdf-vlm-v1:" + model + prompt + webp_bytes)`,
  stored under `~/.cache/razorpdf/vlm/` (override via `RAZORPDF_CACHE_DIR`),
  integrated into the HTTP VLM provider with atomic writes.

### Added — Phase 5: Stateful progressive-disclosure MCP server

- mtime-keyed in-memory LRU `ParsedDocument` cache shared across tool calls.
- Four new MCP tools: `get_pdf_toc`, `search_pdf_chunks` (pure-Rust BM25
  + substring boost with bbox coordinates), `read_pdf_page_range`, and
  `render_pdf_region` (base64 `image/webp` content blocks).

### Added — Phase 6: Python bindings & benchmark harness

- `razorpdf.search_chunks(path, query, top_k)` and
  `razorpdf.render_citation(path, page, bbox)`; `stitch_pages` and
  `use_cache` options on `parse_file`/`to_markdown`; GIL released via
  `py.detach()` for parallel parsing.
- `scripts/bench_omnidoc.py`: throughput, peak RSS, reading-order NED,
  and Zhang–Shasha table TEDS with a Markdown scorecard at
  `reports/omnidoc_scorecard.md`.
- `tests/test_py_smoke_v02.py`: 6-test wheel smoke suite covering all
  four bindings plus concurrent-parse GIL release.

[0.3.0]: https://github.com/shridhargsabat/razorpdf/releases/tag/v0.3.0
[0.2.0]: https://github.com/shridhargsabat/razorpdf/releases/tag/v0.2.0
