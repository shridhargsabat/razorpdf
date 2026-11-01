# Launch drafts (DO NOT POST without explicit approval)

## A. Technical launch post (r/rust, r/LocalLLaMA, Hacker News)

Title options:
- Rust: "razorpdf: hybrid PDF parser — deterministic geometry fast path (16.7k pages/sec) + sub-page VLM fallback"
- LocalLLaMA: "Stop sending whole PDF pages to vision models: razorpdf routes only messy sub-page regions (425 vs 2125 tokens)"
- HN: "razorpdf: local-first PDF parser, geometry for 90%+, VLM only for messy regions"

Body:

> I built razorpdf because PDF→RAG pipelines force a bad tradeoff: flat
> extractors (pypdf, pdfminer, pymupdf) mangle multi-column papers and
> borderless tables, while full-page vision/OCR pipelines burn seconds and
> thousands of image tokens on pages that are trivially parseable.
>
> razorpdf is hybrid and local-first (Rust, MIT/Apache-2.0):
>
> 1. **Extract** spans + vectors + image bboxes with lopdf (pure Rust).
> 2. **Geometry fast path** — line merge, recursive XY-cut reading order,
>    R-tree vector-table reconstruction. Zero model calls, ~60 µs/page
>    (16,720 pages/sec synthetic benchmark).
> 3. **Triage** — every candidate region gets S(R) = min(1, 0.40·overlap +
>    0.35·table-entropy + 0.25·vector-noise). Below 0.45 stays geometric.
> 4. **Sub-page VLM fallback** — only the failing bbox is cropped to WebP
>    and sent to any OpenAI-compatible endpoint (Ollama/OpenAI/Gemini).
>    A borderless table costs ~425 image tokens instead of ~2,125 for the
>    full page; pure-text pages cost 0.
> 5. **Grounded output** — Markdown plus chunks with chunk_id, page_index,
>    {x0,y0,x1,y1} bbox, breadcrumb trail, token estimate, VLM provenance.
>
> Measured on 117 real pages (arXiv papers, Mozilla report, GST invoice):
> 345 ms total (339 pages/sec) vs pymupdf 305, oxidize-pdf 218, pypdf 21,
> pdfminer 15 —
> and only razorpdf recovers the tables as structure (13 table rows on the
> Attention paper). 48 sub-page regions across 117 pages needed any VLM;
> both invoices needed zero.
>
> Ships as CLI + MCP server (`razorpdf mcp`) + Python bindings (GIL
> released). Install: `pip install razorpdf` / `cargo install razorpdf`
> (v0.1.3) or grab a binary from Releases. Demo GIF + notebook in the repo.
>
> Honest limits: scanned/image-only pages still need a VLM or OCR pass
> (pdftoppm rasterizer supported when installed); handwriting is out of
> scope; table header association on exotic layouts is heuristic.
>
> Repo: https://github.com/shridhargsabat/razorpdf — feedback and nasty PDFs welcome.

## B. X/Twitter (≤280 chars + GIF)

> PDFs → RAG without the tradeoff: razorpdf does deterministic Rust geometry
> for 90%+ of pages (16.7k p/s) and sends ONLY messy sub-page regions to a
> VLM (425 vs 2125 tokens).
>
> `pip install razorpdf`
>
> CLI + MCP + Python. MIT/Apache-2.0.
> github.com/shridhargsabat/razorpdf

Alt (numbers-first):

> 117 real PDF pages in 345 ms: razorpdf (339 p/s) vs pymupdf 305, oxidize 218,
> pypdf 21, pdfminer 15. Geometry fast path + sub-page VLM fallback, grounded bboxes
> on every chunk.
>
> `pip install razorpdf` · demo GIF below 👇

## C. LinkedIn (short professional)

> Most PDF→RAG pipelines pick a poison: flat extractors that mangle tables,
> or full-page vision calls billed per page.
>
> razorpdf (open source, MIT/Apache-2.0) takes the middle path: deterministic
> Rust geometry handles 90%+ of pages in microseconds, and only genuinely
> messy sub-page regions get cropped and sent to a vision model — ~425 image
> tokens instead of ~2,125 per page, zero for clean pages.
>
> On 117 real pages (papers, reports, invoices): 339 pages/sec with tables
> recovered as structure. CLI, MCP server, and Python bindings included.
>
> `pip install razorpdf`
> https://github.com/shridhargsabat/razorpdf
