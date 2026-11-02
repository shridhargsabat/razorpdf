#!/usr/bin/env python3
"""OmniDocBench-style evaluation: throughput, memory, reading order, tables.

Runs razorpdf (plus optional pypdf/pymupdf throughput baselines) over a
directory of PDFs and writes a Markdown scorecard to
``reports/omnidoc_scorecard.md``.

Metrics per file:
  - pages/sec (median of N runs after warmup) and peak RSS (MiB)
  - Markdown tables detected, chunks emitted, VLM regions hit

Ground-truth mode: when ``<stem>.json`` sits next to ``<stem>.pdf`` with
``{"text": "...", "tables": [[["h1", "h2"], ["a", "b"]], ...]}``
(OmniDocBench-adjacent shape), also reports reading-order NED
(whitespace-normalized edit similarity vs GT text) and table TEDS
(Zhang-Shasha tree-edit similarity over table/cell trees).

Usage (from repo root, bindings installed, e.g. `maturin develop`):
    .venv/bin/python scripts/bench_omnidoc.py [PDF_DIR]
    .venv/bin/python scripts/bench_omnidoc.py --selftest
"""

from __future__ import annotations

import argparse
import difflib
import json
import resource
import statistics
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPORTS = ROOT / "reports"


# ---------------------------------------------------------------------------
# Tree edit distance (Zhang-Shasha) for TEDS
# ---------------------------------------------------------------------------


class TNode:
    """Ordered labeled tree node for table TEDS."""

    __slots__ = ("label", "children")

    def __init__(self, label: str, children: "list[TNode] | None" = None):
        self.label = label
        self.children = children or []


def markdown_table_to_tree(rows: list[list[str]]) -> TNode:
    """Markdown-ish row matrix -> table(tr(td*)*) tree."""
    return TNode(
        "table",
        [TNode("tr", [TNode(f"td:{cell.strip()}") for cell in row]) for row in rows],
    )


def _postorder_index(root: TNode):
    """Postorder nodes, leftmost-leaf-descendant map, and keyroots."""
    order: list[TNode] = []

    def walk(node: TNode):
        for child in node.children:
            walk(child)
        order.append(node)

    walk(root)
    index = {id(node): i + 1 for i, node in enumerate(order)}  # 1-based
    lld: dict[int, int] = {}
    for node in order:
        i = index[id(node)]
        if not node.children:
            lld[i] = i
        else:
            lld[i] = min(lld[index[id(child)]] for child in node.children)
    keyroots = sorted({i for i in lld if all(lld[j] != lld[i] or j <= i for j in lld)})
    return order, lld, keyroots


def tree_edit_distance(a: TNode, b: TNode) -> int:
    """Zhang-Shasha ordered tree edit distance (unit costs)."""
    nodes_a, lld_a, keyroots_a = _postorder_index(a)
    nodes_b, lld_b, keyroots_b = _postorder_index(b)
    na, nb = len(nodes_a), len(nodes_b)
    treedist = [[0] * (nb + 1) for _ in range(na + 1)]

    def rename_cost(x: TNode, y: TNode) -> int:
        return 0 if x.label == y.label else 1

    for kra in keyroots_a:
        for krb in keyroots_b:
            la, lb = lld_a[kra], lld_b[krb]
            rows, cols = kra - la + 2, krb - lb + 2
            forest = [[0] * cols for _ in range(rows)]
            for i in range(1, rows):
                forest[i][0] = forest[i - 1][0] + 1
            for j in range(1, cols):
                forest[0][j] = forest[0][j - 1] + 1
            for i in range(la, kra + 1):
                for j in range(lb, krb + 1):
                    if lld_a[i] == la and lld_b[j] == lb:
                        cost = rename_cost(nodes_a[i - 1], nodes_b[j - 1])
                        forest[i - la + 1][j - lb + 1] = min(
                            forest[i - la][j - lb + 1] + 1,
                            forest[i - la + 1][j - lb] + 1,
                            forest[i - la][j - lb] + cost,
                        )
                        treedist[i][j] = forest[i - la + 1][j - lb + 1]
                    else:
                        forest[i - la + 1][j - lb + 1] = min(
                            forest[i - la][j - lb + 1] + 1,
                            forest[i - la + 1][j - lb] + 1,
                            forest[lld_a[i] - la][lld_b[j] - lb] + treedist[i][j],
                        )
    return treedist[na][nb]


