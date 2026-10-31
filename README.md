# razorpdf — Hybrid Geometric Document Parser

Ultra-fast, local-first document parser: PDFs in, clean LLM-ready Markdown
plus grounded RAG chunks out. Deterministic Rust geometry handles the easy
90%+ in microseconds per page; only the genuinely messy sub-page regions
ever see a vision model.

```text
PDF bytes
   │
   ▼
┌─────────────┐   lopdf (pure Rust, rayon page-parallel)
│  EXTRACT    │─── TextSpan + VectorSegment + image bboxes (top-left coords)
└──────┬──────┘
       ▼
┌─────────────┐   line merge → XY-cut reading order → vector table grids
│  GEOMETRY   │─── multi-column safe, zero model calls
└──────┬──────┘
       ▼
┌─────────────┐   S(R) = min(1, 0.40·ρoverlap + 0.35·ρtables + 0.25·ρvectors)
│  TRIAGE     │─── fast path (< 0.45) or padded sub-page VLM region
└──┬───────┬──┘
   │       │ WebP crop of BBox only (never full pages)
   │       ▼
   │   ┌────────┐  OpenAI / Gemini / Ollama / any OpenAI-compatible endpoint
   │   │  VLM   │── bounded concurrency, per-region failure tolerance
   │   └────────┘
   ▼       ▼
┌─────────────┐   reading-order Markdown + chunks with
│  ASSEMBLE   │─── [page, x0, y0, x1, y1] citations + heading breadcrumbs
└─────────────┘
```

## Why

- **Fast path is pure geometry.** Line merging, recursive XY-cut, and
  R-tree vector-table reconstruction run with zero model calls.
- **VLM fallback is sub-page.** Only the failing `BBox` is cropped to WebP
  and sent — a messy table costs one small image, not a full-page render.
- **Chunks are grounded.** Every chunk carries its page, bbox, breadcrumb
  (`H1 > H2 > H3`), token count, and whether a VLM produced it.
- **Offline by default.** No network in tests or the fast path; synthetic
  fixtures generate PDFs in memory.

## Quickstart

```bash
git clone <repo> razorpdf && cd razorpdf
cargo build --release -p razorpdf-cli

# Markdown to stdout, grounded JSON to file
./target/release/razorpdf parse paper.pdf -o paper.md --json paper.json

# Per-page layout + triage verdicts
./target/release/razorpdf inspect paper.pdf

# Messy regions via local Ollama (or any OpenAI-compatible endpoint)
./target/release/razorpdf parse scan.pdf --vlm-url http://localhost:11434/v1/chat/completions

# Offline demo of the VLM path (deterministic mock provider)
./target/release/razorpdf parse messy.pdf --mock-vlm
```

## CLI reference

| Command | Purpose |
|---|---|
| `razorpdf parse <pdf> [-o out.md] [--json out.json] [--vlm-url URL] [--mock-vlm]` | Hybrid parse to Markdown + grounded JSON |
| `razorpdf inspect <pdf>` | Per-page spans/vectors/lines/tables, `S(R)` scores, fallback regions |
| `razorpdf bench [--pages 100] [--json]` | Fast-path throughput on a synthetic multi-page document |
| `razorpdf mcp` (or `razorpdf --mcp`) | MCP JSON-RPC 2.0 server over stdio |

Without `--vlm-url`/`--mock-vlm`, messy regions keep their raw text
(graceful degradation — content is never dropped silently).

## MCP server (Claude Code / OpenCode / Hermes)

Any MCP client that spawns stdio servers works. Tools: `parse_pdf`,
`inspect_pdf_layout`, `extract_pdf_tables`.

**Claude Code:**

```bash
claude mcp add razorpdf /path/to/razorpdf/target/release/razorpdf mcp
```

**OpenCode** (`opencode.json`):

```json
{
  "mcp": {
    "razorpdf": {
      "type": "local",
      "command": ["/path/to/razorpdf/target/release/razorpdf", "mcp"]
    }
  }
}
```

**Other MCP clients (incl. Hermes):** spawn the server over stdio —

```json
{
  "name": "razorpdf",
  "transport": "stdio",
  "command": "/path/to/razorpdf/target/release/razorpdf",
  "args": ["mcp"]
}
```

Example round-trip:

```bash
$ echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"parse_pdf","arguments":{"path":"/tmp/paper.pdf"}}}' \
  | razorpdf mcp
{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\n  \"total_pages\": 1, ..."}]}}
```

## Python

```bash
# from crates/razorpdf-py (maturin)
pip install maturin && maturin develop
```

```python
import razorpdf

doc = razorpdf.parse_file("paper.pdf")          # full grounded document (dict)
print(doc["markdown"][:500])
print(doc["chunks"][0]["bbox"], doc["chunks"][0]["breadcrumb"])

md = razorpdf.to_markdown("paper.pdf")          # Markdown only (str)

# messy regions via any OpenAI-compatible endpoint
doc = razorpdf.parse_file("scan.pdf", vlm_url="http://localhost:11434/v1/chat/completions")
```

