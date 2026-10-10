#!/usr/bin/env node
// Import the addon configuration of the Nuvio web app (space.nuvio.webos) into Nuvio Native.
//
//   node tools/nv-import-nuvio.mjs <tv-ip> [--appid com.nuvionative.app] [--device tv] [--profile ID] [--out FILE [--no-push]]
//
// Nuvio must be RUNNING on the TV (Developer Mode exposes its WebView on DevTools port 9998). This
// reads — never writes — the active profile's per-profile localStorage keys and pushes them, raw,
// to /tmp/<appid>/nuvio-import.json (or /tmp/nuvio-import.json for the stable id); Nuvio Native
// consumes that file at its next launch, stores it in its own data, and deletes it.
//
// Addon URLs can embed debrid credentials, so nothing here prints them: only hosts.
import { execFileSync } from "node:child_process";

const [, , host, ...rest] = process.argv;
if (!host) { console.error("usage: nv-import-nuvio.mjs <tv-ip> [--appid ID] [--device NAME] [--profile ID]"); process.exit(2); }
const opt = (n, d) => { const i = rest.indexOf(n); return i >= 0 ? rest[i + 1] : d; };
const appid = opt("--appid", "com.nuvionative.app");
const device = opt("--device", "tv");
const KEYS = ["installedAddonUrls", "installedAddonEnabledStates", "installedAddonDisplayNames", "addonManifestCacheV2", "activeProfileId", "profiles"];

const pages = await (await fetch(`http://${host}:9998/json/list`)).json();
const page = pages.find((p) => p.url.includes("space.nuvio.webos")) || pages.find((p) => /nuvio/i.test(p.title));
if (!page) { console.error("Nuvio is not running (no inspectable Nuvio page on :9998). Launch it first: ares-launch --device tv space.nuvio.webos"); process.exit(1); }
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
ws.send(JSON.stringify({ id: 1, method: "Runtime.evaluate", params: { returnByValue: true, expression: `(${JSON.stringify(KEYS)}).reduce((o,k)=>(o[k]=localStorage.getItem(k),o),{})` } }));
const values = await new Promise((r) => (ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id === 1) r(d.result.result.value); }));
ws.close();

const parse = (s) => { try { return JSON.parse(s); } catch { return s; } };
const profileId = opt("--profile", String(parse(values.activeProfileId) ?? "1"));
const urls = parse(values.installedAddonUrls);
const list = urls?.profiles?.[profileId] ?? [];
if (!Array.isArray(list) || list.length === 0) { console.error(`no addons found for Nuvio profile ${profileId}`); process.exit(1); }

const payload = { source: "nuvio-localstorage", version: 1, profile_id: profileId, exported_at: new Date().toISOString(), local_storage: values };
const out = opt("--out", null);
if (out) {
  // A host copy for the simulator. It carries credentials: keep it outside any repository.
  const { writeFileSync } = await import("node:fs");
  writeFileSync(out, JSON.stringify(payload), { mode: 0o600 });
  console.log(`wrote ${list.length} addons to ${out}`);
  if (rest.includes("--no-push")) process.exit(0);
}
const rundir = appid.split(".").length === 3 ? "/tmp" : `/tmp/${appid}`;
const dest = `${rundir}/nuvio-import.json`;
// Developer Mode profiles do not support `ares-push`, so the file travels as base64 chunks through
// `ares-novacom --run` (the sanctioned channel) and is decoded on the set. The app runs under a
// different uid in the same group (5000) as the dev-mode shell, hence 640.
const b64 = Buffer.from(JSON.stringify(payload)).toString("base64");
const run = (cmd) => execFileSync("ares-novacom", ["--device", device, "--run", cmd], { stdio: "ignore" });
run(`mkdir -p ${rundir} && umask 027 && : > ${dest}.b64`);
for (let i = 0; i < b64.length; i += 60000) run(`printf %s '${b64.slice(i, i + 60000)}' >> ${dest}.b64`);
run(`base64 -d ${dest}.b64 > ${dest}.tmp && rm -f ${dest}.b64 && chmod 640 ${dest}.tmp && mv ${dest}.tmp ${dest}`);
console.log(`pushed ${list.length} addons from Nuvio profile ${profileId} to ${rundir}/nuvio-import.json:`);
for (const u of list) { try { console.log("  -", new URL(String(u)).host); } catch { console.log("  - (unparseable)"); } }
console.log(`relaunch ${appid} to apply.`);
