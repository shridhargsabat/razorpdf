# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1   | :x:                |

Only the latest minor release line receives security fixes. If you are on an
older version, please upgrade and confirm the issue still reproduces before
reporting.

## Reporting a Vulnerability

**Do not open a public issue for security vulnerabilities.** Use one of these
private channels:

1. **Preferred:** [Open a private security
   advisory](https://github.com/shridhargsabat/razorpdf/security/advisories/new)
2. **Fallback:** Email shridhargsabat@gmail.com with subject
   `[razorpdf security] <brief description>`

Please include:

- A description of the vulnerability and its potential impact
- Steps to reproduce (sample PDF, command line, config — redact any secrets)
- The razorpdf version and install method (`pip`, `cargo`, Docker, binary)
- Any suggested mitigation, if you have one

### What to expect

- **Acknowledgement** within 3 business days
- **Status updates** at least every 7 days until resolved
- **Coordinated disclosure:** we will agree on a fix + release timeline with
  you before any public disclosure, and credit you in the release notes unless
  you prefer to stay anonymous

## Scope

In scope:

- The `razorpdf` CLI and MCP server (`crates/razorpdf`)
- The core parsing library (`crates/razorpdf-core`), including PDF parsing,
  geometry, table detection, and chunk grounding code
- The Python bindings (`crates/razorpdf-py`)
- Docker images and release workflows in this repository

Out of scope:

- Third-party VLM providers and API keys (report those to the provider)
- Vulnerabilities in dependencies without a demonstrating exploit path through
  razorpdf itself — though we still appreciate the heads-up so we can upgrade
- Sample PDFs under `samples/` and `tests/`

## Notes for PDF parsing

PDFs are untrusted input by nature. When reporting parser issues (panics,
excessive memory/CPU, path traversal via output files), a minimal crashing
file plus the exact command is the most useful report. Our fuzzer harness lives
in `scripts/fuzz_*.py` if you want to minimize a reproducer.
