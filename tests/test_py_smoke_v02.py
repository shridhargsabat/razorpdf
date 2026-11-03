"""v0.2.0 smoke test: parse_file, to_markdown, search_chunks, render_citation."""

import threading
from pathlib import Path

import razorpdf

SAMPLE = str(Path(__file__).resolve().parent.parent / "samples" / "invoice.pdf")


def test_parse_file_returns_grounded_doc():
    doc = razorpdf.parse_file(SAMPLE)
    assert doc["total_pages"] >= 1
    assert isinstance(doc["markdown"], str) and len(doc["markdown"]) > 0
    assert isinstance(doc["chunks"], list) and len(doc["chunks"]) > 0
    chunk = doc["chunks"][0]
    for key in ("chunk_id", "page_index", "bbox", "markdown", "page_spans"):
        assert key in chunk, f"missing {key}"
    assert isinstance(chunk["page_spans"], list)


def test_parse_file_stitch_cache_flags():
    doc = razorpdf.parse_file(SAMPLE, stitch_pages=False, use_cache=False)
    assert doc["total_pages"] >= 1
    assert isinstance(doc["markdown"], str)


def test_to_markdown():
    md = razorpdf.to_markdown(SAMPLE)
    assert isinstance(md, str) and len(md) > 0


def test_search_chunks():
    doc = razorpdf.parse_file(SAMPLE)
    assert doc["chunks"], "need chunks to query against"
    probe = "invoice"
    hits = razorpdf.search_chunks(SAMPLE, probe, top_k=3)
    assert isinstance(hits, list) and len(hits) > 0
    hit = hits[0]
    for key in ("chunk_id", "page", "bbox", "score", "markdown"):
        assert key in hit, f"missing {key}"
    assert set(hit["bbox"]) >= {"x0", "y0", "x1", "y1"}
    assert hit["page"] >= 1


def test_render_citation_returns_webp():
    webp = razorpdf.render_citation(SAMPLE, 1, [72.0, 100.0, 300.0, 200.0])
    assert isinstance(webp, (bytes, bytearray)) and len(webp) > 0
    assert bytes(webp[:4]) == b"RIFF" and bytes(webp[8:12]) == b"WEBP"


def test_concurrent_parse_releases_gil():
    """Two threads parsing at once must both succeed (py.detach GIL release)."""
    errors = []

    def worker():
        try:
            doc = razorpdf.parse_file(SAMPLE)
            assert doc["total_pages"] >= 1
        except Exception as exc:  # noqa: BLE001
            errors.append(exc)

    threads = [threading.Thread(target=worker) for _ in range(2)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert not errors, errors
