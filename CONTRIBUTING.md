# Contributing to razorpdf

> This project follows a [Code of Conduct](CODE_OF_CONDUCT.md). By
> participating you agree to uphold it. Found a security issue? Please report
> it privately per [SECURITY.md](SECURITY.md), not as a public issue.

Bug reports, benchmark additions, and pull requests are welcome.

## Ground rules

- **Hybrid positioning stays.** The fast path is pure geometry with zero
  model calls; VLM is sub-page fallback only. Changes that route whole
  pages through models don't fit this project.
- **Interfaces are stable.** Don't break the CLI (`parse`/`inspect`/`bench`/
  `mcp`), the Python API (`parse_file`/`to_markdown`), or the MCP tools
  (`parse_pdf`, `inspect_pdf_layout`, `extract_pdf_tables`) without
  discussion first (open an issue).
- **Prove it with numbers.** Parser changes must show `cargo test
  --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo run --release -p razorpdf -- bench` before/after. Throughput
  regressions need justification.
- **Real PDFs, real output.** Table/layout fixes should cite the wild-corpus
  file and show before/after Markdown (see `tests/wild/README.md`).

## Setup

```bash
git clone https://github.com/shridhargsabat/razorpdf.git && cd razorpdf
cargo test --workspace                       # 111 tests, offline
cargo clippy --workspace --all-targets -- -D warnings
```

Python bindings (needs Python 3.10–3.13):

```bash
pip install maturin && cd crates/razorpdf-py && maturin develop --release
.venv/bin/python scripts/bench_wild.py        # head-to-head vs pypdf/pdfminer/pymupdf/…
```

## Good first issues

Look for the [`good first issue`](https://github.com/shridhargsabat/razorpdf/labels/good%20first%20issue)
label — small, well-scoped starting points (CLI flags, demo polish,
packaging). Comment on the issue before starting work so efforts don't
overlap.

## Release process (maintainers)

1. Bump `version` in root `Cargo.toml` + `crates/razorpdf-py/pyproject.toml`
   (they must match; CI preflight enforces it).
2. Publish the crates (manual, needs a crates.io token — no OIDC wired):
   `cargo publish -p razorpdf-core`, wait ~60s for indexing, then
   `cargo publish -p razorpdf`. (To automate: add crates.io trusted
   publishers and re-add a `crates` job using
   `rust-lang/crates-io-auth-action`.)
3. Tag `vX.Y.Z` and push the tag — `release.yml` builds CLI binaries +
   wheels, publishes the GitHub Release and PyPI (OIDC).
4. Verify `pip install razorpdf==X.Y.Z` and `cargo install razorpdf` in a
   clean environment, then update the README install notes.
