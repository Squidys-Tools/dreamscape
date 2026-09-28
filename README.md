# Dreamscape

### The canvas architecture spike. Not the product.

Dreamscape is an infinite canvas for collecting and arranging visual references. This repo exists to answer two questions before any of that gets built: does a board of references pan and zoom at 60fps, and which UI library should own the window?

It is a wgpu renderer, a headless benchmark, and a set of experiments whose only job is to produce a number or settle a question. The questions are tracked in Linear; the numbers live in one place.

## What it settles

The canvas renderer. Infinite grid, viewport culling, a fixed-slot texture atlas with lazy per-level mip allocation and eviction that never drops something on screen, and one instanced draw call for the whole board. Text and strokes are not drawn yet.

Settled so far, and load-bearing for everything after it:

* One Rust canvas codebase, compiling natively for a native desktop shell and to `wasm32` for a browser. One renderer, not two.
* The desktop shell is native Rust, not a webview, because a webview cannot share a wgpu device with the canvas.
* The hosted web version is view-and-modify-only. It cannot add anything, which is what keeps its separate UI affordable.
* The browser extension is capture-only, so it carries no canvas at all.

## Status

The renderer works and is measured headlessly. The windowed path does not work on the development machine at all.

**Working:** culling, LOD, the atlas, eviction, search, 10 unit tests over the correctness-critical logic, and a reproducible benchmark.

**Not working:**

* **Nothing draws to a window.** A raw winit plus wgpu probe, no UI framework involved, presents 180+ frames with `present()` returning `Ok` and no wgpu errors, and the client area stays white. Confirmed while foregrounded and in a full-desktop capture where other GPU-composited windows render normally. Headless wgpu is fine, so the renderer is fine; the display path on this machine is not. This is the biggest open problem and it is an environment issue, not a code issue.
* **Nothing measures mip degradation.** A full board of distinct images hits the atlas ceiling by quietly falling back to coarser mip levels. A 2,048 atlas and an 8,192 atlas measure identically, so "0% placeholders" means nothing failed rather than everything looks right.
* **No UI toolkit chosen yet.** iced is written and shares one wgpu device. Zed's toolkit is the other candidate and is a dependency question, not a performance one.

Expect rough edges and missing pieces. This is a spike, and spikes that finish are the exception.

## For developers

Rust 1.92 and, later, bun 1.4.2 for the hosted tier's TypeScript. Windows first.

```powershell
cargo test -p canvas-core                          # 10 unit tests, no GPU needed
.\scripts\bench.ps1                                # the measured scenarios
.\scripts\bench.ps1 -Scenario quick                # one fast scenario, for a tight loop
cargo run -p host-iced --release                   # opens a window, draws nothing (see Status)
```

Expect roughly 2x run-to-run variance on a shared integrated GPU. Read the p99 and the over-budget count, not the mean alone.

The T3 Code scripts menu runs the same commands, registered in `dreamscape.json`.

**Start here:**

- [`AGENTS.md`](AGENTS.md) - what must not be compromised, the glossary, and how to work in this repo
- [`docs/spikes/canvas-spike.md`](docs/spikes/canvas-spike.md) - the design, the findings that forced it, and every measurement with the command that produced it
- [`dreamscape.json`](dreamscape.json) - project commands for the T3 Code scripts menu

There is no `docs/` hierarchy and no changelog yet, on purpose. A spike with one internals document does not need them.

## Licence

MIT OR Apache-2.0
