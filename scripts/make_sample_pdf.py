#!/usr/bin/env python3
"""Generate samples/invoice.pdf: a tiny demo invoice (title + 2-col header + ruled table).

Regenerate with (from repo root):
    .venv/bin/python scripts/make_sample_pdf.py
"""

from __future__ import annotations

from pathlib import Path

import pymupdf

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "samples/invoice.pdf"

ROWS = [
    ("Sr", "Description", "Qty", "Rate", "Total"),
    ("1", "GPU Node Rental", "2", "45,000.00", "90,000.00"),
    ("2", "Firewall & SD-WAN", "1", "15,000.00", "15,000.00"),
]
COLS = [40.0, 70.0, 300.0, 350.0, 450.0, 555.0]
TOP, ROW_H = 150.0, 26.0


def main() -> None:
    doc = pymupdf.open()
    page = doc.new_page(width=595, height=420)
    page.insert_text((40, 50), "TAX INVOICE (SAMPLE)", fontsize=18)
    page.insert_text((40, 80), "Supplier: Apex Cloud Infra Pvt. Ltd.", fontsize=11)
    page.insert_text((40, 98), "Buyer: Enterprise Corp Mumbai", fontsize=11)
    page.insert_text((350, 80), "GSTIN: 27AABCA1234F1Z5", fontsize=11)
    page.insert_text((350, 98), "State Code: 27", fontsize=11)
    bottom = TOP + ROW_H * len(ROWS)
    for r in range(len(ROWS) + 1):
        y = TOP + r * ROW_H
        page.draw_line((COLS[0], y), (COLS[-1], y))
    for x in COLS:
        page.draw_line((x, TOP), (x, bottom))
    for r, row in enumerate(ROWS):
        for c, cell in enumerate(row):
            page.insert_text((COLS[c] + 5, TOP + r * ROW_H + 18), cell, fontsize=10)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    doc.save(OUT)
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
