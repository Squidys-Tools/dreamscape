# Canvas architecture spike

The internals document for Dreamscape's canvas renderer: the design, the
findings that forced it, and every measurement with the command that produced
it.

Two questions live here. Does a 2,000-item board pan and zoom at 60fps, and what
does the canvas need from whatever hosts it. The second one is now settled, and
the first has to survive it.

## What the canvas is, and what hosts it

The renderer is one Rust codebase. It compiles natively for a native desktop
shell and to `wasm32` for a browser, so the desktop app and the hosted web tier
share it instead of each owning a copy. A web view cannot share a wgpu device
with the canvas, because the view owns its own GPU context, which is why the
desktop shell is native and the hosted tier is a web page with its own chrome.

What a host has to provide is small: a device, a queue, a target texture view,
and somewhere to send pointer, wheel and keyboard events. The canvas renders
into its own texture and the host blits it, so nothing crosses the CPU. That
interface gets written twice and should be designed once.

Which toolkit draws the chrome on the desktop is still open, and it is a
dependency question rather than a performance one. The renderer is identical
whichever one wins and the chrome is a fraction of the pixels, so a frame-time
comparison between two toolkits would be optimising the part that is already
settled.

Crates and their boundaries are in `AGENTS.md`. Everything below is measurement
or reasoning.

## Decision rule, fixed before running

1. 2,000 items, all visible, at 2560×1440, pan and zoom continuously.
2. Sidebar with a virtualised list of 2,000 rows, live selection, and an
   inspector that updates as you pan. Idle canvas is not a result.
3. A text input that accepts IME composition.
4. Zero frames over 16.67ms, measured as CPU submit **plus** a hard
   `device.poll(Wait)`. That serialises CPU and GPU and is pessimistic against a
   pipelined app, which is the correct direction to be wrong in.
5. Eviction must never remove a texture that is on screen this frame. A board
   showing grey placeholders means the atlas is undersized, not that eviction is
   broken. Those are different failures and the harness reports them separately.

If more than one approach passes, the tiebreak is the one that needs no fork.

## Measured

Canvas only, no UI library. Intel Iris Xe, Vulkan backend.

`./scripts/bench.ps1` reproduces the numbers below. Expect roughly 2x run-to-run
variance on a shared integrated GPU; read p99 and the over-budget count rather
than the mean.

Every scenario below is a **full board**: all items on screen at once. That is
the case SQU-60 specifies, and until recently it was not what was being measured.

### Every frame time before this commit was measured on a blank canvas

The vertex shader built its clip-space position from device pixels directly:

```wgsl
out.pos = vec4<f32>(rect.xy + c * rect.zw, 0.0, 1.0);
```

Clip space is -1..1. A 2,560-pixel-wide board put every quad at clip coordinates
of 0..2,560, so **every quad was clipped away and nothing was ever rasterised.**
The clear colour was the entire frame, on every target, for the life of the
spike.

Nothing in the harness could have caught it, because nothing in the harness
looked at a pixel. Culling, LOD, atlas residency, eviction, the `RESULT` line and
all three frame-time columns are computed entirely on the CPU. A board of 2,000
items that renders nothing costs the CPU work of deciding what to draw 2,000
times, which is why the numbers looked comfortable.

It was found in the browser, by `canvas-wasm`'s `verify_pixels`, which reads the
render target back off the GPU and reports the colour histogram. The first
browser frame came back `distinct=1`: one colour, 100,800 samples, no structure.
The fix is a projection uniform carrying the target size, so the pixel-to-clip
conversion happens in the vertex shader where the rest of the transform lives:

```wgsl
let px = rect.xy + c * rect.zw;
out.pos = vec4<f32>(px.x / view.size.x * 2.0 - 1.0, 1.0 - px.y / view.size.y * 2.0, 0.0, 1.0);
```

