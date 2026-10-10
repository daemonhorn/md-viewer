// Verifies Ctrl+P handling in assets/app.js under jsdom.
// Usage: cd tests/js && npm install jsdom@24 && node print-key.mjs
import { JSDOM } from "jsdom";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const js = readFileSync(path.join(here, "..", "..", "assets/app.js"), "utf8");
const html = `<!doctype html><html data-theme="auto" data-mode="rendered"><body>
<div id="mdv-toolbar"><button id="mdv-print">Print</button></div>
<article class="markdown-body"><h1 id="a">A</h1></article>
<div id="mdv-find" hidden><input type="text"><button class="mdv-prev"></button><button class="mdv-next"></button></div>
</body></html>`;

let failed = 0;
const check = (ok, msg) => { console.log(`${ok ? "ok  " : "FAIL"} ${msg}`); if (!ok) failed++; };

function load(withIpc) {
  const dom = new JSDOM(html, { runScripts: "outside-only", pretendToBeVisual: true });
  const w = dom.window;
  const msgs = [];
  if (withIpc) w.ipc = { postMessage: (m) => msgs.push(m) };
  w.matchMedia = w.matchMedia || (() => ({ matches: false }));
  w.eval(js);
  return { w, msgs };
}
function key(w, target, init) {
  const e = new w.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(e);
  return e;
}

{ // document-level Ctrl+P
  const { w, msgs } = load(true);
  const e = key(w, w.document.body, { key: "p", ctrlKey: true });
  check(e.defaultPrevented, "Ctrl+P is preventDefault()ed");
  check(msgs.length === 1 && msgs[0] === "print", "Ctrl+P posts exactly 'print'");
  const e2 = key(w, w.document.body, { key: "P", ctrlKey: true });
  check(msgs.length === 2 && e2.defaultPrevented, "Ctrl+Shift-less uppercase P also works");
  const e3 = key(w, w.document.body, { key: "p", ctrlKey: true, shiftKey: true });
  check(!e3.defaultPrevented && msgs.length === 2, "Ctrl+Shift+P is ignored");
  const e4 = key(w, w.document.body, { key: "p" });
  check(!e4.defaultPrevented && msgs.length === 2, "plain p is ignored");
  w.document.getElementById("mdv-print").click();
  check(msgs.length === 3 && msgs[2] === "print", "Print button posts 'print'");
}
{ // inside the find input
  const { w, msgs } = load(true);
  const input = w.document.querySelector("#mdv-find input");
  const e = key(w, input, { key: "p", ctrlKey: true });
  check(e.defaultPrevented && msgs.length === 1, "Ctrl+P inside find input prints");
  const e2 = key(w, input, { key: "p" });
  check(!e2.defaultPrevented && msgs.length === 1, "typing 'p' in find input is untouched");
  const e3 = key(w, input, { key: "r" });
  check(!e3.defaultPrevented && !w.location.search.includes("raw"), "typing 'r' in find input does not toggle raw");
}
{ // no ipc available
  const { w } = load(false);
  let threw = false;
  try {
    key(w, w.document.body, { key: "p", ctrlKey: true });
    w.document.getElementById("mdv-print").click();
    w.dispatchEvent(new w.Event("beforeprint"));
    w.dispatchEvent(new w.Event("afterprint"));
  } catch { threw = true; }
  check(!threw, "no exception without window.ipc");
}
{ // beforeprint hides the find bar
  const { w } = load(true);
  const f = w.document.getElementById("mdv-find");
  f.hidden = false;
  w.dispatchEvent(new w.Event("beforeprint"));
  check(f.hidden && w.document.documentElement.classList.contains("mdv-printing"), "beforeprint hides find bar");
  w.dispatchEvent(new w.Event("afterprint"));
  check(!w.document.documentElement.classList.contains("mdv-printing"), "afterprint clears state");
}
process.exit(failed ? 1 : 0);
