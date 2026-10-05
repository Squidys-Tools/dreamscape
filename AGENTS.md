# Dreamscape

Dreamscape is an infinite canvas for collecting and arranging visual references. This repo is not the product. It is the **canvas architecture spike** (SQU-60): a wgpu renderer, a headless benchmark, and the experiments that decide how the canvas gets hosted.

Think of it as the measurement the product decisions depend on. Most of the code here exists to produce a number or settle a question, and the questions are tracked in Linear, not in this repo.

## What must not be compromised

The entire value of this repo is that its numbers can be trusted. Everything else is negotiable. These are the specific ways that stops being true, and every one of them has actually happened here.

### 1. A number you cannot reproduce is worse than no number

An unreproducible figure is not a neutral leftover, it is a trap for whoever reads it next. This repo shipped a table claiming 1,194 visible items and 32-42ms at 20,000 items. None of it could be reproduced from the code that produced it. The claims were removed rather than adjusted.

Watch for: quoting a figure from an earlier run, averaging across two different configurations, transcribing a number by hand off a terminal, and reporting a mean without the p99 and the over-budget count beside it. Expect roughly 2x run-to-run variance on a shared iGPU, so a single run is a sample, not a result.

### 2. Measure the case, not a proxy for it

The scene is generated, which means it can be generated wrong, and the benchmark will happily report a beautiful number for an empty board. `Config::extent` was 12,000, which scattered items across a 24,000-square world area while a 2560x1440 viewport covers about 7,300 of it. Only 5% of items were ever on screen, so the 2,000-item scenario rendered 374 items in 0.4ms. The number was real and the claim was worthless.

The same trap has a quieter form. A full board of 2,000 distinct images reports 0% placeholders, because the atlas is partitioned by mip level and overflow degrades to a coarser level instead of failing. An 8,192 atlas and a 2,048 atlas reported identically until the harness learned to count **degraded** items separately from placeholders. So "0% placeholders" means "nothing failed", not "everything looks right", and it is only meaningful next to the degraded count.

Also: a still frame is not a pan, CPU submit is not frame time, and headless is not windowed.

### 3. Never quietly change what a metric means

The `RESULT` line emitted by the bench binary once had a `visible` field, then was refactored to emit `peak_visible` and dropped a `submit_ms` field. `scripts/bench.ps1` kept reading the old names. In PowerShell a missing key is `$null`, which casts to `0`, so the Visible and CpuMs columns printed `0` and `0.00` and the script looked like it was working.

If a metric changes name, unit or meaning, every consumer changes in the same commit: the `FrameStats` field, the `RESULT` line, the parser, the report, and the table in the docs. `bench.ps1` now throws on any missing key for exactly this reason. Do not loosen that.

### 4. The canvas is one codebase, or it is two

Settled in SQU-72. The canvas is Rust and compiles twice, natively for the desktop app and to `wasm32` for the browser. A web view cannot share a wgpu device with the canvas, because the view owns its own GPU context, which is why the desktop shell is native and the hosted tier is a web page with its own chrome.

The failure mode is drifting toward two renderers. Putting `wasm-bindgen` in `canvas-app` instead of its own thin crate. Hand-declaring a shape in TypeScript that Rust already defines. Rebuilding the canvas in TypeScript "just for the web tier".

### 5. Document what exists, not what is intended

This file once described a `justfile`, a `web/` directory and three crates, none of which existed, and named `crates/host-desktop/` where the real directory is `crates/host-iced/`. It was documenting the planned repo as though it were the current one, which broke the rule two sections below it.

Planned structure goes in Linear. At most a short "as planned" list here, clearly marked.

## A note on taste

Simple software that feels obvious beats impressive machinery. Do not preserve complexity just because it already exists. Do not introduce machinery because it looks architecturally impressive. Understand the real constraint, then fight for the smallest model that makes the correct behavior unsurprising.

Channel both "measure twice, cut once" and "yagni". Fight scope creep. Try to honor the dev's intent in both a minimal and realistic fashion.

The rest of this document is meant to help you navigate the codebase and make changes effectively. Think of these instructions less as "hard rules", more as "good defaults". The developer's preferences should be able to override anything here.

If a rule here fights the task in front of you, say so plainly and get a human sign off before breaking it.

## Glossary

We need to be on the same page with terminology. When communicating, use this language:

