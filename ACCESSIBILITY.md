# Accessibility

Everyone should be able to use and contribute to razorpdf, including people
with disabilities and people using assistive technology. This document states
our accessibility commitment, the environments we support, known limitations,
and how to report barriers.

## Commitment

We work toward [WCAG 2.2 Level AA](https://www.w3.org/TR/WCAG22/) for our web
and documentation surfaces. This is a target that guides our work, not a claim
of verified conformance. For the parts of razorpdf that are not visual at all
(CLI, library APIs), our commitment is equivalent access through text: if you
can read text, you can use razorpdf end to end.

## Supported environments

| Surface              | Accessibility notes                                          |
| -------------------- | ------------------------------------------------------------ |
| CLI (`parse`/`inspect`/`bench`/`mcp`) | Plain-text stdout/stderr; works with screen readers and Braille displays. `--json` output is available for programmatic use. We avoid color-only signaling. |
| Python API (`parse_file`/`to_markdown`) | Pure code API; documented in text with typed signatures. |
| MCP server tools     | Text in / text out over the MCP protocol.                    |
| Documentation (`README.md`, `docs/`) | Semantic Markdown: logical heading hierarchy, real lists and tables, descriptive link text, alt text on images. |
| Demo web app (`demo/app.py`, Gradio) | Keyboard-navigable for its core upload/parse flow; inherits Gradio's default component semantics. Least mature surface — see limitations. |

## Known limitations

- **Demo app:** built on stock Gradio components; we have not audited it with
  a screen reader, and focus order / live-region announcements for long parse
  jobs may be imperfect.
- **Benchmark/demo images:** `README.md` and `docs/` images carry alt text,
  but complex before/after extraction comparisons are easier to follow in the
  adjacent Markdown snippets than from the image alone.
- **PDF input itself:** razorpdf parses whatever PDF it is given; scanned or
  untagged source PDFs may produce Markdown whose reading order reflects a
  messy original. `--json` chunks include bounding-box provenance so
  downstream tools can re-order or verify.
- **No dedicated accessibility test suite yet:** checks are manual today. An
  automated pass (e.g. axe on the demo app) is a welcome contribution.

## Feedback and reporting

Accessibility barriers are bugs. Report them like any other bug:

1. **Preferred:** [open an issue](https://github.com/shridhargsabat/razorpdf/issues/new/choose)
   using the bug report form and mention "accessibility" in the title so we
   can label and prioritize it.
2. **Private:** email shridhargsabat@gmail.com with subject
   `[razorpdf a11y] <brief description>` if the report involves personal
   details you don't want public.

Please include what you were trying to do, the assistive technology and
browser/terminal involved, and what happened instead. We aim to acknowledge
reports within 3 business days.

## Contributor expectations

- All images in `README.md`, `docs/`, and examples need meaningful alt text;
  complex diagrams get a text alternative nearby (see the [W3C alt decision
  tree](https://www.w3.org/WAI/tutorials/images/decision-tree/)).
- Keep the heading hierarchy logical (no skipped levels), use real list and
  table elements, and write descriptive link text (never "click here").
- CLI output must stay usable as plain text: no information conveyed by color
  alone, and every human-readable view keeps a `--json` equivalent.
- Demo-app changes should get at least one keyboard-only pass (logical tab
  order, visible focus, no traps) before merge.
