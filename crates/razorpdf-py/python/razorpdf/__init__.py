"""razorpdf Python API (PyO3 bindings, built by maturin)."""

from razorpdf.razorpdf import (
    diff_pdfs,
    extract_schema,
    parse_file,
    render_citation,
    search_chunks,
    to_columnar_dict,
    to_jsonl,
    to_markdown,
)

__all__ = [
    "parse_file",
    "render_citation",
    "search_chunks",
    "to_markdown",
    "extract_schema",
    "diff_pdfs",
    "to_columnar_dict",
    "to_jsonl",
]
