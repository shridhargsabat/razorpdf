"""razorpdf Gradio demo: upload a PDF, get Markdown + grounded chunks + bbox view.

Run locally (needs a razorpdf install: `pip install razorpdf`
or `maturin develop` from crates/razorpdf-py):
    pip install -r demo/requirements.txt
    python demo/app.py

Deploy to Hugging Face Spaces: push this directory as a Space (sdk: gradio).
"""

from __future__ import annotations

import io
import json
import subprocess
import tempfile
from pathlib import Path

import gradio as gr
import pymupdf
from PIL import Image, ImageDraw

TABLE_BOX = (22, 163, 74)
TEXT_BOX = (37, 99, 235)


def _render_page_with_boxes(pdf: Path, page_index: int, chunks: list[dict]) -> Image.Image:
    doc = pymupdf.open(str(pdf))
    number = min(page_index, len(doc) - 1)
    page = doc[number]
    zoom = 150 / 72
    pix = page.get_pixmap(matrix=pymupdf.Matrix(zoom, zoom))
    img = Image.frombytes("RGB", (pix.width, pix.height), pix.samples).convert("RGBA")
    scale = pix.width / page.rect.width
    overlay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    for c in chunks:
        if c["page_index"] != number:
            continue
        b = c["bbox"]
        box = [b["x0"] * scale, b["y0"] * scale, b["x1"] * scale, b["y1"] * scale]
        if c["markdown"].lstrip().startswith("|"):
            draw.rectangle(box, outline=TABLE_BOX + (255,), width=4)
        else:
            draw.rectangle(box, outline=TEXT_BOX + (110,), width=2)
    img.alpha_composite(overlay)
    return img.convert("RGB")


def pdf_loaded(pdf_path: str | None) -> tuple:
    """Bound the page slider to the uploaded PDF (runs on file change)."""
    if not pdf_path:
        return gr.update(maximum=1, value=0), "upload a PDF, then hit Parse"
    n = len(pymupdf.open(str(pdf_path)))
    return (
        gr.update(maximum=max(n - 1, 1), value=0),
        f"{n} page(s) loaded — hit Parse",
    )


def per_page_breakdown(chunks: list[dict]) -> str:
    """Compact 'p0: 6c/0r, p1: 9c/1r' chunk/region counts (capped)."""
    pages: dict[int, list[int]] = {}
    for c in chunks:
        p = pages.setdefault(c["page_index"], [0, 0])
        p[0] += 1
        p[1] += 1 if c["parsed_by_vlm"] else 0
    parts = [f"p{i}: {v[0]}c/{v[1]}r" for i, v in sorted(pages.items())]
    if len(parts) > 12:
        parts = parts[:12] + [f"…+{len(parts) - 12}"]
    return ", ".join(parts)


def parse_pdf(pdf_path: str, page: int, vlm_url: str) -> tuple[str, dict, Image.Image, str]:
    """Run razorpdf and return (markdown, chunks_json, annotated_page, stats)."""
    import razorpdf

    pdf = Path(pdf_path)
    url = vlm_url.strip() or None
    doc = razorpdf.parse_file(str(pdf), vlm_url=url)
    chunks = doc["chunks"]
    slim = [
        {k: (c[k][:120] + "…" if k == "markdown" and len(c[k]) > 120 else c[k])
         for k in ("chunk_id", "page_index", "bbox", "breadcrumb", "markdown",
                   "token_estimate", "parsed_by_vlm")}
        for c in chunks[:50]
    ]
    stats = (
        f"{doc['total_pages']} pages · {len(chunks)} chunks · "
        f"{doc['vlm_fallback_regions']} VLM regions · {doc['elapsed_ms']:.0f} ms"
        f" | {per_page_breakdown(chunks)}"
    )
    img = _render_page_with_boxes(pdf, max(page, 0), chunks)
    return doc["markdown"][:20000], {"chunks": slim}, img, stats


with gr.Blocks(title="razorpdf demo") as demo:
    gr.Markdown(
        "# razorpdf — hybrid local-first PDF parser\n"
        "Deterministic Rust geometry for 90%+ of pages, selective sub-page VLM "
        "for the rest. Upload a PDF to get Markdown + grounded chunks with bboxes."
    )
    with gr.Row():
        pdf_in = gr.File(label="PDF", file_types=[".pdf"])
        with gr.Column():
            page_in = gr.Slider(
                label="Preview page", minimum=0, maximum=1, step=1, value=0
            )
            vlm_in = gr.Textbox(
                label="VLM endpoint (optional, OpenAI-compatible)",
                placeholder="http://localhost:11434/v1/chat/completions",
            )
            go = gr.Button("Parse", variant="primary")
    stats_out = gr.Textbox(label="Stats", interactive=False)
    with gr.Row():
        md_out = gr.Markdown(label="Markdown")
    with gr.Row():
        json_out = gr.JSON(label="Chunks (first 50, markdown trimmed)")
        img_out = gr.Image(label="Page + chunk bboxes (green = table)")
    go.click(parse_pdf, inputs=[pdf_in, page_in, vlm_in],
             outputs=[md_out, json_out, img_out, stats_out])
    pdf_in.change(pdf_loaded, inputs=pdf_in, outputs=[page_in, stats_out])
    gr.Markdown(
        "Keys per chunk: `chunk_id`, `page_index`, `bbox {x0,y0,x1,y1}` (PDF points), "
        "`breadcrumb`, `markdown`, `token_estimate`, `parsed_by_vlm`."
    )

if __name__ == "__main__":
    demo.launch()
