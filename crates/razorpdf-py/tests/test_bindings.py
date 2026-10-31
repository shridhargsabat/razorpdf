"""Binding smoke tests (stdlib only, no pytest needed).

Setup (from the workspace root):
    cargo build -p razorpdf-py
    cargo run -q -p razorpdf-core --example dump_fixtures -- /tmp/razorpdf-fx
    cp target/debug/librazorpdf.so /tmp/razorpdf-pytest/razorpdf.so

Run:
    PYTHONPATH=/tmp/razorpdf-pytest FIXTURES=/tmp/razorpdf-fx python3.12 tests/test_bindings.py
    (use the Python matching PYO3_PYTHON in .cargo/config.toml)
"""

import concurrent.futures as futures
import os
import sys

FIXTURES = os.environ.get("FIXTURES", "/tmp/razorpdf-fx")


def fixture(name):
    path = os.path.join(FIXTURES, name + ".pdf")
    assert os.path.exists(path), f"missing fixture: {path}"
    return path


def test_parse_file_shape():
    import razorpdf

    doc = razorpdf.parse_file(fixture("single_col_clean"))
    assert isinstance(doc, dict), type(doc)
    for key in (
        "source_path",
        "total_pages",
        "fast_path_pages",
        "vlm_fallback_regions",
        "elapsed_ms",
        "markdown",
        "chunks",
    ):
        assert key in doc, f"missing key {key}"
    assert doc["total_pages"] == 1
    assert doc["fast_path_pages"] == 1
    assert doc["vlm_fallback_regions"] == 0
    assert len(doc["chunks"]) == 17
    first, second = doc["chunks"][0], doc["chunks"][2]
    assert first["breadcrumb"] == []
    assert second["breadcrumb"] == ["RazorPDF Benchmark Document", "1. Introduction"]
    for chunk in doc["chunks"]:
        assert chunk["markdown"].strip(), chunk["chunk_id"]
        box = chunk["bbox"]
        assert box["x0"] <= box["x1"] and box["y0"] <= box["y1"]
        assert chunk["token_estimate"] > 0


def test_to_markdown():
    import razorpdf

    md = razorpdf.to_markdown(fixture("bordered_table"))
    assert isinstance(md, str)
    assert "# Quarterly Sales" in md
    assert "| Name | Qty | Price |" in md
    assert "| Milk | 2 | 3.10 |" in md


def test_messy_without_vlm_keeps_raw_text():
    import razorpdf

    doc = razorpdf.parse_file(fixture("messy_overlap_table"))
    assert doc["vlm_fallback_regions"] == 1
    assert doc["fast_path_pages"] == 0
    assert "Item0" in doc["markdown"]
    assert all(not c["parsed_by_vlm"] for c in doc["chunks"])


def test_missing_file_raises():
    import razorpdf

    try:
        razorpdf.parse_file("/tmp/razorpdf-definitely-missing.pdf")
    except RuntimeError as exc:
        assert "read /tmp/razorpdf-definitely-missing.pdf" in str(exc)
    else:
        raise AssertionError("expected RuntimeError")


def test_canvas_source_kwarg():
    import razorpdf

    for canvas in (None, "synthetic", "auto"):
        doc = razorpdf.parse_file(fixture("single_col_clean"), canvas_source=canvas)
        assert doc["total_pages"] == 1
        assert len(doc["chunks"]) == 17
    try:
        razorpdf.parse_file(fixture("single_col_clean"), canvas_source="bogus")
    except ValueError as exc:
        assert "canvas_source" in str(exc)
    else:
        raise AssertionError("expected ValueError")


def test_threads_parse_concurrently():
    import razorpdf

    paths = [fixture("single_col_clean")] * 8
    with futures.ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(razorpdf.to_markdown, paths))
    assert all(md.startswith("# RazorPDF Benchmark Document") for md in results)


def main():
    tests = [
        test_parse_file_shape,
        test_to_markdown,
        test_messy_without_vlm_keeps_raw_text,
        test_missing_file_raises,
        test_canvas_source_kwarg,
        test_threads_parse_concurrently,
    ]
    for test in tests:
        test()
        print(f"ok - {test.__name__}")
    print(f"{len(tests)} binding tests passed")


if __name__ == "__main__":
    sys.exit(main())
