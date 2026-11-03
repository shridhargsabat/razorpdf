"""PEP 561 type stubs for the razorpdf extension module."""

from typing import Any

def parse_file(
    path: str,
    vlm_url: str | None = ...,
    canvas_source: str | None = ...,
    stitch_pages: bool = ...,
    use_cache: bool = ...,
) -> dict[str, Any]:
    """Parse a PDF into a grounded document dict.

    Keys: `source_path`, `total_pages`, `fast_path_pages`,
    `vlm_fallback_regions`, `elapsed_ms`, `markdown`, `chunks` (each with
    `chunk_id`, `page_index`, `bbox`, `breadcrumb`, `markdown`,
    `token_estimate`, `parsed_by_vlm`, `page_spans`, `footnotes`),
    `form_fields`, `footnotes`.
    """
    ...

def to_markdown(
    path: str,
    stitch_pages: bool = ...,
    use_cache: bool = ...,
) -> str:
    """Convert a PDF file to Markdown (footnotes appended as `[^n]` defs)."""
    ...

def search_chunks(
    path: str,
    query: str,
    top_k: int = ...,
) -> list[dict[str, Any]]:
    """BM25 + phrase-match retrieval over grounded chunks.

    Returns `[{chunk_id, page (1-based), bbox, section_path, score,
    markdown}]`, best first.
    """
    ...

def render_citation(
    path: str,
    page: int,
    bbox: list[float],
    canvas_source: str | None = ...,
) -> bytes:
    """Render a highlighted citation crop of a page region as WebP bytes.

    `page` is 1-based; `bbox` is `[x0, y0, x1, y1]` in PDF points.
    """
    ...

def extract_schema(path: str, schema_json: str) -> dict[str, Any]:
    """Extract structured JSON matching a JSON Schema document.

    `schema_json` is a JSON object string with `properties` (and optional
    `required`). Returns `{data, provenance, overall_confidence}` where
    each provenance entry carries `field_name`, `value`, `confidence`,
    `page`, `bbox`, and `source`.
    """
    ...

def diff_pdfs(old_path: str, new_path: str) -> dict[str, Any]:
    """Compare two PDF revisions block-by-block.

    Returns the structured diff (`blocks`, `unchanged`, `modified`,
    `added`, `removed`) plus a `markdown_diff` redline summary.
    """
    ...

def to_columnar_dict(path: str) -> dict[str, list[Any]]:
    """Export chunks as a columnar dict of aligned lists.

    Keys: `chunk_id`, `page`, `bbox_x0`, `bbox_y0`, `bbox_x1`, `bbox_y1`,
    `token_count`, `triage_score`, `block_kind`, `section_path`, `text`.
    Ready for `polars.from_dict` or `pyarrow.Table.from_pydict`.
    """
    ...

def to_jsonl(path: str) -> str:
    """Render grounded chunks as newline-delimited JSON (one record/line)."""
    ...
