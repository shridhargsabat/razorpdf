"""razorpdf (PyPI) vs PyMuPDF: functional comparison on the wild corpus.

Compares fixed releases (see benchmarks/requirements.txt), so pins are
EXACT: any output drift in either library fails loudly and the report
(reports/razorpdf_vs_pymupdf_comparison.md) must be regenerated via
``python benchmarks/compare_razorpdf_vs_pymupdf.py``.

Run: ``pytest tests/test_razorpdf_vs_pymupdf.py -v`` from the repo root.
"""

import math
from importlib import metadata
from pathlib import Path

import pytest

WILD = Path(__file__).resolve().parent / "wild"

# Grounded on razorpdf==0.1.4 / pymupdf==2.0 (see benchmark report).
EXPECTED = {
    "gst_tax_invoice": {"chunks": 4, "pipe_rows": 4, "pymupdf_tables": 1, "pages": 1},
    "financial_borderless_watermark": {
        "chunks": 3,
        "pipe_rows": 4,
        "pymupdf_tables": 0,
        "pages": 1,
    },
    "arxiv_attention_2col": {
        "chunks": 709,
        "pipe_rows": 16,
        "pymupdf_tables": 4,
        "pages": 15,
    },
    "arxiv_deepseek_tables": {
        "chunks": 4364,
        "pipe_rows": 45,
        "pymupdf_tables": 15,
        "pages": 86,
    },
    "mozilla_tracemonkey_2col": {
        "chunks": 1129,
        "pipe_rows": 0,
        "pymupdf_tables": 4,
        "pages": 14,
    },
}


def pipe_rows(markdown: str) -> list[str]:
    return [ln for ln in markdown.splitlines() if ln.startswith("|")]


@pytest.fixture(scope="module")
def parsed():
    import razorpdf

    out = {}
    for stem in EXPECTED:
        path = str(WILD / f"{stem}.pdf")
        out[stem] = {
            "markdown": razorpdf.to_markdown(path),
            "doc": razorpdf.parse_file(path),
        }
    return out


@pytest.fixture(scope="module")
def mu_tables():
    import pymupdf

    out = {}
    for stem in EXPECTED:
        with pymupdf.open(str(WILD / f"{stem}.pdf")) as d:
            out[stem] = sum(len(list(p.find_tables())) for p in d)
    return out


def test_both_libraries_import_at_pinned_versions():
    assert metadata.version("razorpdf") == "0.1.4"
    assert metadata.version("pymupdf") == "2.0"
    import razorpdf

    assert callable(razorpdf.to_markdown) and callable(razorpdf.parse_file)


def test_page_counts_match(parsed):
    for stem, exp in EXPECTED.items():
        assert parsed[stem]["doc"]["total_pages"] == exp["pages"], stem


def test_chunk_counts_match(parsed):
    for stem, exp in EXPECTED.items():
        got = len(parsed[stem]["doc"]["chunks"])
        assert got == exp["chunks"], f"{stem}: {got} chunks"


def test_chunk_schema(parsed):
    required = {
        "bbox",
        "breadcrumb",
        "chunk_id",
        "markdown",
        "page_index",
        "parsed_by_vlm",
        "token_estimate",
    }
    for stem, data in parsed.items():
        assert data["doc"]["chunks"], f"{stem}: no chunks"
        for c in data["doc"]["chunks"]:
            assert required <= set(c), f"{stem}: {sorted(c)}"
            box = c["bbox"]
            assert {"x0", "y0", "x1", "y1"} <= set(box), f"{stem}: {box!r}"
            assert all(
                math.isfinite(box[k]) for k in ("x0", "y0", "x1", "y1")
            ), stem
            assert box["x1"] >= box["x0"] and box["y1"] >= box["y0"], stem
            assert c["token_estimate"] > 0, stem


def test_markdown_table_rows(parsed):
    for stem, exp in EXPECTED.items():
        got = len(pipe_rows(parsed[stem]["markdown"]))
        assert got == exp["pipe_rows"], f"{stem}: {got} pipe rows"


def test_pymupdf_table_counts(mu_tables):
    for stem, exp in EXPECTED.items():
        assert mu_tables[stem] == exp["pymupdf_tables"], stem


def test_ruled_table_parity(parsed):
    # GST 9-col ruled table: both libraries grid it.
    import pymupdf

    md_rows = pipe_rows(parsed["gst_tax_invoice"]["markdown"])
    assert len(md_rows) == 4  # header + separator + 2 data rows
    assert md_rows[0].count("|") == 10  # 9 columns
    with pymupdf.open(str(WILD / "gst_tax_invoice.pdf")) as d:
        (table,) = list(d[0].find_tables())
        assert (table.row_count, table.col_count) == (3, 9)


def test_borderless_table_gap(parsed, mu_tables):
    # financial has a 3-row table with NO ruling lines: razorpdf aligns the
    # text into a grid; pymupdf 2.0 find_tables needs ruling lines (0).
    # The 0 pins pymupdf 2.0 behavior -- if a future pymupdf finds it, this
    # correctly flags "comparison changed, regenerate the report".
    assert len(pipe_rows(parsed["financial_borderless_watermark"]["markdown"])) == 4
    assert mu_tables["financial_borderless_watermark"] == 0


def test_tracemonkey_content_preserved_without_grid(parsed, mu_tables):
    # Honest gap, both directions: pymupdf finds 4 "tables" here but 3 are
    # false positives (p2 assembly listings x2, p7 section prose); its p11
    # 5x10 benchmark table is real. razorpdf grids none of it (0 pipe
    # rows) but preserves every cell value in the markdown.
    md = parsed["mozilla_tracemonkey_2col"]["markdown"]
    assert len(pipe_rows(md)) == 0
    for cell in ("Speedup", "Traces/Loop", "cube", "morph", "bitops"):
        assert cell in md, f"missing cell value {cell!r}"
    assert mu_tables["mozilla_tracemonkey_2col"] == 4


def test_attention_fidelity_markers(parsed):
    # Ligature + caption markers survive the full pipeline.
    md = parsed["arxiv_attention_2col"]["markdown"]
    assert "figures" in md  # fi-ligature intact
    assert len(md) > 10_000


def test_deepseek_many_tables(parsed):
    # 86-page table-heavy paper: 45 gridded rows across the document.
    assert len(pipe_rows(parsed["arxiv_deepseek_tables"]["markdown"])) == 45
