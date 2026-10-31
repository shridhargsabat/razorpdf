#!/usr/bin/env python3
"""Hero demo image: PDF page + triage boxes | Markdown table | grounded chunk JSON.

Regenerate with (from repo root):
    .venv/bin/python scripts/hero_demo.py

Inputs: a real wild-corpus PDF parsed by the release CLI (no fixtures).
Output: docs/assets/hero-demo.png
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pymupdf
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
PDF = ROOT / "tests/wild/arxiv_attention_2col.pdf"
PAGE_INDEX = 5  # Table 1 ("Complexity per Layer") + two-column body text
CLI = ROOT / "target/release/razorpdf"
OUT = ROOT / "docs/assets/hero-demo.png"

DPI = 150
CROP_FRAC = 0.60  # top slice of the page (table + surrounding columns)
PANEL1_W = 680

MONO = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
SANS = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
SANS_BOLD = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"

BG = (248, 250, 252)  # slate-50
CARD = (255, 255, 255)
INK = (15, 23, 42)  # slate-900
MUTED = (100, 116, 139)  # slate-500
LINE = (203, 213, 225)  # slate-300
TABLE_BOX = (22, 163, 74)  # green-600
TEXT_BOX = (37, 99, 235)  # blue-600
TITLE_BG = (15, 23, 42)


def parse() -> dict:
    md_path = Path("/tmp/hero_tmp.md")
    json_path = Path("/tmp/hero_tmp.json")
    subprocess.run(
        [str(CLI), "parse", str(PDF), "-o", str(md_path), "--json", str(json_path)],
        check=True,
        capture_output=True,
    )
    return json.loads(json_path.read_text())


def render_page() -> tuple[Image.Image, float]:
    doc = pymupdf.open(str(PDF))
    page = doc[PAGE_INDEX]
    zoom = DPI / 72.0
    pix = page.get_pixmap(matrix=pymupdf.Matrix(zoom, zoom))
    img = Image.frombytes("RGB", (pix.width, pix.height), pix.samples)
    scale = pix.width / page.rect.width
    crop_h = int(pix.height * CROP_FRAC)
    return img.crop((0, 0, pix.width, crop_h)), scale


def overlay_boxes(img: Image.Image, scale: float, chunks: list[dict]) -> dict[str, int]:
    overlay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    counts = {"text": 0, "table": 0, "vlm": 0}
    for kind in ("text", "table"):  # text first so tables draw on top
        for c in chunks:
            is_table = c["markdown"].lstrip().startswith("|")
            if (kind == "table") != is_table:
                continue
            b = c["bbox"]
            box = [b["x0"] * scale, b["y0"] * scale, b["x1"] * scale, b["y1"] * scale]
            if box[3] > img.size[1]:  # below the crop
                continue
            if is_table:
                draw.rectangle(box, outline=TABLE_BOX + (255,), width=5)
                counts["table"] += 1
            else:
                draw.rectangle(box, outline=TEXT_BOX + (110,), width=2)
                counts["text"] += 1
            counts["vlm"] += bool(c["parsed_by_vlm"])
    base = img.convert("RGBA")
    base.alpha_composite(overlay)
    img.paste(base.convert("RGB"))
    return counts


def text_panel(
    lines: list[str], font: ImageFont.FreeTypeFont, pad: int = 18
) -> Image.Image:
    adv = int(font.getbbox("M")[2])
    lh = int(font.getbbox("Ag")[3] * 1.35)
    w = max(len(ln) for ln in lines) * adv + pad * 2
    h = len(lines) * lh + pad * 2
    img = Image.new("RGB", (w, h), CARD)
    draw = ImageDraw.Draw(img)
    for i, ln in enumerate(lines):
        draw.text((pad, pad + i * lh), ln, font=font, fill=INK)
    return img


def main() -> int:
    if not CLI.exists():
        print(f"build the CLI first: cargo build --release -p razorpdf-cli", file=sys.stderr)
        return 1
    doc = parse()
    chunks = [c for c in doc["chunks"] if c["page_index"] == PAGE_INDEX]
    tables = [c for c in chunks if c["markdown"].lstrip().startswith("|")]
    if not tables:
        print("no table chunk on page", PAGE_INDEX, file=sys.stderr)
        return 1
    table = max(tables, key=lambda c: len(c["markdown"]))

    page_img, scale = render_page()
    counts = overlay_boxes(page_img, scale, chunks)
    panel1 = page_img.resize(
        (PANEL1_W, int(page_img.size[1] * PANEL1_W / page_img.size[0])), Image.LANCZOS
    )

    mono = ImageFont.truetype(MONO, 15)
    table_lines = [ln for ln in table["markdown"].splitlines() if ln.strip()]
    panel2 = text_panel(table_lines, mono)

    slim = dict(table)
    first_line = slim["markdown"].splitlines()[0]
    slim["markdown"] = first_line + "\n…"
    json_lines = json.dumps(slim, indent=1, ensure_ascii=False).splitlines()
    panel3 = text_panel(json_lines, mono)

    # ---- compose ----
    sans = ImageFont.truetype(SANS, 14)
    sans_bold = ImageFont.truetype(SANS_BOLD, 15)
    title_font = ImageFont.truetype(SANS_BOLD, 19)
    captions = [
        f"INPUT + TRIAGE BOXES  ·  p.{PAGE_INDEX + 1} top slice",
        "EXTRACTED MARKDOWN TABLE",
        "GROUNDED CHUNK JSON",
    ]
    panels = [panel1, panel2, panel3]
    gap, margin = 16, 20
    cap_h, title_h = 30, 52
    content_h = max(p.size[1] for p in panels)
    total_w = sum(p.size[0] for p in panels) + gap * 2 + margin * 2
    total_h = title_h + cap_h + content_h + margin * 2 + 34  # + legend row

    canvas = Image.new("RGB", (total_w, total_h), BG)
    draw = ImageDraw.Draw(canvas)
    draw.rectangle([0, 0, total_w, title_h], fill=TITLE_BG)
    n_vlm = sum(1 for c in chunks if c["parsed_by_vlm"])
    title = (
        f"razorpdf  ·  arxiv_attention_2col.pdf p.{PAGE_INDEX + 1}  ·  "
        f"{len(chunks)} chunks, {len(tables)} table, {n_vlm} VLM calls"
    )
    draw.text((margin, 14), title, font=title_font, fill=(255, 255, 255))

    def chip(x: int, y: int, color: tuple, label: str) -> int:
        draw.rectangle([x, y, x + 14, y + 14], outline=color, width=3)
        draw.text((x + 20, y - 2), label, font=sans, fill=MUTED)
        return x + 20 + int(draw.textlength(label, font=sans)) + 24

    x = margin
    for i, (panel, caption) in enumerate(zip(panels, captions)):
        # equal-height white card
        draw.rounded_rectangle(
            [x, margin + title_h + cap_h, x + panel.size[0], margin + title_h + cap_h + content_h],
            radius=8,
            fill=CARD,
            outline=LINE,
            width=1,
        )
        canvas.paste(panel, (x, margin + title_h + cap_h))
        draw.text((x + 2, margin + title_h + 6), caption, font=sans_bold, fill=MUTED)
        x += panel.size[0] + gap

    # legend under panel 1
    lx, ly = margin + 2, margin + title_h + cap_h + content_h + 10
    lx = chip(lx, ly, TABLE_BOX, "table bbox")
    lx = chip(lx, ly, TEXT_BOX, "text chunk")
    draw.text((lx, ly - 2), "coordinates in PDF points (top-left origin)", font=sans, fill=MUTED)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(OUT, optimize=True)
    kb = OUT.stat().st_size // 1024
    print(f"wrote {OUT} ({canvas.size[0]}x{canvas.size[1]}, {kb} KB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