A second bug came out of the same investigation. `AppState::new` built itself
with `motion: Motion::default()` and never read `cfg.motion`, so **every
`--pan` and `--zoom` flag a runner passed was accepted and discarded.** The
`scale` scenario asked for a still board with `--pan=0` and got a panning one,
under a row label reading "all on screen". The rows were still full boards,
because density comes from `Config::extent` rather than from motion, so the
`Degraded` column below survives; the scenario *names* did not.

**So: the frame times in this document are void, and the table that follows is
the first one measured on a renderer that draws.** What replaces them was
captured on a machine running an agent session, at 45-61% CPU, so it is a sample
under load and not a result. It is in the table because a lower bound nobody can
reproduce is what got us here.

### Frame times, first measurement on a renderer that draws

`.\scripts\bench.ps1 -Scenario scale -ResultLog bench-after-renderer-fix.txt`,
captured at 46% CPU before and 51% after. Atlas 2,048, 2560x1440, 150 frames,
everything on screen.

| Items / distinct | Peak visible | Mean | p99 | Max | Over budget | Degraded |
|---|---|---|---|---|---|---|
| 2,000 | 2,000 | 6.00ms | 18.00ms | 20.63ms | 2/150 | 71.97% |
| 4,000 | 4,000 | 6.74ms | 11.05ms | 15.70ms | 0/150 | 85.77% |
| 8,000 | 8,000 | 13.41ms | 25.96ms | 27.49ms | 16/150 | 92.88% |
| 16,000 | 16,000 | 30.51ms | 40.91ms | 42.16ms | 150/150 | 96.44% |
| 32,000 | 32,000 | 71.59ms | 121.60ms | 183.27ms | 150/150 | 98.22% |

The wall is between 8,000 and 16,000 items with everything on screen, and the
curve is now superlinear in a way the old table could never have shown: fill rate
was never being paid. The 2,000-item row going 2/150 while the 4,000-item row
goes 0/150 is the loaded-machine noise described below, not a real inversion, and
it is the reason these numbers need re-taking on an idle machine before anyone
quotes them.

The 2,000-item case SQU-60 asks about, measured with the panning motion the
default scenario uses rather than a still board, is 3.95ms mean / 5.87ms p99 /
0/200 over budget at 45-61% CPU. That is the figure that carries the most weight
and the least confidence.

The whole board is one draw call with no batching logic, so draw-call overhead is
not a factor at any item count. What changed is everything behind the draw call:
the GPU is now actually shading the pixels.

### Why no single number here is trustworthy yet

Re-measuring on an idle machine has not happened, and it is still the first thing
to do. Four runs of the identical 2,000-item scenario, same commit, same
command, at 47-71% CPU load on a machine where a browser and two GPU apps share
the integrated GPU:

| Run | Mean | p99 | Over budget |
|---|---|---|---|
| 1 | 3.97ms | 7.20ms | 0/200 |
| 2 | 4.62ms | 10.16ms | 1/200 |
| 3 | 6.92ms | 24.24ms | 6/200 |
| 4 | 3.97ms | 7.22ms | 0/200 |

Those four runs predate the renderer fix, so they are void for a second reason
now. They are kept because they are still the best available evidence about
run-to-run variance on a loaded machine, which has not been re-measured since.
The p99 moves by 3x and the over-budget count by 6 frames across runs that differ
only in what else the machine was doing. The documented 2x run-to-run variance is
an underestimate under load. T3 Code is itself a consumer, so an agent session
cannot produce a clean run while it is driving one.

Degradation is a count of item-frames rather than a timing, and it is stable
under load in a way timings are not: 74.21% came back identically across all four
of those runs and again after the renderer fix.

To close this, on an idle machine, capture rather than read off the screen:

```
.\scripts\bench.ps1 -Scenario bench -ResultLog bench.txt
.\scripts\bench.ps1 -Scenario scale -ResultLog bench.txt
```

The log brackets the run with the CPU load, so a figure that survives into the
docs carries the conditions it was taken under. The console table is a
`Format-Table` object and does not survive redirection as text, which is why the
`RESULT` line is the thing to capture.

