# Wild-PDF torture corpus (gitignored, refetchable)

Populate from the repo root with:

```bash
cd tests/wild
curl -L -s "https://arxiv.org/pdf/1706.03762.pdf" -o arxiv_attention_2col.pdf
curl -L -s "https://arxiv.org/pdf/2501.12948.pdf" -o arxiv_deepseek_tables.pdf
curl -L -s "https://raw.githubusercontent.com/mozilla/pdf.js/master/test/pdfs/tracemonkey.pdf" -o mozilla_tracemonkey_2col.pdf
# + gst_tax_invoice.pdf / financial_borderless_watermark.pdf: single-page
# samples generated locally with reportlab (Helvetica/WinAnsi only — use
# Rs., not the rupee sign U+20B9). Optional: scripts/bench_wild.py degrades
# gracefully when files are absent.
```

Then:

```bash
for f in tests/wild/*.pdf; do
  echo "=== $f ==="
  ./target/release/razorpdf inspect "$f"
done
```

Dead ends (2026-10-31, do not retry as-is): `test/pdfs/issue6409.pdf`
(404 — the pdf.js tree moved), the W3C `table-example.pdf` working-example
URL (Page Not Found), `web/compressed.tracemonkey-pldi-09.pdf` (byte-dup
of `test/pdfs/tracemonkey.pdf`).