- **you** means the agent reading this file and changing Dreamscape.
- **we and maintainers** mean the people building Dreamscape. That is who you are talking to now.
- **board** means the infinite canvas surface. The thing being drawn.
- **item** means one thing on the board: an image, text, video, swatch or stroke. Data, not UI.
- **atlas** means the fixed-slot GPU texture holding the mip levels of every visible thumbnail.
- **mip level** means one resolution of a thumbnail's pyramid. Level 0 is full size.
- **resident** means a mip level currently occupying an atlas slot.
- **placeholder** means a flat swatch drawn because no level could be placed. A bug signal, not styling.
- **degraded** means drawn at a mip level coarser than the one the item's on-screen size asked for. Designed behaviour, not a bug: a placeholder is a broken board, a degraded board is an undersized atlas.
- **cull** means deciding which items intersect the viewport. Runs every frame.
- **LOD** means picking which mip level an item draws at, from its size on screen.
- **peak visible** means the most items on screen in any measured frame. This is the number that says whether a scene is dense enough for its frame times to mean anything.
- **frame time** means CPU submit plus a hard `device.poll(Wait)`. It serialises CPU and GPU, so it is a ceiling rather than a typical frame.
- **over budget** means frames over 16.67ms, the 60fps threshold.
- **seam** means the small interface between the canvas and whatever hosts it. One definition, two implementations.
- **desktop shell** means the native Rust UI on Windows. Shares one wgpu device with the canvas.
- **hosted tier** means the paid web version. View and modify only; you cannot add anything to it.
- **capture-only** means the browser extension's entire job. It carries no canvas and no UI.
- **spike** means code that exists to produce a measurement or settle a question, not to ship.

## The four ways to hurt yourself

1. **Reading a frame time without the visible count beside it.** This is the single most expensive mistake available here, and it has already produced a comfortable-looking table that means nothing. Peak visible is not a footnote, it is the validity check on everything else in the row.

2. **Writing a number into the docs from memory, or from a terminal you happened to have open.** Capture the run, read the number off the captured output. If you cannot say which command and which machine produced a figure, it does not go in the docs.

3. **Killing by pattern, or blocking on a sleep.** Never `Stop-Process -Name`, `pkill -f`, or kill a PID found by matching a name or path. This repo runs inside T3 Code and the machine runs other things. Kill only a PID you captured at spawn. Never `Start-Sleep` after launching a bench or a server: start it detached, do other work, then poll its log.

4. **Committing generated output or a machine-specific path.** `crates/canvas-wasm/pkg/` is `wasm-pack` output. Never hardcode an absolute user path, a machine-specific cache location, or a localhost port into source or committed config. A fresh clone on another Windows machine has to work. The web host binds port 0 and prints the port it got, for the same reason `bench.ps1` captures a RESULT line rather than reading a console table.

## Hit every surface

The common defect here is a change that works on the path you tested and is missing everywhere else. Before calling work done, walk this list and say which entries applied.

- **Entry points.** The per-frame pipeline is reached by `canvas-harness`, by `host-iced`, and later by the WASM host and the server. A change to `AppState::draw_frame` has to hold in all of them, not just the one you ran.
- **Targets.** Native and `wasm32-unknown-unknown`, and the wasm one is now a build that the "Check before commit" script runs rather than an aspiration. There is no CI yet, which is the gap: nothing catches a target-specific regression but whoever runs the script. Platform-specific dependencies in a shared crate erode the layering that lets one renderer serve both, so check that `canvas-core`, `canvas-gpu` and `canvas-app` still build for a target that has no window system. `pollster` is the trap to look for: it compiles for wasm and cannot work there.
- **Metrics.** A field flows from `FrameStats` to the `RESULT` line to `bench.ps1` to the report to the table in the docs. All of them or none. See principle 3.
- **Reverse states.** A new bench scenario needs a row in the docs table. A new crate needs removing from the workspace when it goes, and its dependency tree out of `Cargo.lock`. Adding a way in without a way out is a bug.
- **Docs.** Check whether the change makes existing guidance inaccurate. Apply the documentation rules before adding anything new.

## Running things

- Rust 1.92 is the floor in `rust-version`. There is no pinned channel, so a
  machine with a newer rustc builds with the newer rustc; see the known-broken
  list, because a second Rust install on `PATH` outranks rustup.
- `wasm32-unknown-unknown` is declared in `rust-toolchain.toml`, so `rustup`
  installs the target's std on the clone that needs it. Do not remove that line
  and do not trust `rustup target list --installed` as proof the target works:
  this repository shipped a claim that it was installed while the target's lib
  directory was empty, and every wasm build failed with `can't find crate for
  core`.
