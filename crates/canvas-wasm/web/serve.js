// Static host for the browser canvas spike.
//
// Two jobs and no framework: serve `crates/canvas-wasm/` so the page and the
// wasm-pack output are reachable from one origin, and take the RESULT line the
// page measured and append it to a file.
//
// The file matters more than it looks. A figure quoted from a screenshot is a
// transcription, and this repo has a documented habit of publishing numbers
// that cannot be reproduced. The native runner captures to a file for the same
// reason, and `scripts/web-bench.ps1` brackets both runs with the machine's CPU
// load so the number carries its conditions.
//
// Port and log path come from arguments, never from a constant, because a
// committed localhost port is a machine-specific path by another name.

import { appendFileSync } from "node:fs";
import { resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

// `resolve` because the URL form keeps a trailing separator, and comparing
// against `root + sep` then looks for a doubled separator and rejects
// everything including the page itself.
const crateRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const port = Number(process.argv[2] ?? process.env.PORT ?? 0);
const resultLog = process.argv[3] ?? process.env.RESULT_LOG ?? null;

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json",
  // `WebAssembly.instantiateStreaming` refuses anything else, and the failure
  // mode is a confusing compile error rather than a content-type complaint.
  ".wasm": "application/wasm",
  ".css": "text/css; charset=utf-8",
};

const server = Bun.serve({
  hostname: "127.0.0.1",
  port,
  async fetch(req) {
    const url = new URL(req.url);

    if (req.method === "POST" && url.pathname === "/result") {
      const body = await req.json();
      const line = body.error
        ? `browser | backend=${body.backend} | ERROR ${body.error}`
        : [
            "browser",
            `backend=${body.backend}`,
            `${body.scene.items} items / ${body.scene.textures} distinct`,
            `atlas=${body.scene.atlas}`,
            `extent=${body.scene.extent}`,
            `pan=${body.scene.pan}`,
            `zoom=${body.scene.zoom}`,
            body.size,
            body.adapter,
            body.result,
          ].join(" | ");
      console.log(line);
      if (resultLog) appendFileSync(resultLog, `${line}\n`);
      return new Response("ok");
    }

    const rel = url.pathname === "/" ? "/web/index.html" : url.pathname;
    const path = resolve(crateRoot, `.${rel}`);
    // Serving is scoped to the crate, not the disk: a static server that will
    // hand out `../../.ssh` is not a dev convenience.
    if (path !== crateRoot && !path.startsWith(crateRoot + sep)) {
      return new Response("forbidden", { status: 403 });
    }

    const file = Bun.file(path);
    if (!(await file.exists())) return new Response("not found", { status: 404 });

    const ext = path.slice(path.lastIndexOf("."));
    return new Response(file, {
      headers: {
        "content-type": TYPES[ext] ?? "application/octet-stream",
        // The page is rebuilt constantly during a spike, and a cached wasm-pack
        // bundle is a very confusing thing to debug.
        "cache-control": "no-store",
      },
    });
  },
});

const { port: bound } = server;
console.log(`dreamscape canvas - browser host`);
console.log(`  http://127.0.0.1:${bound}/web/index.html            interactive`);
console.log(
  `  http://127.0.0.1:${bound}/web/index.html?bench=1     measured run`,
);
if (resultLog) console.log(`  results -> ${resultLog}`);
