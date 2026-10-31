# VLM providers (sub-page fallback only)

Geometry-first: most pages never touch a model. When triage flags a region
(`S(R) ≥ 0.45`, scanned pages, diagram clusters), razorpdf POSTs a WebP crop
of **that bbox only** to an OpenAI-compatible `/chat/completions` endpoint
with `temperature: 0`, parses `choices[0].message.content` as Markdown
(code fences stripped), and keeps raw geometric text on any failure.

## Configuration

| Setting | CLI flag | Env var | Default |
| :--- | :--- | :--- | :--- |
| Endpoint URL | `--vlm-url` | `RAZORPDF_VLM_URL` (or `OPENAI_BASE_URL`) | none (raw-text fallback) |
| API key (Bearer) | — | `OPENAI_API_KEY` | none (no auth header) |
| Model id | — | `RAZORPDF_VLM_MODEL` | `qwen2-vl-7b-instruct` |
| Offline demo | `--mock-vlm` | — | — |
| Crop source | `--canvas-source auto` | — | synthetic canvas |

Python mirrors the CLI: `razorpdf.parse_file("x.pdf", vlm_url="…")`
(model/key still come from the env vars above).

## Ollama (local, no key)

```bash
ollama pull qwen2-vl:7b   # any vision-capable tag works
export RAZORPDF_VLM_MODEL="qwen2-vl:7b"
./target/release/razorpdf parse scan.pdf \
  --vlm-url http://localhost:11434/v1/chat/completions
```

## OpenAI

```bash
export OPENAI_API_KEY="sk-..."
export RAZORPDF_VLM_MODEL="gpt-4o-mini"   # or gpt-4o
./target/release/razorpdf parse scan.pdf \
  --vlm-url https://api.openai.com/v1/chat/completions
```

`OPENAI_BASE_URL` also works instead of `--vlm-url` (same value).

## Gemini (OpenAI-compatibility endpoint)

Gemini exposes an OpenAI-compatible route, so the same Bearer flow works —
put the Gemini key in `OPENAI_API_KEY`:

```bash
export OPENAI_API_KEY="<gemini-api-key>"
export RAZORPDF_VLM_MODEL="gemini-2.0-flash"
./target/release/razorpdf parse scan.pdf \
  --vlm-url https://generativelanguage.googleapis.com/v1beta/openai/chat/completions
```

## Any other OpenAI-compatible endpoint

vLLM, LM Studio, Together, OpenRouter, Ollama-compatible gateways — anything
speaking `POST /chat/completions` with `image_url` parts:

```bash
export OPENAI_API_KEY="<key-if-needed>"
export RAZORPDF_VLM_MODEL="<model-id>"
./target/release/razorpdf parse scan.pdf --vlm-url <endpoint>
```

## Cost intuition

Measure, don't guess: `razorpdf inspect` shows per-page triage verdicts, and
the JSON output's `vlm_fallback_regions` counts regions that would call the
model (48 across our 117-page wild suite; invoices: 0). A typical sub-page
crop is ~425 image tokens vs ~2,125 for a full 150 dpi page — see the cost
table in the main README.