- **bun for all JavaScript work.** Not npm, not yarn, not pnpm. bun 1.4.2 is on PATH.
- `just` is not installed, so nothing may invoke it. Project commands are registered in `t3.json` at the repo root, and that is what the T3 Code scripts menu runs.
- **The manifest filename is not ours to choose.** T3 Code hard-codes `t3.json` and silently ignores any other name, so do not rename it to `dreamscape.json` or anything else. Verified against the installed bundle: the lookup passes the literal `t3.json`, and the invalid-manifest message is hard-coded to that name too. The repo can have a different name than the file.
- Cargo and bun share no root manifest, so build order across the two ecosystems is orchestrated explicitly rather than by a root task runner.
- Never start a windowed binary and then immediately assert what it drew. See the known-broken list.

## Verifying

- Smallest proof that the change works. The focused test for the module you touched, plus targeted checks for the scope you changed.
- Test meaningful logic. Do not add a test that mirrors the implementation or asserts wiring with no behavior. The 10 tests in `canvas-core` cover culling, the spatial index and LOD, which is where the correctness risk actually lives.
- A GPU change is verified by running the benchmark, not by reading the diff. `.\scripts\bench.ps1 -Scenario quick` is the fast loop; the full sweep is the real one.
- `cargo clippy` currently emits one pre-existing warning in `canvas-gpu` about indexing `per_row`. It is deliberately left alone, so a clean run means "one warning", not "no warnings".
- Never claim a check you did not run. If a number is reported, say which command produced it and on which machine.

## Linear issues

- One Linear issue owns one task. That issue is the tracker, not a plan file and not a checklist in this repo.
- Before starting: read the issue and its comments, move it to In Progress, branch from its `gitBranchName`.
- While working: post progress as issue comments. Never rewrite the description to add a log.
- Before finishing: move the issue to its review state, and mark it Done only once the work is landed or the developer confirms.
- A merged commit is the implementation record. Do not preserve a second checklist in the repository.

## Documentation

Most code changes need no docs change. Agents can read the code.

- `docs/spikes/canvas-spike.md` is the internals document: the design, the findings that forced it, and the measurements, each with the command that produced it. That is the only home for a measurement.
- `AGENTS.md` is how to work in this repo. `README.md` is the front door and an index.
- **Do not invent a second place for the same fact.** The README used to carry its own copy of the benchmark table, which is a copy that drifts. It links instead.
- Before adding a paragraph, ask what a maintainer would get wrong without it. If reading the relevant code answers the question, leave it out.
- Do not document every crate, enumerate modules, narrate control flow, or append commit summaries. The code and the tests already record the implementation.
- Keep a local implementation explanation in a nearby code comment. Use the spike document when the reasoning crosses crate boundaries.
- When a documented decision or constraint changes, rewrite or remove the affected text. Do not append another account of the new behavior.
- A new document needs a distinct, durable reason to exist. A spike with one internals document does not need a `docs/` hierarchy yet.

## How it works

Every frame, in every consumer: advance the camera, cull the grid to the viewport, pin the visible set, request a mip level for anything not already resident, upload those, encode one instanced draw call, submit.

A host supplies the device, the queue and a target texture view. The canvas renders into its own offscreen `Rgba8UnormSrgb` texture, and the host blits that texture inside its existing render pass, so nothing crosses the CPU. The headless harness does the same into a plain texture and additionally waits for the GPU, which is why its numbers are a ceiling.

Full reasoning, and the findings that shaped it, in `docs/spikes/canvas-spike.md`.

## Where code lives

- `crates/canvas-core` is the item model, viewport, spatial hash, culling, LOD and the camera maths pan and zoom need. No GPU, no platform dependencies, 14 unit tests. Most of the correctness risk lives here.
- `crates/canvas-gpu` is the wgpu renderer: fixed-slot atlas, lazy per-level allocation, eviction, one instanced draw call.
- `crates/canvas-app` is the scene, camera, selection, search and the per-frame pipeline. Every consumer links this, so the only thing that differs between them is how the chrome is drawn.
- `crates/canvas-harness` is the headless driver over `canvas-app` plus frame metrics. No asset files.
- `crates/canvas-wasm` is the browser host, and the only crate that may depend on `wasm-bindgen`. It is thin on purpose: it owns a GPU context, a clock and the compositor, and nothing else. `web/` is the page, the local server and the capture endpoint; `pkg/` is generated.
- `crates/host-iced` is an iced 0.14 host sharing one wgpu device with the canvas. It draws and its counters read healthy, but nothing on it has been confirmed against a screen; see the known-broken list.
- `crates/probe-surface` is a throwaway raw winit plus wgpu control. Delete it once presentation works.
- `scripts/bench.ps1` is the native benchmark runner, `scripts/web-bench.ps1` the browser one. `docs/spikes/canvas-spike.md` is the only internals document.

