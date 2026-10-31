//! TEMP probe: H-rule + island geometry on a page; font-size distribution.

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("pdf");
    let page: usize = std::env::args().nth(2).expect("page").parse()?;
    let bytes = std::fs::read(&path)?;
    let pages = razorpdf_core::extractor::extract_document(&bytes, &path)?;
    let p = &pages[page];
    println!(
        "page {page}: {} spans, {} vectors",
        p.spans.len(),
        p.vectors.len()
    );

    // H rules sorted by y.
    let mut rules: Vec<_> = p
        .vectors
        .iter()
        .filter(|s| (s.y1 - s.y0).abs() < 1.5 && (s.x1 - s.x0).abs() > 40.0)
        .collect();
    rules.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap());
    println!("--- H rules >40pt: {}", rules.len());
    for r in rules.iter().take(25) {
        println!(
            "  y={:.0} x={:.0}..{:.0} len={:.0}",
            r.y0,
            r.x0.min(r.x1),
            r.x0.max(r.x1),
            (r.x1 - r.x0).abs()
        );
    }

    // H-segment y histogram (tiled-rect detection: stroked grids show
    // count≈1 per line; tiled heatmap cells show dozens).
    let mut hist: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
    for s in &p.vectors {
        let dx = (s.x1 - s.x0).abs();
        let dy = (s.y1 - s.y0).abs();
        if dy < 1.5 && dx > 15.0 {
            *hist
                .entry(((s.y0 + s.y1) * 0.5).round() as i32)
                .or_default() += 1;
        }
    }
    let mut top: Vec<_> = hist.into_iter().collect();
    top.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    println!("--- H y-histogram top 8 ---");
    for (y, n) in top.iter().take(8) {
        println!("  y={y} n={n}");
    }
    let lines = razorpdf_core::geometry::merge_spans_to_lines(&p.spans);
    println!("--- lines: {}", lines.len());
    let mut sizes: Vec<f32> = lines.iter().map(|l| l.font_size).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "font sizes: min={:.1} p25={:.1} med={:.1} p75={:.1} max={:.1}",
        sizes[0],
        sizes[sizes.len() / 4],
        sizes[sizes.len() / 2],
        sizes[sizes.len() * 3 / 4],
        sizes[sizes.len() - 1]
    );
    // Weighted median by text length.
    let total: usize = lines.iter().map(|l| l.text.len()).sum();
    let mut acc = 0usize;
    let mut by_size = lines.iter().collect::<Vec<_>>();
    by_size.sort_by(|a, b| a.font_size.partial_cmp(&b.font_size).unwrap());
    for l in &by_size {
        acc += l.text.len();
        if acc * 2 >= total {
            println!("length-weighted median font: {:.1}", l.font_size);
            break;
        }
    }
    // Detector verdicts.
    let med = razorpdf_core::geometry::body_font_size(&lines);
    let ruled = razorpdf_core::geometry::detect_ruled_tables(&p.vectors, &lines, med);
    let borderless = razorpdf_core::geometry::detect_borderless_tables(&lines, med);
    println!(
        "--- ruled: {} borderless: {} (body font {med:.1}) ---",
        ruled.len(),
        borderless.len()
    );
    // Debug: dump the synthetic canvas (with decoded image pixels) to PNG.
    let img = razorpdf_core::vlm::cropper::render_synthetic_canvas(p, 72);
    let out = format!("/tmp/canvas_p{page}.png");
    if let Err(e) = img.save(&out) {
        println!("canvas save failed: {e}");
    } else {
        println!(
            "--- canvas: {out} (embedded={}) ---",
            p.embedded_images.len()
        );
    }
    for t in ruled.iter().chain(borderless.iter()) {
        let vectors_inside = p
            .vectors
            .iter()
            .filter(|s| {
                let (x0, x1) = (s.x0.min(s.x1), s.x0.max(s.x1));
                let (y0, y1) = (s.y0.min(s.y1), s.y0.max(s.y1));
                x0 < t.bbox.x1 && x1 > t.bbox.x0 && y0 < t.bbox.y1 && y1 > t.bbox.y0
            })
            .count();
        println!(
            "  {:?} {}x{} vec={} bbox={:?}",
            t.source,
            t.n_rows(),
            t.n_cols(),
            vectors_inside,
            t.bbox
        );
        for row in &t.rows {
            println!("    {row:?}");
        }
    }
    // Band mode: island/gap structure of lines within [y0, y1].
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 5 {
        let (by0, by1): (f32, f32) = (args[3].parse()?, args[4].parse()?);
        println!("--- band {by0}..{by1} ---");
        let mut band: Vec<_> = lines
            .iter()
            .filter(|l| l.bbox.y0 >= by0 && l.bbox.y0 <= by1)
            .collect();
        band.sort_by(|a, b| a.bbox.y0.partial_cmp(&b.bbox.y0).unwrap());
        for l in band {
            let spans: Vec<String> = l
                .spans
                .iter()
                .map(|s| format!("[{:.0}..{:.0}]{:?}", s.bbox.x0, s.bbox.x1, s.text))
                .collect();
            println!(
                "  y={:.0}..{:.0} x={:.0}..{:.0} size={:.1} {:?}\n    spans: {spans:?}",
                l.bbox.y0, l.bbox.y1, l.bbox.x0, l.bbox.x1, l.font_size, l.text,
            );
        }
    }
    Ok(())
}
