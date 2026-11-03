# Changelog

All notable changes to razorpdf are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

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

[0.2.0]: https://github.com/shridhargsabat/razorpdf/releases/tag/v0.2.0
