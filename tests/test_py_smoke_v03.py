"""v0.3.0 smoke test: extract_schema, diff_pdfs, to_columnar_dict, to_jsonl, .pyi stubs."""

import json
from pathlib import Path

import razorpdf

SAMPLE = str(Path(__file__).resolve().parent.parent / "samples" / "invoice.pdf")
STUB = (
    Path(razorpdf.__file__).resolve().parent
    / "__init__.pyi"
)


def test_extract_schema_returns_provenance():
    schema = json.dumps(
        {
            "type": "object",
            "properties": {
                "supplier": {"type": "string"},
                "line_items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": {"type": "string"},
                            "qty": {"type": "integer"},
                        },
                    },
                },
            },
        }
    )
    result = razorpdf.extract_schema(SAMPLE, schema)
    assert isinstance(result, dict)
    assert "data" in result and "provenance" in result
    assert result["data"].get("supplier") == "Apex Cloud Infra Pvt. Ltd."
    hit = next(
        m for m in result["provenance"] if m["field_name"] == "supplier"
    )
    assert set(hit["bbox"]) >= {"x0", "y0", "x1", "y1"}
    assert hit["confidence"] > 0.5


def test_extract_schema_rejects_bad_json():
    try:
        razorpdf.extract_schema(SAMPLE, "{not json")
    except ValueError:
        pass
    else:
        raise AssertionError("expected ValueError")


def test_diff_pdfs_self_compare_is_clean():
    result = razorpdf.diff_pdfs(SAMPLE, SAMPLE)
    assert isinstance(result, dict)
    assert result["modified"] == 0
    assert result["added"] == 0
    assert result["removed"] == 0
    assert result["unchanged"] > 0
    assert "markdown_diff" in result


def test_to_columnar_dict_aligns_columns():
    batch = razorpdf.to_columnar_dict(SAMPLE)
    assert isinstance(batch, dict)
    for key in (
        "chunk_id",
        "page",
        "bbox_x0",
        "bbox_y0",
        "bbox_x1",
        "bbox_y1",
        "token_count",
        "block_kind",
        "section_path",
        "text",
    ):
        assert key in batch, f"missing {key}"
    n = len(batch["chunk_id"])
    assert n > 0
    assert all(len(batch[k]) == n for k in batch), "ragged columns"


def test_to_jsonl_roundtrips_chunks():
    payload = razorpdf.to_jsonl(SAMPLE)
    assert isinstance(payload, str)
    lines = [line for line in payload.splitlines() if line.strip()]
    assert len(lines) > 0
    first = json.loads(lines[0])
    assert "chunk_id" in first and "markdown" in first


def test_pyi_stub_covers_public_api():
    assert STUB.exists(), f"missing stub: {STUB}"
    stub = STUB.read_text()
    for symbol in (
        "parse_file",
        "to_markdown",
        "search_chunks",
        "render_citation",
        "extract_schema",
        "diff_pdfs",
        "to_columnar_dict",
        "to_jsonl",
    ):
        assert f"def {symbol}" in stub, f"stub missing {symbol}"
        assert symbol in razorpdf.__all__, f"__all__ missing {symbol}"
