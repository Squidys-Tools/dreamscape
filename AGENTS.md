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
* Vite is the bundler for the hosted web app, driven by bun.
* The `wasm32-unknown-unknown` target is already installed. No `rustup target
  add` needed.

Two lockfiles are expected and are not a problem: `Cargo.lock` and
`web/bun.lock`. Cargo and bun do not share a root manifest, so build ordering is
orchestrated explicitly (see `justfile`).

## Layout

```
crates/
  canvas-core/    culling, LOD, spatial index, item model. No GPU, no platform deps.
  canvas-gpu/     the wgpu renderer
  canvas-app/     scene, camera, selection, search, the per-frame pipeline
  app-core/       storage, ingest, embedding orchestration
  canvas-wasm/    thin wasm-bindgen surface over canvas-app
  host-desktop/   the native shell
web/
  hosted/         the hosted tier's chrome, TypeScript
```

**Keep `wasm-bindgen`, `js-sys` and `web-sys` out of `canvas-app`.** The Rust side
has three consumers: the native desktop app, WASM in a browser, and a server for
the paid tier's inference. Platform-specific dependencies in a shared crate
erode that layering, so the web surface lives in its own thin crate.

## Commands

```sh
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
cargo test -p canvas-core
.\scripts\bench.ps1                     # 2k / 8k / 20k / 32k scenarios
.\scripts\bench.ps1 -Scenario quick     # one fast scenario
just check                              # everything above plus tsc
```

Benchmarks print a `RESULT key=value ...` line. `scripts/bench.ps1` parses it and
**throws on any missing key**, because a missing PowerShell key silently becomes
`0` and a broken run looks like a fast one. Do not loosen that.

## Conventions

* **Comments explain why, not what.** The existing code carries a reasoning
  comment wherever a choice looks odd, especially where a finding forced the
  design. Match that density or raise it. A comment restating the line below it
  is noise.
* Never commit generated output. `canvas-wasm/pkg/` is produced by
  `wasm-pack build` and is build output, not source.
* Types crossing the Rust/TypeScript seam come from `wasm-bindgen`. Never
  hand-declare a shape in TypeScript that Rust also defines; that is how the two
  sides silently diverge.
* Measurement claims belong in `docs/spikes/canvas-spike.md` with the command
  that produced them. If a number cannot be reproduced, remove it rather than
  adjusting it.
