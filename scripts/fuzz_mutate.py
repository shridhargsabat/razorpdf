#!/usr/bin/env python3
"""Deterministic mutation fuzzer seed generator for razorpdf.

Produces mutated PDFs from the wild corpus + fixtures. Every mutant is
reproducible: manifest.json records (seed, mutator, params) per file.

Usage: python3 scripts/fuzz_mutate.py [out_dir] [mutants_per_seed]
"""

import json
import os
import random
import re
import sys

SEEDS = [
    "tests/wild/arxiv_attention_2col.pdf",
    "tests/wild/arxiv_deepseek_tables.pdf",
    "tests/wild/mozilla_tracemonkey_2col.pdf",
    "tests/wild/financial_borderless_watermark.pdf",
    "tests/wild/gst_tax_invoice.pdf",
]


def m_flip(rng, data):
    """Random byte flips (1-64 bytes anywhere)."""
    data = bytearray(data)
    for _ in range(rng.randint(1, 64)):
        data[rng.randrange(len(data))] = rng.randrange(256)
    return bytes(data), {}


def m_trunc(rng, data):
    """Truncate to a random fraction (1%-99%)."""
    frac = rng.uniform(0.01, 0.99)
    return data[: int(len(data) * frac)], {"frac": round(frac, 3)}


def m_splice(rng, data):
    """Copy a random chunk over another location (dict/stream surgery)."""
    data = bytearray(data)
    size = rng.randint(4, min(4096, len(data) // 4))
    src = rng.randrange(len(data) - size)
    dst = rng.randrange(len(data) - size)
    data[dst : dst + size] = data[src : src + size]
    return bytes(data), {"size": size}


def m_length_lie(rng, data):
    """/Length values multiplied or zeroed (stream over/under-reads)."""
    mode = rng.choice(["zero", "huge", "double"])

    def fix(m):
        if mode == "zero":
            return b"/Length 0"
        if mode == "huge":
            return b"/Length 999999999"
        return b"/Length " + str(int(m.group(1)) * 2).encode()

    out, n = re.subn(rb"/Length\s+(\d+)", fix, data)
    return out, {"mode": mode, "n": n}


def m_filter_swap(rng, data):
    """Swap FlateDecode for a neighbor filter (decoder confusion)."""
    target = rng.choice([b"ASCIIHexDecode", b"ASCII85Decode", b"LZWDecode"])
    out, n = re.subn(rb"FlateDecode", target, data, count=rng.randint(1, 3))
    return out, {"target": target.decode(), "n": n}


def m_xref_break(rng, data):
    """Corrupt the xref table / startxref offset."""
    idx = data.rfind(b"startxref")
    if idx == -1:
        return m_flip(rng, data)
    data = bytearray(data)
    data[idx : idx + 9] = b"startxrXf"
    return bytes(data), {}


def m_stream_grow(rng, data):
    """Insert random garbage right after a stream keyword (filter bombs)."""
    idxs = [m.start() for m in re.finditer(rb"stream\r?\n", data)]
    if not idxs:
        return m_flip(rng, data)
    idx = rng.choice(idxs)
    eol = data.index(b"\n", idx) + 1
    junk = bytes(rng.randrange(256) for _ in range(rng.randint(16, 2048)))
    return data[:eol] + junk + data[eol:], {"junk": len(junk)}


MUTATORS = {
    "flip": m_flip,
    "trunc": m_trunc,
    "splice": m_splice,
    "length_lie": m_length_lie,
    "filter_swap": m_filter_swap,
    "xref_break": m_xref_break,
    "stream_grow": m_stream_grow,
}


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else "/tmp/fuzzcorpus"
    per_seed = int(sys.argv[2]) if len(sys.argv) > 2 else 40
    os.makedirs(out_dir, exist_ok=True)
    manifest = {}
    rng = random.Random(0xC10C)
    for seed in SEEDS:
        data = open(seed, "rb").read()
        base = os.path.splitext(os.path.basename(seed))[0]
        names = sorted(MUTATORS)
        for i in range(per_seed):
            name = names[i % len(names)]
            mutant, params = MUTATORS[name](rng, data)
            fname = f"{base}_{name}_{i:03d}.pdf"
            with open(os.path.join(out_dir, fname), "wb") as f:
                f.write(mutant)
            manifest[fname] = {"seed": seed, "mutator": name, "params": params}
    with open(os.path.join(out_dir, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=1)
    print(f"wrote {len(manifest)} mutants to {out_dir}")


if __name__ == "__main__":
    main()
