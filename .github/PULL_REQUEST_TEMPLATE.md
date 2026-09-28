<!--
One issue, one PR. Branch from the issue's gitBranchName and link the issue at
the top of the body.

Scope notes that decide what goes in here rather than in a comment:
- A metric that changes name, unit or meaning moves the FrameStats field, the
  RESULT line, the parser in bench.ps1, the report, and the table in the docs, in
  one commit. A consumer that misses it reads a missing key as zero.
- A figure quoted anywhere comes from a captured run, not from a terminal that
  happened to be open. bench.ps1 -ResultLog writes the RESULT line and brackets
  it with the machine's load.
-->

## What changed

## Why

## How it was verified

<!-- Commands run and what they printed. Say "not run" where that is the case. -->

- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets`, `cargo test -p canvas-core` pass. Clippy is clean at one known warning, the `per_row` one in `canvas-gpu`.
- [ ] GPU or renderer behaviour changed, so the benchmark was run rather than the diff read. Figures are off a captured log.
- [ ] A metric changed, so every consumer of it changed in the same commit.
- [ ] `docs/spikes/canvas-spike.md` and `AGENTS.md` still describe what the code does.
- [ ] A new crate, scenario or bench column has a way out as well as a way in.
