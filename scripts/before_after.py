#!/usr/bin/env python3
"""Before/after images: PDF render + boxes beside extracted Markdown.

Regenerate with (from repo root):
    .venv/bin/python scripts/before_after.py

Outputs (all from real wild-corpus PDFs via the release CLI):
    docs/assets/before-after-invoice.png     (GST bordered 9-col table)
    docs/assets/before-after-borderless.png (borderless table + rotated watermark)
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pymupdf
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
CLI = ROOT / "target/release/razorpdf"
DPI = 150
LEFT_W = 560

MONO = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
SANS = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
SANS_BOLD = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"

BG = (248, 250, 252)
CARD = (255, 255, 255)
INK = (15, 23, 42)
MUTED = (100, 116, 139)
LINE = (203, 213, 225)
TABLE_BOX = (22, 163, 74)
TEXT_BOX = (37, 99, 235)
MARK_BOX = (234, 88, 12)  # watermark / special
TITLE_BG = (15, 23, 42)


def parse(pdf: Path) -> dict:
    json_path = Path("/tmp/ba_tmp.json")
    subprocess.run(
        [str(CLI), "parse", str(pdf), "-o", "/dev/null", "--json", str(json_path)],
        check=True,
        capture_output=True,
    )
    return json.loads(json_path.read_text())


def render(pdf: Path) -> tuple[Image.Image, float]:
    doc = pymupdf.open(str(pdf))
    page = doc[0]
    zoom = DPI / 72.0
    pix = page.get_pixmap(matrix=pymupdf.Matrix(zoom, zoom))
    img = Image.frombytes("RGB", (pix.width, pix.height), pix.samples)
    return img, pix.width / page.rect.width


def overlay(
    img: Image.Image, scale: float, chunks: list[dict], mark_tall: bool
) -> Image.Image:
    overlay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    boxes: list[list[float]] = []
    for c in chunks:
        b = c["bbox"]
        box = [b["x0"] * scale, b["y0"] * scale, b["x1"] * scale, b["y1"] * scale]
        boxes.append(box)
        if c["markdown"].lstrip().startswith("|"):
            draw.rectangle(box, outline=TABLE_BOX + (255,), width=5)
        elif mark_tall and (b["y1"] - b["y0"]) > 100:
            draw.rectangle(box, outline=MARK_BOX + (255,), width=4)
        else:
            draw.rectangle(box, outline=TEXT_BOX + (110,), width=2)
    base = img.convert("RGBA")
    base.alpha_composite(overlay)
    rgb = base.convert("RGB")
    # crop to content union + margin (both PDFs are mostly empty page)
    x0 = max(0, int(min(b[0] for b in boxes)) - 40)
    y0 = max(0, int(min(b[1] for b in boxes)) - 40)
    x1 = min(rgb.size[0], int(max(b[2] for b in boxes)) + 40)
    y1 = min(rgb.size[1], int(max(b[3] for b in boxes)) + 40)
    return rgb.crop((x0, y0, x1, y1))


def text_card(lines: list[str], font_size: int = 14) -> Image.Image:
    font = ImageFont.truetype(MONO, font_size)
    adv = int(font.getlength("M")) + 1  # advance, not bbox (bbox under-measures)
    lh = int(font.getbbox("Ag")[3] * 1.4)
    pad = 18
    w = max(len(ln) for ln in lines) * adv + pad * 2
    h = len(lines) * lh + pad * 2
    img = Image.new("RGB", (w, h), CARD)
    draw = ImageDraw.Draw(img)
    draw.rectangle([0, 0, w - 1, h - 1], outline=LINE, width=1)
    for i, ln in enumerate(lines):
        color = TABLE_BOX if ln.startswith("|") and "---" not in ln else INK
        if ln.startswith("| ---"):
            color = MUTED
        draw.text((pad, pad + i * lh), ln, font=font, fill=color)
    return img


def compose(
    title: str,
    caption_left: str,
    caption_right: str,
    left: Image.Image,
    right: Image.Image,
    legend: list[tuple[tuple[int, int, int], str]],
    out: Path,
) -> None:
    sans = ImageFont.truetype(SANS, 13)
    sans_bold = ImageFont.truetype(SANS_BOLD, 14)
    title_font = ImageFont.truetype(SANS_BOLD, 18)
    gap, margin = 16, 20
    cap_h, title_h, leg_h = 30, 50, 32
    content_h = max(left.size[1], right.size[1])
    total_w = left.size[0] + right.size[0] + gap + margin * 2
    total_h = title_h + cap_h + content_h + leg_h + margin * 2
    canvas = Image.new("RGB", (total_w, total_h), BG)
    draw = ImageDraw.Draw(canvas)
    draw.rectangle([0, 0, total_w, title_h], fill=TITLE_BG)
    draw.text((margin, 13), title, font=title_font, fill=(255, 255, 255))
    y = margin + title_h + cap_h
    draw.text((margin + 2, margin + title_h + 6), caption_left, font=sans_bold, fill=MUTED)
    canvas.paste(left, (margin, y))
    x2 = margin + left.size[0] + gap
    draw.text((x2 + 2, margin + title_h + 6), caption_right, font=sans_bold, fill=MUTED)
    # right card vertically centered if shorter
    canvas.paste(right, (x2, y + (content_h - right.size[1]) // 2))
    lx = margin + 2
    ly = y + content_h + 8
    for color, label in legend:
        draw.rectangle([lx, ly, lx + 14, ly + 14], outline=color, width=3)
        draw.text((lx + 20, ly - 2), label, font=sans, fill=MUTED)
        lx += 20 + int(draw.textlength(label, font=sans)) + 24
    out.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(out, optimize=True)
    print(f"wrote {out} ({canvas.size[0]}x{canvas.size[1]}, {out.stat().st_size // 1024} KB)")


def invoice() -> None:
    pdf = ROOT / "tests/wild/gst_tax_invoice.pdf"
    doc = parse(pdf)
    img, scale = render(pdf)
    cropped = overlay(img, scale, doc["chunks"], mark_tall=False)
    left = cropped.resize((LEFT_W, int(cropped.size[1] * LEFT_W / cropped.size[0])), Image.LANCZOS)
    table = next(c for c in doc["chunks"] if c["markdown"].lstrip().startswith("|"))
    lines = [
        "# TAX INVOICE (ORIGINAL FOR RECIPIENT)",
        "Supplier: Apex Cloud Infra Pvt. Ltd. | GSTIN: 27AABCA1234F1Z5",
        "Buyer: Enterprise Corp Mumbai | GSTIN: 27AAACB5678G1Z2 | State Code: 27",
        "",
    ] + [ln for ln in table["markdown"].splitlines() if ln.strip()]
    right = text_card(lines)
    compose(
        "razorpdf  ·  gst_tax_invoice.pdf  ·  bordered 9-column table",
        "INPUT + DETECTED TABLE",
        "EXTRACTED MARKDOWN",
        left,
        right,
        [(TABLE_BOX, "table bbox"), (TEXT_BOX, "text chunk")],
        ROOT / "docs/assets/before-after-invoice.png",
    )


def borderless() -> None:
    pdf = ROOT / "tests/wild/financial_borderless_watermark.pdf"
    doc = parse(pdf)
    img, scale = render(pdf)
    cropped = overlay(img, scale, doc["chunks"], mark_tall=True)
    left = cropped.resize((LEFT_W, int(cropped.size[1] * LEFT_W / cropped.size[0])), Image.LANCZOS)
    table = next(c for c in doc["chunks"] if c["markdown"].lstrip().startswith("|"))
    lines = [
        "## Q3 Unaudited Financial Summary (INR in Lakhs)",
        "",
    ] + [ln for ln in table["markdown"].splitlines() if ln.strip()]
    lines += ["", "watermark isolated in its own chunk — never merged into rows"]
    right = text_card(lines)
    compose(
        "razorpdf  ·  financial_borderless_watermark.pdf  ·  rulings-free table",
        "INPUT + DETECTED REGIONS",
        "EXTRACTED MARKDOWN",
        left,
        right,
        [(TABLE_BOX, "borderless table"), (MARK_BOX, "rotated watermark"), (TEXT_BOX, "text")],
        ROOT / "docs/assets/before-after-borderless.png",
    )


def main() -> None:
    invoice()
    borderless()


if __name__ == "__main__":
    main()
