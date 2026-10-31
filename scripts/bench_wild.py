#!/usr/bin/env python3
"""Head-to-head: razorpdf vs text extractors vs Rust parsers vs Docling.

Fast backends run the full tests/wild suite; heavy model pipelines
(Docling) run a small-file section. Wall-clock ms + pages/sec per file
(median of 3 runs after 1 warmup), GST/Attention fidelity markers, and
Markdown table-row counts per backend.

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

RUNS = 3
HEAVY_MAX_PAGES = 2  # heavy pipelines only run files this small


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


def bench_rasgapdf(path: Path):
    import rasgapdf

    t0 = time.perf_counter()
    text = rasgapdf.extract_text(str(path))
    dt = (time.perf_counter() - t0) * 1000.0
    return dt, text, 0


def bench_oxidize(path: Path):
    import oxidize_pdf

    t0 = time.perf_counter()
    reader = oxidize_pdf.PdfReader.open(str(path))
    pages = reader.extract_text()
    dt = (time.perf_counter() - t0) * 1000.0
    text = "\n".join(pages) if isinstance(pages, list) else str(pages)
    return dt, text, 0


_DOCLING_CONVERTER = None


def bench_docling(path: Path):
    from docling.document_converter import DocumentConverter

    global _DOCLING_CONVERTER
    if _DOCLING_CONVERTER is None:
        _DOCLING_CONVERTER = DocumentConverter()
    t0 = time.perf_counter()
    result = _DOCLING_CONVERTER.convert(str(path))
    md = result.document.export_to_markdown()
    dt = (time.perf_counter() - t0) * 1000.0
    return dt, md, 0


FAST_BACKENDS = [
    ("razorpdf", bench_razorpdf, "razorpdf"),
    ("pypdf", bench_pypdf, "pypdf"),
    ("pdfminer", bench_pdfminer, "pdfminer.six"),
    ("pymupdf", bench_pymupdf, "pymupdf"),
    ("rasgapdf", bench_rasgapdf, "rasgapdf"),
    ("oxidize", bench_oxidize, "oxidize-pdf"),
]

HEAVY_BACKENDS = [
    ("docling", bench_docling, "docling"),
]


def table_rows(text: str) -> int:
    return sum(1 for ln in text.splitlines() if ln.startswith("|") and "---" not in ln)


def run_suite(backends, files, npages):
    """Returns {backend: {filename: (median_ms, text)}}."""
    results: dict[str, dict[str, tuple[float, str]]] = {}
    for name, fn, _ in backends:
        results[name] = {}
        for path in files:
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
    return results


def print_speed_table(backends, files, npages, results):
    hdr = ["file", "pages"] + [n for n, _, _ in backends]
    print("| " + " | ".join(hdr) + " |")
    print("|" + "|".join(["---"] * len(hdr)) + "|")
    totals: dict[str, float] = {n: 0.0 for n, _, _ in backends}
    total_pages = 0
    for path in files:
        n = npages[path]
        total_pages += n
        row = [path.stem, str(n)]
        for name, _, _ in backends:
            ms, _ = results[name][path.name]
            totals[name] += ms
            row.append(f"{ms:.0f} ms ({n / ms * 1000:.0f} p/s)")
        print("| " + " | ".join(row) + " |")
    total_row = ["TOTAL", str(total_pages)]
    for name, _, _ in backends:
        total_row.append(f"{totals[name]:.0f} ms ({total_pages / totals[name] * 1000:.0f} p/s)")
    print("| " + " | ".join(total_row) + " |")


def print_fidelity_table(backends, results, stems=None):
    names = [n for n, _, _ in backends]
    print("| file | " + " | ".join(names) + " |")
    print("|" + "|".join(["---"] * (len(names) + 1)) + "|")
    for stem, markers in FIDELITY.items():
        if stems is not None and stem not in stems:
            continue
        row = [stem]
        for name in names:
            text = results[name].get(f"{stem}.pdf", (0.0, ""))[1]
            found = sum(1 for m in markers if m in text)
            missing = [m for m in markers if m not in text]
            cell = f"{found}/{len(markers)}"
            if missing:
                cell += f" (miss: {', '.join(missing)})"
            row.append(cell)
        print("| " + " | ".join(row) + " |")


def print_structure_table(backends, results, stems=None):
    names = [n for n, _, _ in backends]
    print("| file | " + " | ".join(f"{n} rows" for n in names) + " |")
    print("|" + "|".join(["---"] * (len(names) + 1)) + "|")
    for stem in FIDELITY:
        if stems is not None and stem not in stems:
            continue
        row = [stem]
        for name in names:
            text = results[name].get(f"{stem}.pdf", (0.0, ""))[1]
            row.append(str(table_rows(text)))
        print("| " + " | ".join(row) + " |")


def main() -> int:
    if not WILD:
        print("no PDFs in tests/wild — see tests/wild/README.md", file=sys.stderr)
        return 1
    from importlib.metadata import version

    try:
        import razorpdf  # noqa: F401

        print(f"versions: razorpdf={version('razorpdf')} (release)", end="")
    except ImportError:
        print("razorpdf bindings not installed (maturin develop)", file=sys.stderr)
        return 1

    # Optional backends: present only if importable.
    fast, heavy = [], []
    for name, fn, dist in FAST_BACKENDS:
        try:
            version(dist)
            fast.append((name, fn, dist))
            if name != "razorpdf":  # already printed in the header
                print(f", {name}={version(dist)}", end="")
        except Exception:  # noqa: BLE001
            print(f"\n(skip {name}: not installed)", file=sys.stderr)
    if len(fast) < 2:
        print("need at least razorpdf + one competitor", file=sys.stderr)
        return 1
    for name, fn, dist in HEAVY_BACKENDS:
        try:
            version(dist)
            heavy.append((name, fn, dist))
            print(f", {name}={version(dist)}", end="")
        except Exception:  # noqa: BLE001
            print(f"\n(skip {name}: not installed)", file=sys.stderr)
    print()

    from pypdf import PdfReader

    npages = {f: len(PdfReader(str(f)).pages) for f in WILD}

    print("\n## Fast backends — wall-clock (median ms of 3, 1 warmup) + pages/sec\n")
    fast_results = run_suite(fast, WILD, npages)
    print_speed_table(fast, WILD, npages, fast_results)

    print("\n## Fidelity (markers found / total)\n")
    print_fidelity_table(fast, fast_results)

    print("\n## Structure (Markdown table rows emitted)\n")
    print_structure_table(fast, fast_results)

    small = [f for f in WILD if npages[f] <= HEAVY_MAX_PAGES]
    if heavy and small:
        names = ["razorpdf"] + [n for n, _, _ in heavy]
        subset = [(n, f, d) for n, f, d in fast if n == "razorpdf"] + heavy
        print(f"\n## Heavy pipelines — files ≤ {HEAVY_MAX_PAGES} pages\n")
        heavy_results = run_suite(heavy, small, npages)
        merged = {"razorpdf": fast_results["razorpdf"], **heavy_results}
        print_speed_table(subset, small, npages, merged)
        print("\n## Heavy fidelity\n")
        run_stems = [f.stem for f in small]
        print_fidelity_table(subset, merged, stems=run_stems)
        print("\n## Heavy structure (Markdown table rows)\n")
        print_structure_table(subset, merged, stems=run_stems)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
