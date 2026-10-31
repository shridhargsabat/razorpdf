#!/usr/bin/env python3
"""Animated demo GIF: CLI parse of a GST invoice -> Markdown table -> chunk JSON.

Regenerate with (from repo root):
    .venv/bin/python scripts/demo_gif.py

All shown text is real `razorpdf` output (blank lines compressed for pacing).
Output: docs/assets/demo.gif
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs/assets/demo.gif"

MONO = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
SANS = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"

BG = (13, 17, 23)  # GitHub dark
BAR = (22, 27, 34)
GREEN = (63, 185, 80)
TEXT = (201, 209, 217)
DIM = (110, 119, 129)
TABLE_HL = (88, 166, 255)

FONT_SIZE = 15
FPS = 10

SCENES: list[tuple[str, list[str]]] = [
    (
        "razorpdf parse gst_tax_invoice.pdf",
        [
            "# TAX INVOICE (ORIGINAL FOR RECIPIENT)",
            "Supplier: Apex Cloud Infra Pvt. Ltd. | GSTIN: 27AABCA1234F1Z5",
            "Buyer: Enterprise Corp Mumbai | GSTIN: 27AAACB5678G1Z2 | State Code: 27",
            "| Sr | Description of Service | HSN/SAC | Qty | Rate (INR) | Taxable | CGST 9% | SGST 9% | Total |",
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- |",
            "| 1 | Dedicated GPU Node Rental | 998315 | 2 | 45,000.00 | 90,000.00 | 8,100.00 | 8,100.00 | 1,06,200.00 |",
            "| 2 | Managed Firewall & SD-WAN | 998314 | 1 | 15,000.00 | 15,000.00 | 1,350.00 | 1,350.00 | 17,700.00 |",
        ],
    ),
    (
        "razorpdf parse gst_tax_invoice.pdf --json chunks.json",
        [
            "{",
            '  "chunk_id": "p0-003",',
            '  "page_index": 0,',
            '  "bbox": {"x0": 40.0, "y0": 116.0, "x1": 575.0, "y1": 182.0},',
            '  "breadcrumb": ["TAX INVOICE (ORIGINAL FOR RECIPIENT)"],',
            '  "markdown": "| Sr | Description of Service | ... |",',
            '  "token_estimate": 92,',
            '  "parsed_by_vlm": false',
            "}",
        ],
    ),
]


def render(lines: list[tuple[str, tuple]], w: int, h: int, font, title: str) -> Image.Image:
    img = Image.new("RGB", (w, h), BG)
    draw = ImageDraw.Draw(img)
    draw.rectangle([0, 0, w, 34], fill=BAR)
    for i, color in enumerate([(255, 95, 86), (255, 189, 46), (39, 201, 63)]):
        draw.ellipse([14 + i * 22, 11, 26 + i * 22, 23], fill=color)
    sans = ImageFont.truetype(SANS, 13)
    tw = draw.textlength(title, font=sans)
    draw.text(((w - tw) / 2, 8), title, font=sans, fill=DIM)
    lh = 22
    y = 48
    for text, color in lines:
        draw.text((18, y), text, font=font, fill=color)
        y += lh
    return img


def main() -> None:
    font = ImageFont.truetype(MONO, FONT_SIZE)
    adv = int(font.getbbox("M")[2])
    longest = max(len(cmd) + 2 for cmd, _ in SCENES)
    longest = max([longest] + [len(ln) for _, out in SCENES for ln in out])
    w = min(longest * adv + 36, 1100)
    max_lines = max(len(out) for _, out in SCENES) + 2
    h = 48 + max_lines * 22 + 16

    frames: list[Image.Image] = []
    durations: list[int] = []
    prompt = "$ "
    for scene_i, (cmd, out) in enumerate(SCENES):
        # type the command (2 chars/frame)
        for n in range(2, len(cmd) + 3, 2):
            shown = cmd[:n]
            cursor = "▊" if (n // 2) % 2 == 0 else ""
            frames.append(render([(prompt + shown + cursor, GREEN)], w, h, font, "razorpdf demo"))
            durations.append(int(1000 / FPS))
        # command line complete, brief pause
        full = [(prompt + cmd, GREEN)]
        frames.append(render(full, w, h, font, "razorpdf demo"))
        durations.append(int(1000 / FPS))
        # reveal output line by line
        shown_lines: list[tuple[str, tuple[int, int, int]]] = list(full)
        for ln in out:
            color = TABLE_HL if ln.startswith("|") else TEXT
            if scene_i == 1 and ln.strip().startswith('"'):
                color = TEXT
            shown_lines.append((ln, color))
            frames.append(render(list(shown_lines), w, h, font, "razorpdf demo"))
            durations.append(int(1000 / FPS))
        # hold (single long frame — duplicate frames collapse in GIF)
        frames.append(render(list(shown_lines), w, h, font, "razorpdf demo"))
        durations.append(2000)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    frames[0].save(OUT, save_all=True, append_images=frames[1:], duration=durations, loop=0)
    kb = OUT.stat().st_size // 1024
    print(f"wrote {OUT} ({w}x{h}, {len(frames)} frames, {kb} KB)")


if __name__ == "__main__":
    main()
