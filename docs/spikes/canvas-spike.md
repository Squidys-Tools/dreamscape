# Canvas architecture spike

Tests the assumption SQU-60 rests on: that a 2,000-item board pans and zooms at
60fps, and that a UI library can host that canvas without a per-frame copy
between the two.

## Why this is three configurations, not two

The canvas renderer is shared. The *host* is not, because the two families of UI
library disagree about what owns the GPU:

| Config | Renderer | Shares one wgpu device with the canvas? |
|---|---|---|
| `host-iced` | `iced_wgpu`, which *is* wgpu | Yes, first-party. `Renderer::draw` targets any `TextureView`. |
| `host-wgpui` | wgpu + winit (fork of gpui-ce) | Yes, via the fork. |
| `host-gpui` | D3D12 / Metal / Vulkan | **No.** A texture copy per frame is unavoidable. |

So the third config is not a fourth option, it is the control. If gpui-as-is
lands inside budget, the fork question is moot. If it does not, gpui only stays
in the running by way of a fork of a fork, and iced wins by default.

## Crates

- `canvas-core` — item model, viewport, spatial hash, culling, LOD. No GPU.
  10 unit tests, all green. Most of the correctness risk lives here.
- `canvas-gpu` — wgpu renderer. Fixed-slot atlas with LRU eviction, mip pyramid,
  one instanced draw call.
- `canvas-harness` — procedural content and frame metrics. No asset files.
- `host-*` — the three UI hosts.

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

If more than one config passes, the tiebreak is the one that needs no fork.

## Measured

Canvas only, no UI library. Intel Iris Xe, Vulkan backend. Frame time is CPU
submit plus a hard `device.poll(Wait)`, which serialises CPU and GPU and is
pessimistic against a pipelined app.

`./scripts/bench.ps1` reproduces these. Expect roughly 2x run-to-run variance on
a shared iGPU; read p99 and the over-budget count rather than the mean.

| Scenario | Visible | Mean | p99 | Max | Over budget | Placeholders |
|---|---|---|---|---|---|---|
| 2,000 items / 2,000 distinct | 1,194 | 1.6ms | 4-5ms | 6-11ms | 0/200 | 0.00% |
| 8,000 items / 8,000 distinct | 4,967 | 6.7-13.3ms | 24-55ms | 29-124ms | 7-54/200 | 0.00% |
| 20,000 items / 20,000 distinct | 12,431 | 32-42ms | 121-160ms | 134-287ms | 130-140/200 | 0.00% |
| 32,000 items / 32,000 distinct, all on screen | 32,000 | 774ms | 5,100ms | 6,177ms | 150/150 | 0.00% |

The 2,000-item case SQU-60 specifies passes with about 10x headroom. Degradation
starts near 5,000 simultaneously visible distinct images; 12,000 is over budget.
The 32,000 case is a wall of thumbnails, not a moodboard, and is listed to show
where the wall is rather than as a target.

**CPU-bound at 85-90% throughout.** The cost is culling, building the per-frame
instance buffer, and uploading texture levels. The whole board is one draw call
with no batching logic, so draw-call overhead is not a factor at any item count.

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

- `host-iced`: reference implementation, one wgpu device shared with the canvas.
- `host-wgpui`: fetch the fork, confirm it builds against a current toolchain.
- `host-gpui`: measure the cost of the per-frame texture copy.
- Canvas text via `cosmic-text` + `glyphon`, verified against the chrome's text.
- Decide atlas capacity policy, or move off the atlas.
