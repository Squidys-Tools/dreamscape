# Dreamscape

An infinite visual workspace for collecting and arranging visual references.
Local-first, Windows first, macOS later.

This repository currently contains the **canvas architecture spike** (SQU-60,
SQU-61): a wgpu renderer and measurement harness for an infinite canvas, built
to answer whether a board of references pans and zooms at 60fps, and to compare
three candidate UI libraries as hosts for it.

Nothing here is the product yet. It is the measurement that the product
decisions depend on.

## Running the benchmark

Requires a Rust toolchain (developed against 1.92).

```sh
cargo test -p canvas-core                          # 10 unit tests, no GPU needed
.\scripts\bench.ps1                                # realistic views: 2k / 8k / 20k
.\scripts\bench.ps1 -Scenario scale                # item counts doubled repeatedly
.\scripts\bench.ps1 -Scenario quick                # one fast scenario
```

Or drive the benchmark directly:

```sh
cargo run -p canvas-harness --bin bench --release -- --items=8000 --textures=8000
```

Flags: `--items --textures --atlas --width --height --warmup --frames --pan --zoom`.
Each run prints a human-readable report and a `RESULT key=value ...` line that
scripts parse.

**Expect run-to-run variance of roughly 2x.** The reference machine for these
numbers is an Intel Iris Xe integrated GPU sharing a Windows desktop, which
schedules and thermally throttles unpredictably. Read the p99 and the over-budget
count, not the mean alone.

## Results

Frame time is CPU submit plus a hard `device.poll(Wait)`, which serialises CPU
and GPU. That is pessimistic against a real app that runs the CPU ahead, so
treat it as a ceiling.

| Scenario | Visible | Mean | p99 | Max | Over budget | Placeholders |
|---|---|---|---|---|---|---|
| 2,000 items, 2,000 distinct | 1,194 | 1.6ms | 4-5ms | 6-11ms | 0 / 200 | 0.00% |
| 8,000 items, 8,000 distinct | 4,967 | 6.7-13.3ms | 24-55ms | 29-124ms | 7-54 / 200 | 0.00% |
| 20,000 items, 20,000 distinct | 12,431 | 32-42ms | 121-160ms | 134-287ms | 130-140 / 200 | 0.00% |

The 2,000-item case that SQU-60 specifies passes with roughly 10x headroom. Past
about 5,000 simultaneously visible distinct images it degrades, and past about
12,000 it is over budget.

**It is CPU-bound, not GPU-bound**, at 85-90% CPU share throughout. The cost is
culling, building the per-frame instance buffer, and uploading texture levels.

The whole board is one draw call, so draw-call overhead is not a factor at any
item count.

## Layout

| Crate | What it is |
|---|---|
| `canvas-core` | Item model, viewport, spatial hash, culling, LOD. No GPU. 10 unit tests. |
| `canvas-gpu` | wgpu renderer. Texture atlas with lazy per-level allocation and eviction that never drops something on screen. One instanced draw call. |
| `canvas-harness` | Procedural scene and textures, frame metrics. No asset files. |
| `host-*` | The three UI library hosts. Not built yet. |

See `docs/spikes/canvas-spike.md` for the design, the decision rule for
choosing a UI library, and what is still outstanding.

## What is not measured

Real photographs, file decoding, text, drawing tools, selection, grouping, any
sidebar, any UI library. This measures whether the canvas can draw a busy board
fast enough, which is the assumption the rest of the architecture rests on.

## Licence

MIT OR Apache-2.0
