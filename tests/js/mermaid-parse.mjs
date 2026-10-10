// Validates the vendored mermaid bundle + docs/examples/mermaid.md in jsdom.
// Usage: cd tests/js && npm install jsdom@24 && node mermaid-parse.mjs
// Parsing needs no layout engine; visual rendering must still be checked in the real webview.
import { JSDOM } from "jsdom";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, "..", "..");
const dom = new JSDOM("<!doctype html><body></body>", { runScripts: "dangerously" });
dom.window.structuredClone = structuredClone; dom.window.TextEncoder = TextEncoder; dom.window.TextDecoder = TextDecoder; // jsdom lacks these; real webviews have them
// A real <script> element, like the viewer uses (top-level `var` must become global).
const el = dom.window.document.createElement("script");
el.textContent = readFileSync(path.join(root, "assets/vendor/mermaid.min.js"), "utf8");
dom.window.document.body.appendChild(el);
const mermaid = dom.window.mermaid;
if (!mermaid) throw new Error("bundle did not define window.mermaid");

const md = readFileSync(path.join(root, "docs/examples/mermaid.md"), "utf8");
const blocks = [...md.matchAll(/```mermaid\n([\s\S]*?)```/g)].map((m) => m[1]);
let failed = 0;
for (const [i, src] of blocks.entries()) {
  try { await mermaid.parse(src); console.log(`ok   block ${i}`); }
  catch (e) { failed++; console.log(`FAIL block ${i}: ${e.message}`); }
}
// Negative control: garbage must be rejected.
try { await mermaid.parse("not a diagram at all"); failed++; console.log("FAIL: garbage accepted"); }
catch { console.log("ok   garbage rejected"); }
if (blocks.length < 6) { failed++; console.log("FAIL: expected >= 6 blocks"); }
process.exit(failed ? 1 : 0);