### Mip degradation is the real constraint

Getting the board dense is what exposed this.

`Config::extent` was 12,000, which scattered items across a 24,000-square world
area while the viewport covers about 7,300 of it. Only 5% of items were ever on
screen, and the 2,000-item scenario rendered 374 while reporting a healthy 0.4ms.
The number was real and the claim was worthless. It is now 1,000, which puts every
item in view.

That fix immediately produced a second finding. **A board full of 2,000 distinct
images reports 0% placeholders, and on its own that says almost nothing.** The
atlas is partitioned by mip level, so a 2,048 atlas holds only 256 slots at
128px. The rest fall back to 64px and then 32px, which is exactly the degradation
the design intends, and the placeholder count stays at zero because nothing ever
fails outright.

The harness now counts **degraded** separately from placeholders: items drawn at a
mip level coarser than their on-screen size asked for. That number is what makes
the atlas legible, and the earlier claim that 2,048 and 8,192 atlases were
indistinguishable is what the metric was missing.

| Atlas | Peak visible | Placeholders | Degraded |
|---|---|---|---|
| 2,048 | 2,000 | 0.00% | **74.21%** |
| 8,192 | 2,000 | 0.00% | **0.00%** |

```
.\scripts\bench.ps1 -Scenario quick              # atlas 2048
.\scripts\bench.ps1 -Scenario quick -Atlas 8192  # atlas 8192
```

Same board, same seed, same budget. Placeholders are identical at 0.00% in both,
which is the whole point: the column that used to be the only one available could
not tell these two apart, and now it can. **Roughly three quarters of a
2,000-image board was being drawn at a mip level well below what its on-screen
size warranted, and the default atlas is the reason.**

The arithmetic behind 74% is unforgiving and not fixable by a larger square
alone. 2,000 items want roughly 2,000 x 128px, or 33M pixels, against 4.2M in a
2,048 atlas. It cannot fit. 8,192 has the 67M to hold them, which is why it
reaches 0.00% and not merely a lower number.

The implication for the design is that **atlas capacity, not frame time, is what
bounds a dense board.** The frame-time wall is somewhere between 2,000 and 32,000
visible items and needs re-measuring; the quality wall is 2,000 items at a 2,048
atlas, and it is crossed today. A moodboard of a few hundred references is fine.
A few thousand references need a bigger atlas or a coarser rendering policy, and
which one is the same question as how large thumbnails are on disk. That is
SQU-65, and it now has the number it needs.

### The CPU/GPU split is no longer measured

This document previously claimed the workload was "CPU-bound at 85-90%"
throughout. The refactor into `canvas-app` kept a single CPU timing and dropped
the separate GPU timing, so that claim can no longer be substantiated and has
been removed. `bench.ps1` no longer prints a `CpuMs` column for the same reason.

`FrameStats` now carries no clock at all: the renderer cannot call
`Instant::now()` on `wasm32-unknown-unknown`, and the host is the only layer that
knows what a frame is on its platform. The harness's number is the whole frame
including a hard GPU wait, which is the sum, not a split. Restoring the split
needs a timestamp-query or buffer-readback path, and nothing currently depends on
it.

## Windowed presentation does not work on the dev machine

Every windowed host built here opens a real window and never draws a pixel. This
is not a bug in the canvas code, and it is not specific to iced.

`crates/probe-surface` is the control: raw winit 0.30 and raw wgpu 27, no UI
framework, clearing every frame to solid magenta. It reports

```
adapter: Intel(R) Iris(R) Xe Graphics / Vulkan
presented frame 1 / 2 / 3 / 60 / 120 / 180
```

`present()` returns `Ok` on every frame, the surface configures cleanly,
`VK_KHR_swapchain` is present, and wgpu logs no error - and the window client
area stays white. Confirmed while the window was explicitly foregrounded, and
confirmed again in a full-desktop capture where other GPU-composited windows
render normally, so it is not a screenshot artefact. Headless wgpu renders
correctly on the same machine, so the GPU and the wgpu build are fine.

