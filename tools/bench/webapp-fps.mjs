#!/usr/bin/env node
// Baseline for the web Nuvio app: frame pacing while the remote navigates, measured inside the
// TV's WebView through the DevTools protocol that webOS exposes for Developer Mode web apps.
//
//   node tools/bench/webapp-fps.mjs <tv-ip> [--title "Nuvio TV"] [--keys R,R,R,D,R,R] [--gap 250]
//
// Injects a requestAnimationFrame recorder, dispatches the key script as real key events, and
// reports frame-interval statistics over the navigation window. Read-only for the app's data.
const [, , host, ...rest] = process.argv;
if (!host) { console.error("usage: webapp-fps.mjs <tv-ip> [--title T] [--keys R,D,L,U] [--gap ms]"); process.exit(2); }
const opt = (name, dflt) => { const i = rest.indexOf(name); return i >= 0 ? rest[i + 1] : dflt; };
const title = opt("--title", "Nuvio TV");
const keys = opt("--keys", "R,R,R,R,R,R,R,R,D,R,R,R,R,R,D,R,R,R,R,R,U,U,L,L,L,L").split(",");
const gap = Number(opt("--gap", "250"));
const KEYMAP = { R: ["ArrowRight", 39], L: ["ArrowLeft", 37], U: ["ArrowUp", 38], D: ["ArrowDown", 40], OK: ["Enter", 13], B: ["Escape", 461] };

const list = await (await fetch(`http://${host}:9998/json/list`)).json();
const page = list.find((p) => p.title === title) || list[0];
if (!page) { console.error("no inspectable page"); process.exit(1); }
const ws = new WebSocket(page.webSocketDebuggerUrl);
let id = 0; const pending = new Map();
const send = (method, params = {}) => new Promise((res) => { const i = ++id; pending.set(i, res); ws.send(JSON.stringify({ id: i, method, params })); });
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); } };
await new Promise((r) => (ws.onopen = r));
const evalJs = async (expr) => (await send("Runtime.evaluate", { expression: expr, returnByValue: true, awaitPromise: true })).result?.result?.value;

const nav = await evalJs(`(() => { const t = performance.timing; return { domContentLoaded: t.domContentLoadedEventEnd - t.navigationStart, load: t.loadEventEnd - t.navigationStart, heapMB: performance.memory ? +(performance.memory.usedJSHeapSize/1048576).toFixed(1) : null, imgs: document.images.length }; })()`);
await evalJs(`window.__nvf = []; (function f(t){ window.__nvf.push(t); if (window.__nvf.length < 100000) requestAnimationFrame(f); })(performance.now()); true`);
await new Promise((r) => setTimeout(r, 500));
const t0 = await evalJs("performance.now()");
for (const k of keys) {
  const [key, code] = KEYMAP[k] || KEYMAP.R;
  const base = { key, code: key, windowsVirtualKeyCode: code, nativeVirtualKeyCode: code };
  await send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...base });
  await send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  await new Promise((r) => setTimeout(r, gap));
}
await new Promise((r) => setTimeout(r, 800));
const frames = await evalJs(`window.__nvf.filter(t => t >= ${t0})`);
const iv = []; for (let i = 1; i < frames.length; i++) iv.push(frames[i] - frames[i - 1]);
iv.sort((a, b) => a - b);
const pct = (p) => iv[Math.min(iv.length - 1, Math.floor(iv.length * p))];
const span = frames[frames.length - 1] - frames[0];
const late = iv.filter((x) => x > 25).length;
console.log(JSON.stringify({
  page: page.title, nav, keys: keys.length, gap_ms: gap,
  frames: frames.length, avg_fps: +(1000 * (frames.length - 1) / span).toFixed(1),
  p50_ms: +pct(0.5).toFixed(1), p95_ms: +pct(0.95).toFixed(1), p99_ms: +pct(0.99).toFixed(1), max_ms: +iv[iv.length - 1].toFixed(1),
  frames_over_25ms: late, pct_late: +(100 * late / iv.length).toFixed(1),
}, null, 1));
ws.close();
