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

Canvas only, no UI library. Intel Iris Xe, Vulkan backend. Frame time is CPU
submit plus a hard `device.poll(Wait)`, which serialises CPU and GPU and is
pessimistic against a pipelined app.

`./scripts/bench.ps1` reproduces these. Expect roughly 2x run-to-run variance on
a shared iGPU; read p99 and the over-budget count rather than the mean.

| Scenario | Peak visible | Mean | p99 | Max | Over budget | Placeholders |
|---|---|---|---|---|---|---|
| 2,000 items / 2,000 distinct | 374 | 0.4ms | 0.7-0.8ms | 1.2ms | 0/200 | 0.00% |
| 8,000 items / 8,000 distinct | 1,558 | 0.9-1.6ms | 1.7-4.7ms | 2.9-4.7ms | 0/200 | 0.00% |
| 20,000 items / 20,000 distinct | 3,927 | 3.8-4.6ms | 7.4-7.9ms | 9.4ms | 0/200 | 0.00% |
| 32,000 items / 32,000 distinct | 6,270 | 10.7ms | 22.2ms | 23.4ms | 22/200 | 0.00% |

The whole board is one draw call with no batching logic, so draw-call overhead is
not a factor at any item count.

### The scene is too sparse to answer SQU-60

**Read the visible column before believing any of these numbers.** Only about 5%
of items are on screen: 374 of 2,000, 3,927 of 20,000. `Config::extent` is
12,000, so items are scattered over a 24,000 x 24,000 world area, while at
`start_scale` 0.35 a 2560x1440 viewport covers only 7,314 x 4,114 of it. The
measured area ratio, 5.2%, matches the observed visible fraction exactly.

That means the headline case in SQU-60 — "2,000 items, all visible" — is **not
currently being measured**. What is measured is a 374-item board. The
correspondingly reassuring frame times say very little about a board that is
actually full.

An earlier revision of this document reported 1,194 visible of 2,000 and 20,000
items at 32-42ms and over budget. Those figures are not reproducible from the
current code and have been removed rather than adjusted. Re-tuning `extent` to
make the board dense is a change to the measured configuration and belongs with
SQU-60, not in a cleanup commit; until it happens, treat every row above as
"a sparse board" rather than "a moodboard".

### The CPU/GPU split is no longer measured

This document previously claimed the workload was "CPU-bound at 85-90%"
throughout. The refactor into `canvas-app` kept a single `FrameStats::cpu`
timing and dropped the separate GPU timing, so that claim can no longer be
substantiated and has been removed. `bench.ps1` no longer prints a `CpuMs`
column for the same reason. Restoring the split needs a timestamp-query or
buffer-readback path, and nothing currently depends on it.

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
`VK_KHR_swapchain` is present, and wgpu logs no error — and the window client
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
1,024 at 64px. So a board of a few hundred distinct images at a sharp zoom is
already near the limit, and pushing past it means either a larger atlas, coarser
rendering when many items are visible, or abandoning the atlas for per-item
textures. SQU-65 should own this decision, since it is the same question as how
large thumbnails are on disk.

## Not representative of production

Mip levels are generated on the CPU here. Production decodes once at ingest and
reads them back from the thumbnail cache (SQU-65). Atlas residency, eviction and
upload behaviour under pan and zoom is identical either way, and that is what
this measures. Text is not drawn on the canvas yet; SQU-60 requires text and
strokes composited in a stable pass, and `cosmic-text` plus `glyphon` is the
intended stack.

## Still to do

- Re-tune `Config::extent` so the board is dense enough to test the "2,000
  items, all visible" case, then re-measure. Everything in **Measured** is
  provisional until that happens.
- Restore a GPU-vs-CPU split if any decision depends on which side is the
  bottleneck.
- `host-iced`: written, shares one wgpu device with the canvas, and has never
  rendered. Assess it as a product shell, not as a frame-time benchmark.
- Zed's UI toolkit (`wgpu`, formerly `gpui`): establish whether it is dependable
  as an external dependency before anything else. The pinned wgpu version
  matters more than anything else about it.
- Choose between them, and record the reasoning.
- Compile the renderer to `wasm32` and measure it in a browser. Nothing has ever
  done this and the whole plan rests on it working.
- Design the canvas/host seam once, so both implementations share a definition.
- Canvas text via `cosmic-text` + `glyphon`. Anything drawn in the renderer is
  shared between desktop and web, and the chrome is not, so this is the
  highest-leverage piece of the renderer rather than a later polish item.
- Decide atlas capacity policy, or move off the atlas. The web has a smaller
  texture budget than the desktop, so there are two answers, not one.