Three hosts were tried and all blank: iced 0.14, iced 0.13.1, and the raw probe.
Process inspection during the iced hang showed 27 threads all in wait, one in
`LpcReply`, which is suggestive of a kernel or driver call but is not proof.

The consequence for this spike: **no wgpu host can be visually verified or timed
on this machine.** Headless numbers are trustworthy; anything requiring a window
is blocked. Tracked as SQU-73. One loose end there: `WGPU_BACKEND=dx12` is
ignored by wgpu 27 here, which still selected Vulkan, so DX12 is genuinely
untested and needs the backend hard-coded rather than set by environment.

## Findings that changed the design

**The WebGL2 floor rules out storage buffers.** Committing to WebGL2 as the
graphics floor means `max_storage_buffers_per_shader_stage = 0`, so per-instance
data cannot be fed through a storage buffer in the vertex stage. It is fed
through an instanced vertex buffer instead, which is more portable anyway. This
surfaced as a validation panic on the first run.

**Mip levels must be allocated lazily, one at a time.** The first version
allocated a whole pyramid the moment a texture became visible and rejected the
texture outright if any level was oversubscribed. On a board of distinct images
that produced **95% grey placeholders** on a 2,000-image board, because a 2,048
atlas holds only 64 slots at 256px while holding 262,144 at 4px. Allocating
per level, and falling back to a coarser level rather than failing, took that to
**0%**. This is the single most important finding in the spike and it was only
visible because the content is distinct images rather than a handful reused.

Note the earlier version of this document reported 0.83% placeholders. That figure
came from a scene reusing 64 images across 2,000 items, which is not what a
moodboard looks like. The realistic figure was 95% and the test was wrong.

**Retrying a failed allocation is far more expensive than the upload.** After
lazy allocation, an item that cannot get its preferred level failed every frame,
re-uploading and re-scanning the eviction ring. Callers must check whether *some*
usable level is already resident before requesting, so a request resolves once
and sticks. Before this fix, 32,000 distinct images cost 830ms per frame; after
it, the same scene renders with zero placeholders.

**Eviction must not scan.** Finding a victim with `min_by_key` over the residency
map is a full O(n) scan with no early exit, so a board of many distinct images
spends the frame inside eviction. A rotating cursor over an append-only key list
finds an unpinned victim in a bounded number of steps. The cursor still refuses
to evict anything pinned this frame, and refusing to thrash is correct: the
caller degrades to a coarser level or a placeholder.

**Pin before ingest, never after.** Ingesting one newly visible texture can evict
another that is already on screen but has not been visited by the loop yet. The
symptom is a hole in the board. Fixed by pinning the visible set first.

**Placeholders are required, and are nearly free.** When no level can be placed
the item draws as a flat swatch rather than vanishing. Implemented as a
zero-area uv rect aimed at one reserved texel, so it needs no shader branch and
costs the same as a real item. This is the "minimum-size placeholders" clause in
SQU-60 and it is load-bearing whenever the atlas is undersized.

## Atlas capacity is the real constraint

A 2,048 square atlas holds 4.2M pixels, which is about 256 images at 128px or
1,024 at 64px. The atlas is partitioned by mip level, so those budgets do not
pool: overflow at one level falls back to the next rather than borrowing space.

That makes the arithmetic unforgiving at full density. A board of 2,000 items
wants roughly 2,000 x 128px, or 33M pixels, against an atlas of 4.2M. It cannot
fit, and the fallback is the designed behaviour, so the board renders rather
than failing. Measured, that fallback is **74.21% of item-frames** at 2,048 and
**0.00%** at 8,192. See *Mip degradation is the real constraint* for the runs.

Pushing past this means a larger atlas, coarser rendering when many items are
visible, or abandoning the atlas for per-item textures. SQU-65 should own that
decision, since it is the same question as how large thumbnails are on disk.

