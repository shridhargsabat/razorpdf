---
title: razorpdf demo
emoji: 📄
colorFrom: slate
colorTo: emerald
sdk: gradio
sdk_version: "5.0.0"
app_file: app.py
pinned: false
---

# razorpdf Gradio demo

Upload a PDF → Markdown + grounded chunks + bbox visualization.

**Deploy to Hugging Face Spaces:** push this directory to a new Space (it
already has the `sdk: gradio` frontmatter, `app.py`, and `requirements.txt`
— `razorpdf` 0.1.1 is on PyPI, so Spaces install it directly).

**Run locally:**

```bash
pip install -r demo/requirements.txt  # or: pip install razorpdf gradio pymupdf pillow
python demo/app.py
```