Parsing releases the GIL, so thread pools scale across cores.

## Benchmarks

Measured with `razorpdf bench` (400-page mixed two-column/single-column/
bordered-table paper, fast path, no VLM; median of 3 timed runs after 3
warmups) on an AMD EPYC 9354P (4 vCPU):

| Build | Pages/sec | ms/page | Extract | Full assembly |
|---|---|---|---|---|
| debug | 2,219 | 0.45 | 152.4 ms | 180.3 ms |
| release | 16,720 | 0.06 | 11.4 ms | 23.9 ms |

Reproduce: `cargo run --release -p razorpdf-cli -- bench`.
Criterion suite: `cargo bench -p razorpdf-core --bench throughput`.
Assembly (triage + block layout) runs parallel over pages via rayon, so
shared-host scheduling jitter shows run-to-run; the median-of-3 report
absorbs single-sample noise.

### Wild head-to-head (real PDFs, Python API)

`scripts/bench_wild.py` runs `razorpdf.parse_file` (release bindings) against
pypdf, pdfminer.six, and pymupdf text extraction on `tests/wild/*.pdf`
(median of 3 runs after 1 warmup, same EPYC box; razorpdf 0.1.0,
pypdf 6.21.4, pdfminer.six 20260107, pymupdf 2.0):

| file | pages | razorpdf | pypdf | pdfminer | pymupdf |
|---|---|---|---|---|---|
| arxiv_attention_2col | 15 | 93 ms (161 p/s) | 1442 ms (10 p/s) | 1667 ms (9 p/s) | 129 ms (116 p/s) |
| arxiv_deepseek_tables | 86 | 160 ms (537 p/s) | 2588 ms (33 p/s) | 4796 ms (18 p/s) | 227 ms (379 p/s) |
| financial_borderless_watermark | 1 | <1 ms (2774 p/s) | 2 ms (466 p/s) | 7 ms (135 p/s) | 2 ms (549 p/s) |
| gst_tax_invoice | 1 | <1 ms (3110 p/s) | 3 ms (309 p/s) | 10 ms (97 p/s) | 2 ms (508 p/s) |
| mozilla_tracemonkey_2col | 14 | 71 ms (197 p/s) | 911 ms (15 p/s) | 1778 ms (8 p/s) | 77 ms (182 p/s) |
| TOTAL | 117 | 325 ms (360 p/s) | 4946 ms (24 p/s) | 8260 ms (14 p/s) | 437 ms (268 p/s) |

Content fidelity (required substrings found) plus structural tables emitted:

| file | razorpdf | pypdf | pdfminer | pymupdf | md table rows (razorpdf) |
|---|---|---|---|---|---|
| gst_tax_invoice | 5/5 | 5/5 | 5/5 | 5/5 | 3 |
| arxiv_attention_2col | 5/5 | 5/5 | 5/5 | 5/5 | 13 |

All four backends recover the marker text on these files; only razorpdf
returns tables as structure (GST's 3-row invoice grid, Attention's Table 1–3
plus the author block) instead of flat text. Reproduce:
`.venv/bin/python scripts/bench_wild.py` (after `maturin develop --release`).

The design target is <3 ms/page fast path with ≥90% of digital pages
never touching a model; triage scores and region counts per page are
visible via `razorpdf inspect`.

## Triage math

For each candidate region `R`, with weights from `plan.md`:

```text
S(R) = min(1.0, 0.40·ρoverlap + 0.35·ρtab_entropy + 0.25·ρvector_noise)
```

- `ρoverlap`: share of spans touching an overlap above 0.25 (span
  involvement — a pair fraction would dilute to ~0 for any realistic page).
- `ρtab_entropy`: 1.0 when ≥4 consecutive ≥3-island rows fail column
  bucketing (jagged counts or `x0` stdev above tolerance).
- `ρvector_noise`: diagonal vectors over text spans.
- `S(R) ≥ 0.45` → padded `BBox` goes to VLM; scanned pages and
  ≥4-segment diagram clusters trip structurally.

See `plan.md` §3 and `crates/razorpdf-core/src/triage/scorer.rs`.

## Project structure

```text
razorpdf/
├── plan.md                        # full architecture + execution spec
├── crates/
│   ├── razorpdf-core/             # engine: extract → geometry → triage → VLM → emit
│   │   ├── benches/throughput.rs  # criterion pages/sec suite
│   │   └── examples/dump_fixtures.rs
│   ├── razorpdf-cli/              # parse / inspect / bench / mcp
│   └── razorpdf-py/               # parse_file / to_markdown (+ GIL release)
```

Status: Phases 1–6 implemented. Roadmap: pdfium render backend
(`--features pdfium`), `accurate-tokens` tiktoken counting (vendored
ranks), region splitting for long blocks.

## License

MIT OR Apache-2.0.
