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

## Measured so far

Canvas only, no UI library, Intel Iris Xe, Vulkan backend.

| Scene | serial mean | p99 | max | over budget |
|---|---|---|---|---|
| 2,000 visible, 1600×900 | 0.53ms | 1.47ms | 1.90ms | 0 / 400 |
| 2,000 visible, 2560×1440 | 1.03ms | 2.81ms | 3.15ms | 0 / 400 |

Roughly 5× headroom on the pessimistic measure, so considerably more in a real
pipelined app. The whole board is one draw call with no batching logic, so
draw-call overhead is not a factor at any item count.

## Findings that changed the design

**The WebGL2 floor rules out storage buffers.** Committing to WebGL2 as the
graphics floor means `max_storage_buffers_per_shader_stage = 0`, so per-instance
data cannot be fed through a storage buffer in the vertex stage. It is fed
through an instanced vertex buffer instead, which is more portable anyway. This
would have been discovered late and expensively if the floor had not been
declared up front.

**Pin before ingest, never after.** Ingesting one newly visible texture can evict
another that is already on screen but has not been visited by the loop yet. The
symptom is a hole in the board. Fixed by pinning the visible set first.

**Fine mip levels are the scarce resource, not texture count.** A 2,048² atlas
holds 64 slots at mip 0 and 65,536 at mip 6, so a working set is over-subscribed
at the fine levels long before it is over-subscribed overall. The current
renderer allocates all 7 levels the moment a texture becomes resident, which
wastes the fine levels on items that are only ever seen zoomed out.

*Follow-up, not a blocker:* allocate mip levels lazily, on first request. Most
visible items need one or two levels, not seven.

**Placeholders are required, and are nearly free.** When no atlas slot is
available the item draws as a flat swatch rather than vanishing. This is
implemented as a zero-area uv rect aimed at one reserved texel, so it needs no
shader branch and costs the same as a real item. This is the "minimum-size
placeholders" clause in SQU-60, and it is load-bearing whenever the atlas is
undersized for the working set.

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
- Lazy per-level mip allocation.

## Running it

```sh
cargo test -p canvas-core
cargo run -p canvas-harness --bin bench --release
cargo run -p canvas-harness --bin bench --release -- --pan=0 --zoom=0.96 --width=2560 --height=1440
cargo run -p canvas-harness --bin bench --release -- --atlas=512   # deliberate pressure
```

Flags: `--items --textures --atlas --width --height --warmup --frames --pan --zoom`.