## Not representative of production

Mip levels are generated on the CPU here. Production decodes once at ingest and
reads them back from the thumbnail cache (SQU-65). Atlas residency, eviction and
upload behaviour under pan and zoom is identical either way, and that is what
this measures. Text is not drawn on the canvas yet; SQU-60 requires text and
strokes composited in a stable pass, and `cosmic-text` plus `glyphon` is the
intended stack.

## Still to do

- **Re-measure the frame-time table on an idle machine.** The published table is
  void: it was measured on a renderer that rasterised nothing, so every figure in
  it is a lower bound. A first sample on a fixed renderer is in *Frame times,
  first measurement on a renderer that draws*, and it was taken at 45-61% CPU
  with an agent session driving it. Four attempts on a loaded machine ranged
  3.97-6.92ms mean and 7.20-24.24ms p99 on the same scenario, so this cannot be
  closed from a busy desktop. First item because every other performance claim
  depends on it, and it is now also the only measurement of the real thing.
- **Run the WebGL2 fallback and report a frame.** It compiles and the seam is
  unchanged by construction, which is not the same as working. Detail in *In a
  browser*.
- **Confirm the browser's colours.** No sRGB surface format was available on the
  machine that ran the spike, so the output may be gamma-crushed and nobody has
  looked at a real image. Also in *In a browser*.
- Restore a GPU-vs-CPU split if any decision depends on which side is the
  bottleneck. More pressing than it was: the wall is now superlinear, which is
  what fill-rate pressure looks like, and that is a GPU-side cause.
- `host-iced`: written, shares one wgpu device with the canvas, and has never
  rendered. Assess it as a product shell, not as a frame-time benchmark. Its
  canvas now rasterises, so if it is still blank that is SQU-73 and not the
  shader.
- Zed's UI toolkit (`wgpu`, formerly `gpui`): establish whether it is dependable
  as an external dependency before anything else. The pinned wgpu version
  matters more than anything else about it.
- Choose between them, and record the reasoning.
- Canvas text via `cosmic-text` + `glyphon`. Anything drawn in the renderer is
  shared between desktop and web, and the chrome is not, so this is the
  highest-leverage piece of the renderer rather than a later polish item.
- Decide atlas capacity policy, or move off the atlas. The measured number now
  exists: 74.21% of item-frames degrade at 2,048 for a 2,000-item board, rising
  to 98.22% at 32,000. The web has a smaller texture budget than the desktop, so
  there are two answers, not one.

## In a browser

SQU-83. The renderer compiles to `wasm32-unknown-unknown` and runs under
`canvas-wasm`, which is the browser implementation of the seam. What that
settled, and what it did not.

### It works, and it is the same renderer

`canvas-core`, `canvas-gpu` and `canvas-app` build for `wasm32-unknown-unknown`
with nothing windowing-related in the graph. `pollster` is a
`cfg(not(target_arch = "wasm32"))` dependency, because `headless_device` blocks on
a future and a wasm module has no second thread to wake.

The strongest evidence that this is the same renderer and not a second one
dressed up as the first: the browser reported **74.21% mip degradation at a
2,048 atlas for a 2,000-item board**, which is the native figure to two decimal
places, and the same 2,000 peak-visible count. Those are computed on the CPU from
atlas residency, so an identical value means an identical pipeline.

### The seam

Six things, all defined in `canvas-app` and `canvas-core` and none of them in a
host: the device, the queue and the clock; a target view; pointer and wheel
events; one `draw_frame` per frame; and the metrics. There is deliberately no
trait for it, because a host's entire contribution is `get_current_texture`, a
clock and an event forwarder, and a trait over three lines of behaviour is a
name rather than an abstraction. What stops two hosts drifting is that the
RESULT line is formatted in one place, `Metrics::result_line`, so the browser
and the harness cannot spell a number differently.

Keyboard is **not** in the seam yet, against the original list. Nothing in the
canvas consumes a key, and an event type with no handler on the other end is a
promise rather than an interface. It joins when something consumes it.

