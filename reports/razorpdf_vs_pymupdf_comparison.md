# razorpdf vs PyMuPDF (library comparison)

Generated: 2026-11-01 18:45 UTC by `benchmarks/compare_razorpdf_vs_pymupdf.py`.
Environment: Python 3.12.3 on Linux x86_64, razorpdf==0.1.4, pymupdf==2.0.
Timing is median-of-5 per file on this box; re-run to reproduce.

## Workload note (read before the timings)

The two columns do NOT do equal work: `page.get_text()` returns raw
text only, while `razorpdf.to_markdown()` runs the full pipeline
(extraction + reading order + table detection + Markdown). Matching
or beating raw-text speed while producing structured output is the
point of the timing table.

## Timing (ms, median-of-5)

| file | pages | razorpdf to_markdown | pymupdf get_text | faster |
|---|---|---|---|---|
| arxiv_attention_2col | 15 | 92.9 | 131.6 | razorpdf |
| arxiv_deepseek_tables | 86 | 142.8 | 197.0 | razorpdf |
| financial_borderless_watermark | 1 | 0.2 | 1.7 | razorpdf |
| gst_tax_invoice | 1 | 0.4 | 1.7 | razorpdf |
| mozilla_tracemonkey_2col | 14 | 53.1 | 67.8 | razorpdf |
| **total** | | **289.4** | **399.9** | |

razorpdf faster-or-equal on 5/5 files.

## Quality metrics

| file | razorpdf chunks | razorpdf table rows | pymupdf tables | md chars |
|---|---|---|---|---|
| arxiv_attention_2col | 709 | 16 | 4 | 40459 |
| arxiv_deepseek_tables | 4364 | 45 | 15 | 238243 |
| financial_borderless_watermark | 3 | 4 | 0 | 259 |
| gst_tax_invoice | 4 | 4 | 1 | 544 |
| mozilla_tracemonkey_2col | 1129 | 0 | 4 | 83703 |

## Findings

- **Ruled tables (GST 9-col): parity.** Both libraries grid it
  (razorpdf 4 Markdown rows incl. header+separator; pymupdf 3x9).
- **Borderless tables (financial, no ruling lines): razorpdf wins.
  razorpdf aligns the text into 4 grid rows; pymupdf 2.0
  `find_tables()` needs ruling lines and finds 0.
- **Complex real-world table (tracemonkey p11 5x10): split.**
  pymupdf grids it; razorpdf preserves every cell value in the
  Markdown but does not grid it (0 pipe rows). Conversely 3 of
  pymupdf's 4 tracemonkey 'tables' are false positives (assembly
  listings, section prose).
- **One-call structure:** `parse_file()` returns grounded chunks
  (`bbox`, `breadcrumb`, `token_estimate`, `parsed_by_vlm`) plus
  full Markdown. PyMuPDF returns unordered raw blocks -- reading
  order, tables, and Markdown are caller assembly work.

## Verdict

- Use razorpdf for Markdown/chunks/tables-with-bboxes in one call
  (RAG ingestion, document pipelines) at raw-text speed.
- Use PyMuPDF for full PDF-toolkit breadth (rendering, annotation,
  editing, redaction), which razorpdf deliberately does not do.
