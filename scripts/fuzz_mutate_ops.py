#!/usr/bin/env python3
"""Stage-2 content-aware mutations: stay parseable, attack deep code.

Decompresses content streams, mutates operator bytes, rebuilds the PDF
via pypdf (container stays valid so the fuzzer reaches the operand
parser, font decoder, table detectors and cropper).

Usage: .venv/bin/python scripts/fuzz_mutate_ops.py [out_dir] [variants]
"""

import json
import os
import random
import sys

from pypdf import PdfReader, PdfWriter
from pypdf.generic import (
    ArrayObject,
    DecodedStreamObject,
    FloatObject,
    NameObject,
    NumberObject,
)

SEEDS = [
    "tests/wild/arxiv_attention_2col.pdf",
    "tests/wild/arxiv_deepseek_tables.pdf",
    "tests/wild/mozilla_tracemonkey_2col.pdf",
    "tests/wild/financial_borderless_watermark.pdf",
    "tests/wild/gst_tax_invoice.pdf",
]

HOSTILE_OPS = [
    b"999999999999 999999999999 Td\n",
    b"1e30 1e30 1e30 1e30 1e30 1e30 Tm\n",
    b"q\n" * 2000,
    b"BT\n" * 500,
    b"/NoSuchFont 9999 Tf\n",
    b"/NoSuchXObject Do\n",
    b"[" + b"(s) 1 " * 5000 + b"] TJ\n",
    b"(" + b"A" * 200_000 + b") Tj\n",
    b"0 0 m 0 0 l 0 0 c h\n" * 200,
    b"1 0 0 1 99999999 99999999 cm\n",
    b"() Tj\n" * 1000,
    b"<FFFE0041> Tj\n",
    b"[0 0 0 0 0 0] d\n",
    b"999999 w 999999 M 999999 i\n",
]


def op_bytes(rng, data, variant):
    """Mutate decompressed content-stream bytes."""
    if variant == 0:
        data = bytearray(data)
        for _ in range(rng.randint(4, 200)):
            data[rng.randrange(len(data))] = rng.randrange(256)
        return bytes(data)
    if variant == 1:
        inject = b"".join(rng.sample(HOSTILE_OPS, rng.randint(2, 6)))
        at = rng.randrange(len(data) + 1)
        return data[:at] + inject + data[at:]
    # variant 2: hostile-only stream (valid container, pure attack ops)
    return b"".join(rng.sample(HOSTILE_OPS, 8))


def mediabox(rng, page):
    choice = rng.randrange(5)
    boxes = [
        [0, 0, 0, 0],
        [0, 0, -612, -792],
        [0, 0, 1e9, 1e9],
        [612, 792, 0, 0],
        [0, 0, 612, 792],
    ]
    vals = boxes[choice]
    page[NameObject("/MediaBox")] = ArrayObject(
        [FloatObject(v) for v in vals]
    )
    return choice


def cmap_garbage(rng, writer, page):
    """Replace ToUnicode streams with random bytes. Returns count."""
    try:
        fonts = page["/Resources"]["/Font"]
    except Exception:
        return 0
    n = 0
    for key in list(fonts.keys()):
        try:
            font = fonts[key].get_object()
            if "/ToUnicode" not in font:
                continue
            junk = bytes(rng.randrange(256) for _ in range(rng.randint(8, 4000)))
            s = DecodedStreamObject()
            s.set_data(junk)
            font[NameObject("/ToUnicode")] = writer._add_object(s)
            n += 1
        except Exception:
            continue
    return n


def mutate_pdf(seed, rng, variant, aggressive_media):
    reader = PdfReader(seed)
    writer = PdfWriter()
    for page in reader.pages:
        writer.add_page(page)
    for page in writer.pages:
        try:
            raw = page.get_contents().get_data()
        except Exception:
            continue
        if not raw:
            continue
        mutated = op_bytes(rng, raw, variant)
        s = DecodedStreamObject()
        s.set_data(mutated)
        page[NameObject("/Contents")] = writer._add_object(s)
        if aggressive_media:
            mediabox(rng, page)
    cmap_hits = sum(cmap_garbage(rng, writer, p) for p in writer.pages)
    return writer, cmap_hits


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else "/tmp/fuzzcorpus2"
    os.makedirs(out_dir, exist_ok=True)
    rng = random.Random(0x0BE7)
    manifest = {}
    for seed in SEEDS:
        base = os.path.splitext(os.path.basename(seed))[0]
        for v in range(12):
            variant = v % 3
            aggressive_media = v % 4 == 3
            try:
                writer, cmap_hits = mutate_pdf(
                    seed, rng, variant, aggressive_media)
                fname = f"{base}_ops{v}.pdf"
                with open(os.path.join(out_dir, fname), "wb") as f:
                    writer.write(f)
                manifest[fname] = {"seed": seed, "variant": variant,
                                   "media": aggressive_media,
                                   "cmaps": cmap_hits}
            except Exception as e:  # noqa: BLE001 - skip unmutatable seeds
                manifest[f"SKIP_{base}_ops{v}"] = {"error": str(e)[:120]}
    with open(os.path.join(out_dir, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=1)
    n = sum(1 for k in manifest if not k.startswith("SKIP"))
    print(f"wrote {n} stage-2 mutants to {out_dir}")


if __name__ == "__main__":
    main()
