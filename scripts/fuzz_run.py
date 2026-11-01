#!/usr/bin/env python3
"""Run the razorpdf CLI over a mutant corpus, classifying outcomes.

Usage: python3 scripts/fuzz_run.py [corpus_dir] [timeout_secs]
Prints a summary; writes findings.json with every non-clean result.
"""

import json
import os
import subprocess
import sys
import time

BIN = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "../target/release/razorpdf")


def run_one(path, timeout):
    t0 = time.time()
    try:
        p = subprocess.run(
            [BIN, "parse", path, "-o", "/dev/null", "--mock-vlm"],
            capture_output=True, timeout=timeout)
        dt = time.time() - t0
        err = p.stderr.decode("utf-8", "replace")
        if p.returncode in (-6, -11) or "panicked" in err:
            return "PANIC", p.returncode, dt, err[-500:]
        if p.returncode != 0:
            return "ERR", p.returncode, dt, err[-200:]
        return "OK", 0, dt, ""
    except subprocess.TimeoutExpired:
        return "TIMEOUT", 124, timeout, ""


def main():
    corpus = sys.argv[1] if len(sys.argv) > 1 else "/tmp/fuzzcorpus"
    timeout = int(sys.argv[2]) if len(sys.argv) > 2 else 20
    files = sorted(f for f in os.listdir(corpus) if f.endswith(".pdf"))
    counts = {}
    findings = []
    slowest = []
    for i, f in enumerate(files):
        verdict, code, dt, err = run_one(os.path.join(corpus, f), timeout)
        counts[verdict] = counts.get(verdict, 0) + 1
        slowest.append((dt, f))
        if verdict != "OK":
            findings.append({"file": f, "verdict": verdict, "code": code,
                             "secs": round(dt, 2), "stderr": err})
        if (i + 1) % 50 == 0:
            print(f"  {i + 1}/{len(files)} ... {counts}", flush=True)
    print("counts:", counts)
    print("slowest 5:", [(f, round(d, 2)) for d, f in
                         sorted(slowest, reverse=True)[:5]])
    with open("findings.json", "w") as fh:
        json.dump(findings, fh, indent=1)
    print(f"{len(findings)} non-OK findings -> findings.json")


if __name__ == "__main__":
    main()
