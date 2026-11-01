# PR Notes — audit/logic-leak-hunt (review follow-ups)

Review of `main...HEAD`: 1 security fix + 16 probes, plus review-driven
hardening. This file records the trade-offs that need a human call.
(Suggested: keep for the review, drop before merge.)

## Fixed during review (unambiguous)

1. **Silent spill failures** — both `O_EXCL` sites used `.ok()?`, discarding
   the `io::Error`. Now `tracing::warn!` with path + error (repo-standard
   `tracing` facade, no new dependency). The `None` → synthetic-canvas
   fallback itself is by design (see below).
2. **Windows portability** — the symlink repro used `std::os::unix` with no
   gate, breaking `cargo test` compilation on Windows. Now `#[cfg(unix)]`.
   (CI is ubuntu-only, but the crate publishes to crates.io.)
3. **Weak oracles** — the empty-show and zero-font probes asserted only page
   count; both now also assert finite bboxes on any produced spans.

## Verified, no change needed

- **Positive pdftoppm path**: the pre-existing `pdftoppm_rasterizes_when_present`
  unit test covers it (`Some` + 612×792 + text pixels when the binary exists,
  and it proves the pre-created empty output file doesn't break poppler).
  Suite green on this box (poppler present) ⇒ success path intact.
- **Caller contract**: `render_page_canvas` falls back to the synthetic canvas
  on `None`, so fail-closed is graceful degradation, never a failed parse.
- **Cleanup guard vs planted links**: `remove_file` on a symlink removes the
  link, never the target — safe on the fail-closed path.
- **Fearless parallel testing**: the symlink farm is pid-namespaced and skips
  occupied tags; the only other in-repo caller of the spill fn lives in the
  lib unit-test binary (separate process, separate counter).

## Trade-offs for human decision

1. **Fail-closed vs retry-on-EEXIST.** A collision (planted link, or a stale
   file after SIGKILL + pid reuse) yields `None` → synthetic canvas. A retry
   with the next tag would self-heal stale files but gives a planter a way to
   force tag-walking (needs a loop cap) and complicates the code. Current
   choice: no retry; stale files self-heal anyway as the counter advances.
2. **`Option` vs `Result`.** The spill fn returns `None` for every failure
   mode (missing binary, timeout, EEXIST, short write). Callers only need the
   fallback bit, so `Option` is honest — but operators get cause detail only
   via the new `warn!` logs. Promoting to `Result` is API churn for little
   gain; recommend staying with `Option`.
3. **Predictable names vs `tempfile`.** `O_EXCL` closes the hole; random names
   (via the `tempfile` crate) would add defense-in-depth at the cost of a new
   dependency. Recommend staying dependency-free — `create_new` is the
   correct primitive here, not obscurity.
4. **Windows coverage gap.** The `O_EXCL` protection works on Windows
   (`create_new` is portable) but the symlink repro can't run there. Options:
   accept (current), or add a Windows-only variant using junctions. The
   behavior under test is one std call; accept is reasonable.
