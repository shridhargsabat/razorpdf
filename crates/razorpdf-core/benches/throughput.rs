//! Throughput benchmarks: pages/second on synthetic documents.
//!
//! Run: `cargo bench -p razorpdf-core --bench throughput`
//! Quick smoke: `cargo bench -p razorpdf-core --bench throughput -- --test`

use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use razorpdf_core::{assembler, extractor, test_fixtures, vlm::NoVlmProvider, ParserConfig};

const PAGES: usize = 100;

fn bench_extract(c: &mut Criterion) {
    let pdf = test_fixtures::mixed_paper_pdf(PAGES);
    let mut group = c.benchmark_group("extract");
    group
        .throughput(Throughput::Elements(PAGES as u64))
        .measurement_time(Duration::from_secs(8));
    group.bench_function("mixed_100_pages", |b| {
        b.iter(|| extractor::extract_document(&pdf, "bench.pdf").expect("extract"));
    });
    group.finish();
}

fn bench_assemble(c: &mut Criterion) {
    let pdf = test_fixtures::mixed_paper_pdf(PAGES);
    let config = ParserConfig::default();
    let runtime = tokio::runtime::Runtime::new().expect("bench tokio runtime");
    let mut group = c.benchmark_group("assemble");
    group
        .throughput(Throughput::Elements(PAGES as u64))
        .measurement_time(Duration::from_secs(8));
    group.bench_function("mixed_100_pages_fast_path", |b| {
        b.iter(|| {
            runtime
                .block_on(assembler::assemble_document(
                    &pdf,
                    "bench.pdf",
                    &config,
                    Arc::new(NoVlmProvider),
                ))
                .expect("assemble")
        });
    });
    group.finish();
}

criterion_group!(benches, bench_extract, bench_assemble);
criterion_main!(benches);