def teds_similarity(a: TNode, b: TNode) -> float:
    """TEDS: 1 - edit_distance / max_nodes (1.0 = identical)."""

    def size(node: TNode) -> int:
        return 1 + sum(size(child) for child in node.children)

    denom = max(size(a), size(b))
    if denom == 0:
        return 1.0
    return max(0.0, 1.0 - tree_edit_distance(a, b) / denom)


def extract_md_tables(markdown: str) -> list[list[list[str]]]:
    """Parse GitHub-style tables out of Markdown into row matrices."""
    tables: list[list[list[str]]] = []
    current: list[list[str]] = []
    for line in markdown.splitlines():
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if line.strip().startswith("|") and len(cells) >= 1:
            if set("".join(cells)) <= set("-: |"):
                continue  # separator row
            current.append(cells)
        elif current:
            tables.append(current)
            current = []
    if current:
        tables.append(current)
    return tables


def normalized_edit_similarity(a: str, b: str) -> float:
    """Reading-order NED: SequenceMatcher ratio on squashed whitespace."""
    squash = lambda s: " ".join(s.split())
    return difflib.SequenceMatcher(None, squash(a), squash(b)).ratio()


# ---------------------------------------------------------------------------
# Benchmarking
# ---------------------------------------------------------------------------


def peak_rss_mib() -> float:
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024.0


def bench_razorpdf(path: Path, runs: int, warmup: int):
    import razorpdf

    for _ in range(warmup):
        razorpdf.parse_file(str(path))
    dts = []
    doc = None
    for _ in range(runs):
        t0 = time.perf_counter()
        doc = razorpdf.parse_file(str(path))
        dts.append((time.perf_counter() - t0) * 1000.0)
    assert doc is not None
    tables = extract_md_tables(doc["markdown"])
    return {
        "ms": statistics.median(dts),
        "pages": doc["total_pages"],
        "markdown": doc["markdown"],
        "tables": tables,
        "chunks": len(doc["chunks"]),
        "vlm_regions": doc["vlm_fallback_regions"],
    }


def _read_pypdf(path):
    from pypdf import PdfReader

    reader = PdfReader(str(path))
    return "\n".join((p.extract_text() or "") for p in reader.pages), len(reader.pages)


def _read_pymupdf(path):
    import pymupdf

    doc = pymupdf.open(str(path))
    text = "\n".join(p.get_text() for p in doc)
    npages = doc.page_count
    doc.close()
    return text, npages


def bench_baseline(name: str, path: Path, runs: int, warmup: int):
    """Optional pypdf/pymupdf throughput baseline (None when missing)."""
    read_all = {"pypdf": _read_pypdf, "pymupdf": _read_pymupdf}.get(name)
    if read_all is None:
        return None
    try:
        for _ in range(warmup):
            read_all(path)
        dts = []
        for _ in range(runs):
            t0 = time.perf_counter()
            _, npages = read_all(path)
            dts.append((time.perf_counter() - t0) * 1000.0)
    except ImportError:
        return None
    return {"ms": statistics.median(dts), "pages": npages}


