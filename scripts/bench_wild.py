#!/usr/bin/env python3
"""Head-to-head: razorpdf vs pypdf vs pdfminer.six vs pymupdf on tests/wild.

Measures wall-clock ms + pages/sec per file (median of 3 runs after 1
warmup) and checks GST/Attention content fidelity per backend.

Usage (from repo root, bindings installed, e.g. `maturin develop`):
    .venv/bin/python scripts/bench_wild.py
"""

from __future__ import annotations

import statistics
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WILD = sorted((ROOT / "tests" / "wild").glob("*.pdf"))

# Fidelity markers: substrings that must survive extraction.
FIDELITY = {
    "gst_tax_invoice": [
        "TAX INVOICE",
        "Dedicated GPU Node Rental",
        "998315",
        "1,06,200.00",
        "SGST",
    ],
    "arxiv_attention_2col": [
        "Self-Attention",
        "Table 1",
        "significantly",
        "Łukasz",
        "O(n",
    ],
}


def bench_razorpdf(path: Path):
    import razorpdf

    t0 = time.perf_counter()
    doc = razorpdf.parse_file(str(path))
    dt = (time.perf_counter() - t0) * 1000.0
    return dt, doc["markdown"], doc["total_pages"]


def bench_pypdf(path: Path):
    from pypdf import PdfReader

    t0 = time.perf_counter()
    reader = PdfReader(str(path))
    text = "\n".join((p.extract_text() or "") for p in reader.pages)
    dt = (time.perf_counter() - t0) * 1000.0
    return dt, text, len(reader.pages)


def bench_pdfminer(path: Path):
    from pdfminer.high_level import extract_text

    t0 = time.perf_counter()
    text = extract_text(str(path))
    dt = (time.perf_counter() - t0) * 1000.0
    return dt, text, 0  # pages filled in later


def bench_pymupdf(path: Path):
    import pymupdf

    t0 = time.perf_counter()
    doc = pymupdf.open(str(path))
    text = "\n".join(p.get_text() for p in doc)
    npages = doc.page_count
    doc.close()
    dt = (time.perf_counter() - t0) * 1000.0
    return dt, text, npages


BACKENDS = [
    ("razorpdf", bench_razorpdf),
    ("pypdf", bench_pypdf),
    ("pdfminer", bench_pdfminer),
    ("pymupdf", bench_pymupdf),
]

RUNS = 3


def main() -> int:
    if not WILD:
        print("no PDFs in tests/wild — see tests/wild/README.md", file=sys.stderr)
        return 1
    versions = {}
    try:
        import razorpdf  # noqa: F401
        versions["razorpdf"] = "0.1.0 (release)"
    except ImportError:
        print("razorpdf bindings not installed (maturin develop)", file=sys.stderr)
        return 1
    try:
        from importlib.metadata import version

        versions["pypdf"] = version("pypdf")
        versions["pdfminer"] = version("pdfminer.six")
        versions["pymupdf"] = version("pymupdf")
    except Exception as exc:  # noqa: BLE001
        print(f"missing backend dep: {exc}", file=sys.stderr)
        return 1
    print("versions:", ", ".join(f"{k}={v}" for k, v in versions.items()))

    # Page counts from pypdf (neutral ground truth for pages/sec).
    from pypdf import PdfReader

    npages = {f: len(PdfReader(str(f)).pages) for f in WILD}

    results: dict[str, dict[str, tuple[float, str]]] = {}
    for name, fn in BACKENDS:
        results[name] = {}
        for path in WILD:
            try:
                fn(path)  # warmup
                samples = []
                text = ""
                for _ in range(RUNS):
                    dt, text, _ = fn(path)
                    samples.append(dt)
                results[name][path.name] = (statistics.median(samples), text)
            except Exception as exc:  # noqa: BLE001
                print(f"{name} FAILED on {path.name}: {exc}", file=sys.stderr)
                results[name][path.name] = (float("nan"), "")

    print("\n## Wall-clock (median ms of 3, 1 warmup) + pages/sec\n")
    hdr = ["file", "pages"] + [n for n, _ in BACKENDS]
    print("| " + " | ".join(hdr) + " |")
    print("|" + "|".join(["---"] * len(hdr)) + "|")
    totals: dict[str, float] = {n: 0.0 for n, _ in BACKENDS}
    total_pages = 0
    for path in WILD:
        n = npages[path]
        total_pages += n
        row = [path.stem, str(n)]
        for name, _ in BACKENDS:
            ms, _ = results[name][path.name]
            totals[name] += ms
            row.append(f"{ms:.0f} ms ({n / ms * 1000:.0f} p/s)")
        print("| " + " | ".join(row) + " |")
    total_row = ["TOTAL", str(total_pages)]
    for name, _ in BACKENDS:
        total_row.append(f"{totals[name]:.0f} ms ({total_pages / totals[name] * 1000:.0f} p/s)")
    print("| " + " | ".join(total_row) + " |")

    print("\n## Fidelity (markers found / total)\n")
    print("| file | " + " | ".join(n for n, _ in BACKENDS) + " | md table rows (razorpdf) |")
    print("|" + "|".join(["---"] * (len(BACKENDS) + 2)) + "|")
    for stem, markers in FIDELITY.items():
        row = [stem]
        for name, _ in BACKENDS:
            text = results[name].get(f"{stem}.pdf", (0.0, ""))[1]
            found = sum(1 for m in markers if m in text)
            missing = [m for m in markers if m not in text]
            cell = f"{found}/{len(markers)}"
            if missing:
                cell += f" (miss: {', '.join(missing)})"
            row.append(cell)
        md = results["razorpdf"].get(f"{stem}.pdf", (0.0, ""))[1]
        rows = sum(
            1
            for ln in md.splitlines()
            if ln.startswith("|") and "---" not in ln
        )
        row.append(str(rows))
        print("| " + " | ".join(row) + " |")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