The camera maths that pan and zoom need moved into `canvas-core` as
`CameraControl`, beside `Viewport`, with four tests. Both hosts now drag the same
way, and a drag pins the grabbed world point to the cursor rather than
accumulating deltas, so a zoom mid-drag does not make the item creep out from
under it.

### Two numbers, one of them decided

`.wasm` after `wasm-pack build --release` and `wasm-opt`: **252,395 bytes.** A
local download for the desktop app, a network download for the hosted tier, so
it is a number worth having before the ingest design gets written.

Frame times, same scene and same definition as the harness. `draw_frame` plus the
queue reporting all submitted work done is the browser's `device.poll(Wait)`:

| Scenario | Peak visible | Mean | p99 | Over budget | Degraded |
|---|---|---|---|---|---|
| 2,000 / 2,000 distinct, 2560x1440 | 2,000 | 9.93ms | 16.50ms | 2/200 | 74.21% |

One run, on a machine running the agent session that took it, so it is a sample
and not a result. It is roughly 2.5x the native mean for the same scene, which
is the shape of the answer the issue wanted: the boundary costs something real,
and it is not catastrophic. The native side of that comparison needs an
idle-machine run before the ratio means anything.

One asymmetry is baked in and is not a bug: the native hosts render into an
offscreen texture and blit it, because their toolkit owns the surface. The canvas
context *is* the target here, so the browser figure excludes one fullscreen blit
the desktop pays. Read it as a lower bound on the desktop host.

### What is not settled

**The WebGL2 fallback is compiled but unverified.** It is a feature flag rather
than a second renderer, which is the right shape: `--features webgl` compiles
`canvas-wasm` against wgpu's GL backend and the seam is untouched, because the
backend is chosen in one `InstanceDescriptor`. That is a compile-time claim, not
a measurement. Nobody has run it and reported a frame.

**Chromium reports no adapter identity.** `adapter.get_info()` comes back with an
empty name and zero vendor and device IDs on this machine, so a browser figure
cannot name its GPU. `describe` prints what is available and says so explicitly
when there is nothing, rather than emitting a bare slash. This is a real
conditions problem and it is worse than the native path's, not better.

**Colour has not been confirmed end to end.** The surface reports no sRGB format
on this machine, so the target is `Bgra8Unorm` and the renderer's linear output is
stored without an encode. The board rasterises and has real dynamic range
(`distinct=8,287` colours, 28% of sampled pixels off the clear colour), but
whether it is *correct* is unverified, and the honest next step is a
max-pooled luminance readback rather than another guess. Native hosts pass
`Rgba8UnormSrgb` explicitly and are unaffected.

**A headless browser cannot confirm presentation.** The page screenshot came back
blank for the canvas and `createImageBitmap` on it read an empty layer, while a
2D-canvas control read back correctly through the same code. A frame that was
submitted and never painted, and a frame that was painted and never composited,
look identical from outside and mean opposite things. GPU readback distinguishes
the renderer; only a headed browser can settle the compositor.

### The toolchain is not what this document claimed

`AGENTS.md` said `wasm32-unknown-unknown` was installed with no
`rustup target add` needed. It was listed by `rustup target list --installed` and
the target's lib directory was **empty**, so every cross-target build failed with
`can't find crate for core` on a target nobody had asked about. `rust-toolchain.toml`
now declares the target, which makes `rustup` install it on the clone that needs
it.

Separately, this machine has two Rust installations and `C:\Program Files\Rust
stable MSVC 1.98\bin` precedes the rustup shim on `PATH`. Cargo resolves `rustc`
from `PATH`, so cross-target builds silently used the 1.98 toolchain, whose
sysroot has no wasm32 std, and native builds used 1.98 while the repo documents a
1.92 floor. Prepending the rustup toolchain's `bin` fixes it. A fresh clone on a
machine with only rustup is unaffected, which is why this is a note rather than a
committed workaround.

