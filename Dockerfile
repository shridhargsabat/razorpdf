# syntax=docker/dockerfile:1

# ---- builder: compile the CLI only (no Python needed for -p razorpdf) ----
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p razorpdf

# ---- runtime: slim Debian, static-ish binary (rustls TLS, no OpenSSL) ----
# Runs as root by design: a local batch CLI writing to mounted volumes.
FROM debian:bookworm-slim
COPY --from=builder /app/target/release/razorpdf /usr/local/bin/razorpdf
WORKDIR /work
ENTRYPOINT ["razorpdf"]
CMD ["--help"]
