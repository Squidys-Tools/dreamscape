# Working in this repository

## What this is

The canvas architecture spike for Dreamscape: a wgpu renderer for an infinite
canvas, a headless benchmark, and UI host experiments. Read
`docs/spikes/canvas-spike.md` before changing anything in the renderer. It
records findings that are not obvious from the code and will bite anyone who
rediscover them.

Architecture decisions are **not** recorded here. They live in Linear, project
Dreamscape, as `decision` issues, so they can be argued and revised. This file
records only how to build and what the conventions are.

## Toolchain

* Rust 1.92. `rust-version` is pinned in the workspace `Cargo.toml`.
* **bun for all JavaScript work.** Not npm, not yarn, not pnpm. `bun install`,
  `bun run`, `bun test`. bun 1.4.2 is on PATH.
* Vite is the bundler for the hosted web app, driven by bun. Not built yet.
* The `wasm32-unknown-unknown` target is already installed. No `rustup target
  add` needed.
* `just` is **not** installed, so nothing may invoke it. `t3.json` at the repo
  root is where project commands are registered, and that is what the T3 Code
  scripts menu runs.

Two lockfiles are expected once `web/` exists: `Cargo.lock` and
`web/bun.lock`. Cargo and bun share no root manifest, so build order is
orchestrated explicitly in `t3.json`.

## Layout, as it exists today

```
crates/
  canvas-core/     culling, LOD, spatial index, item model. No GPU, no platform deps.
  canvas-gpu/      the wgpu renderer
  canvas-app/      scene, camera, selection, search, the per-frame pipeline
  canvas-harness/  headless driver over canvas-app, plus frame metrics
  host-iced/       iced 0.14 host sharing one device with the canvas
  probe-surface/   throwaway raw winit + wgpu control for SQU-73
scripts/
  bench.ps1
docs/
  spikes/canvas-spike.md
```

## Layout, as planned

These do not exist yet. Tracked in Linear; listed here so the shape is agreed
before anyone starts building.

* `crates/canvas-wasm/` — thin wasm-bindgen surface over `canvas-app`
* `crates/app-core/` — storage, ingest, embedding orchestration
* `crates/host-desktop/` — the chosen native shell, superseding `host-iced`
* `web/hosted/` — the hosted tier's chrome, TypeScript

**Keep `wasm-bindgen`, `js-sys` and `web-sys` out of `canvas-app`.** The Rust
side will have three consumers: the native desktop app, WASM in a browser, and a
server for the paid tier's inference. Platform-specific dependencies in a shared
crate erode that layering, so the web surface belongs in its own thin crate.

## Commands

Verified working. The T3 Code scripts menu runs the same list.

```sh
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
cargo fmt --all -- --check
cargo test -p canvas-core                     # 10 unit tests, no GPU needed

.\scripts\bench.ps1                           # 2k / 8k / 20k
.\scripts\bench.ps1 -Scenario quick            # one fast scenario
.\scripts\bench.ps1 -Scenario scale            # counts doubled, all on screen
cargo run -p canvas-harness --bin bench --release -- --items=8000 --textures=8000
```

`cargo clippy` currently emits one warning in `canvas-gpu` about indexing
`per_row`. It is pre-existing and deliberately left alone, so a clean run means
"one warning", not "no warnings".

Benchmarks print a `RESULT key=value ...` line. `scripts/bench.ps1` parses it and
**throws on any missing key**, because a missing PowerShell key silently becomes
`0` and a broken run looks like a fast one. Do not loosen that.

## Known-broken

Do not use these to conclude anything, they are documented so nobody re-derives
the failure.

* **`cargo run -p host-iced` opens a window and never draws.** Tracked as
  SQU-73. It is a machine-level presentation failure, not a bug in the host: a
  raw winit + wgpu probe presents 180+ frames with `present()` returning `Ok` and
  the client area still shows nothing.
* **The benchmark board is too sparse to test the case SQU-60 specifies.** Only
  about 5% of items are on screen, so the 2,000-item scenario renders 374. Fixed
  by SQU-84; until then read the visible column before believing any frame time.

## Conventions

* **Comments explain why, not what.** The existing code carries a reasoning
  comment wherever a choice looks odd, especially where a finding forced the
  design. Match that density or raise it. A comment restating the line below it
  is noise.
* Never commit generated output. `canvas-wasm/pkg/` will be produced by
  `wasm-pack build` and is build output, not source.
* Types crossing the Rust/TypeScript seam come from `wasm-bindgen`. Never
  hand-declare a shape in TypeScript that Rust also defines; that is how the two
  sides silently diverge.
* Measurement claims belong in `docs/spikes/canvas-spike.md` with the command
  that produced them. If a number cannot be reproduced, remove it rather than
  adjusting it.
* **This file describes the repo as it is.** Do not write down a crate, path or
  command that does not exist yet. Planned structure goes in Linear, and at most
  a short "as planned" list here.
