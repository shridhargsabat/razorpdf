//! Dump the four synthetic fixtures to disk for manual CLI QA.
//!
//! Usage: `cargo run -p razorpdf-core --example dump_fixtures -- /tmp/fx`

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/razorpdf-fx"));
    std::fs::create_dir_all(&dir)?;
    for (name, bytes) in razorpdf_core::test_fixtures::all_fixtures() {
        let path = dir.join(format!("{name}.pdf"));
        std::fs::write(&path, &bytes)?;
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}
