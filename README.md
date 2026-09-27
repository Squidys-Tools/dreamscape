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

| Scenario | Peak visible | Mean | p99 | Max | Over budget | Placeholders |
|---|---|---|---|---|---|---|
| 2,000 items, 2,000 distinct | 374 | 0.4ms | 0.7-0.8ms | 1.2ms | 0 / 200 | 0.00% |
| 8,000 items, 8,000 distinct | 1,558 | 0.9-1.6ms | 1.7-4.7ms | 2.9-4.7ms | 0 / 200 | 0.00% |
| 20,000 items, 20,000 distinct | 3,927 | 3.8-4.6ms | 7.4-7.9ms | 9.4ms | 0 / 200 | 0.00% |
| 32,000 items, 32,000 distinct | 6,270 | 10.7ms | 22.2ms | 23.4ms | 22 / 200 | 0.00% |

**Only about 5% of items are on screen in these runs** — 374 of 2,000, 3,927 of
20,000 — because `Config::extent` scatters items over a 24,000-square world area
while the viewport covers about 7,300 of it. The case SQU-60 actually asks about,
2,000 items all visible, is **not** what this measures. The frame times above are
for a sparse board and should not be read as evidence about a full one. See
`docs/spikes/canvas-spike.md`.

The whole board is one draw call, so draw-call overhead is not a factor at any
item count.

## Windowed rendering is blocked on this machine

`host-iced` builds and opens a window that never draws, and so does a raw
winit + wgpu control that clears every frame to magenta: 180+ frames presented,
`present()` returns `Ok`, no wgpu errors, client area stays white. Headless wgpu
renders fine. So the headless numbers above are trustworthy and anything needing
a window is currently blocked. Tracked as SQU-73.

## Layout

| Crate | What it is |
|---|---|
| `canvas-core` | Item model, viewport, spatial hash, culling, LOD. No GPU. 10 unit tests. |
| `canvas-gpu` | wgpu renderer. Texture atlas with lazy per-level allocation and eviction that never drops something on screen. One instanced draw call. |
| `canvas-app` | Scene, camera, selection and the per-frame pipeline. Shared by every host so their frame times are comparable. |
| `canvas-harness` | Headless driver over `canvas-app`, plus frame metrics. No asset files. |
| `host-iced` | iced 0.14 host, sharing one wgpu device with the canvas. Written; blocked on SQU-73. |
| `probe-surface` | Throwaway raw winit + wgpu control for the presentation bug. |

`host-wgpui` and `host-gpui` do not exist yet.

See `docs/spikes/canvas-spike.md` for the design, the decision rule for
choosing a UI library, and what is still outstanding.

## What is not measured

Real photographs, file decoding, text, drawing tools, selection, grouping, any
sidebar, any UI library, and anything requiring a visible window. A dense board,
where all 2,000 items are on screen at once. This measures whether the canvas
renderer can draw a sparse board quickly, which is a necessary condition for the
architecture but not a sufficient one.

## Licence

MIT OR Apache-2.0