def evaluate(pdf_dir: Path, runs: int, warmup: int) -> list[dict]:
    pdfs = sorted(pdf_dir.glob("*.pdf"))
    if not pdfs:
        print(f"no PDFs in {pdf_dir}", file=sys.stderr)
        return []
    rows = []
    for pdf in pdfs:
        rss_before = peak_rss_mib()
        rz = bench_razorpdf(pdf, runs, warmup)
        rss_after = peak_rss_mib()
        pages = max(rz["pages"], 1)
        row = {
            "file": pdf.name,
            "pages": pages,
            "rz_pages_sec": 1000.0 * pages / max(rz["ms"], 1e-9),
            "rss_mib": rss_after - rss_before,
            "tables": len(rz["tables"]),
            "chunks": rz["chunks"],
            "vlm_regions": rz["vlm_regions"],
            "ned": None,
            "teds": None,
        }
        for base in ("pypdf", "pymupdf"):
            got = bench_baseline(base, pdf, runs, warmup)
            row[f"{base}_pages_sec"] = (
                round(1000.0 * max(got["pages"], 1) / max(got["ms"], 1e-9), 1)
                if got
                else None
            )
        gt_path = pdf.with_suffix(".json")
        if gt_path.exists():
            gt = json.loads(gt_path.read_text())
            if gt.get("text"):
                row["ned"] = round(normalized_edit_similarity(rz["markdown"], gt["text"]), 4)
            gt_tables = gt.get("tables") or []
            if gt_tables and rz["tables"]:
                scores = []
                for pred_rows in rz["tables"]:
                    best = max(
                        teds_similarity(
                            markdown_table_to_tree(pred_rows),
                            markdown_table_to_tree(gt_rows),
                        )
                        for gt_rows in gt_tables
                    )
                    scores.append(best)
                row["teds"] = round(sum(scores) / len(scores), 4)
        rows.append(row)
        print(
            f"{pdf.name}: {row['rz_pages_sec']:.1f} p/s, "
            f"tables={row['tables']} chunks={row['chunks']} vlm={row['vlm_regions']} "
            f"ned={row['ned']} teds={row['teds']}"
        )
    return rows


def write_scorecard(rows: list[dict], out: Path):
    lines = [
        "# OmniDocBench-style scorecard",
        "",
        f"Generated by `scripts/bench_omnidoc.py` — {len(rows)} file(s).",
        "NED/TEDS appear only for PDFs with a `<stem>.json` ground-truth sidecar.",
        "",
        "| file | pages | razorpdf p/s | pypdf p/s | pymupdf p/s | "
        "RSS MiB | tables | chunks | vlm | NED | TEDS |",
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for r in rows:
        fmt = lambda v: "n/a" if v is None else str(v)
        lines.append(
            f"| {r['file']} | {r['pages']} | {r['rz_pages_sec']:.1f} | "
            f"{fmt(r['pypdf_pages_sec'])} | {fmt(r['pymupdf_pages_sec'])} | "
            f"{r['rss_mib']:.1f} | {r['tables']} | {r['chunks']} | "
            f"{r['vlm_regions']} | {fmt(r['ned'])} | {fmt(r['teds'])} |"
        )
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(lines) + "\n")
    print(f"scorecard -> {out}")


def selftest():
    a = markdown_table_to_tree([["h1", "h2"], ["a", "b"]])
    assert tree_edit_distance(a, a) == 0
    assert teds_similarity(a, a) == 1.0
    b = markdown_table_to_tree([["h1", "h2"], ["a", "XX"]])
    assert 0.0 < teds_similarity(a, b) < 1.0, teds_similarity(a, b)
    assert normalized_edit_similarity("a  b", "a b") == 1.0
    assert extract_md_tables("| a |\n| --- |\n| b |\n") == [[["a"], ["b"]]]
    print("bench_omnidoc selftest OK")


def main(argv: list[str]):
    parser = argparse.ArgumentParser(description="OmniDocBench-style evaluation harness")
    parser.add_argument("pdf_dir", nargs="?", default=str(ROOT / "tests" / "wild"))
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--out", default=str(REPORTS / "omnidoc_scorecard.md"))
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)
    if args.selftest:
        return selftest()
    rows = evaluate(Path(args.pdf_dir), args.runs, args.warmup)
    if rows:
        write_scorecard(rows, Path(args.out))


if __name__ == "__main__":
    main(sys.argv[1:])