## Known broken

Recorded so nobody re-derives the failure. Tracked in Linear.

- **`cargo run -p probe-surface` presents nothing on one specific machine.** SQU-73. On the machine this work has been driven from, the probe presents 300+ frames with `present()` returning `Ok` and no wgpu errors, and the client area is white instead of magenta, confirmed foregrounded and in a full-desktop capture where other GPU-composited windows render normally. **A second Windows machine runs the identical binary and shows magenta**, so this is not wgpu, not Vulkan, and not a canvas problem. Treat it as a property of one box and suspect driver or GPU configuration before suspecting the renderer. Two bugs hid behind it: the canvas vertex shader was clipping every quad until the SQU-83 work, and the probe itself was panicking on any monitor wider than 2048 because it never resolved its texture limits against the adapter.
- **Do not generalise that failure into a claim about the desktop tier.** The window path works on other hardware, so the toolkit comparison and the native benchmarks are runnable. Nothing is blocked that says "on this laptop".
- **Every frame time in this repository was measured on a canvas that drew nothing.** The vertex shader put device pixels straight into clip space, so every quad was clipped and the clear colour was the whole frame. No harness looked at a pixel, so every counter-based metric read healthy. Fixed in the renderer, and `canvas-wasm`'s `verify_pixels` exists because a screenshot and a `RESULT` line both failed to notice. Until the table is re-measured on a quiet machine, treat every published figure as a lower bound and re-derive it rather than citing it.
- **A second Rust install can shadow the rustup one.** Cargo resolves `rustc` from `PATH`, so on a machine where a standalone install comes first, cross-target builds fail with `can't find crate for core` and native builds quietly use a different compiler than the documented floor. Check `rustc --version` against `rustup show` before believing a toolchain problem is a target problem.
- **A headless browser will not confirm that a WebGPU canvas is presented.** The page screenshot is blank and `createImageBitmap` on the canvas reads an empty layer, while a 2D control reads back correctly through the same code. Verify the renderer with GPU readback; verify the compositor with a headed browser.
- **An exported `&mut self` that awaits is a re-entrancy trap in wasm.** The borrow is held until the future resolves, so any JS call into the same object in that window throws `recursive use of an object detected which would lead to unsafe aliasing in rust`. It reads as dropped input, not a crash. Keep mutable state behind a `RefCell` and take `&self` on exports, so no borrow is live across an await.
- **A driven path that was only ever scripted stays broken.** The benchmark route into the browser worked, and the interactive route was dead twice over, both times only visible once something sent real pointer and wheel events. Exercise the path a person uses, not just the one the harness uses.
- **Wheel zoom on the native host is fixed and unverified.** The handler matched `ScrollDelta::Pixels` only, which is what a browser sends, while winit reports the native wheel in notches as `LineDelta`. Both are handled now, against the 100px notch `canvas-core` assumes, and the browser page stopped ignoring `deltaMode`. Pan is proven, by a 300px drag that moved the camera centre as the scale predicts. Zoom is not: input only landed once, right after the window had been activated, and 150 real notches moved the camera zero pixels. Treat it as a code change with no evidence behind it until someone scrolls on the second machine.

## Taste

- Complexity belongs at the adapter boundary. The renderer stays pure, hosts stay dumb.
- Inferred types over annotations.
- Comments explain why, not what. The existing code carries a reasoning comment wherever a choice looks odd, especially where a finding forced the design. Match that density or raise it. A comment restating the line below it is noise.
- The whole board is one draw call, so resist adding batching logic at low item counts. It is not where the time goes.
- A grey placeholder on screen is a bug report about the atlas, never a styling decision.
- If a rule here fights the task in front of you, say so loudly and get a human sign-off before breaking it.

## Working in this harness

These apply when running inside T3 Code only, and they override nothing above.

- Screenshots the user must see never render from a tool result. Save with `screenshot_out_file` and embed the path, so the image survives the turn.
- Computer use is for looking at a window, not for driving the product. Do not use it to verify unless the developer agrees or asks.

## Additional tips

- Do not verify with a browser or computer use unless the developer agrees or asks.
- A spike earns its keep by settling a question. If code has outlived the question it was written to answer, deleting it is the correct change.
