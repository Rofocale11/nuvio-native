#!/usr/bin/env node
'use strict';
/*
 * Trailer decipher child (REAL).
 *
 * System Node (/usr/bin/node, floor v16.20.2 on the dev set) runs this as a
 * short-lived child per resolve, like the P2P server.js pattern: no bundled
 * runtime, no embedded JS engine in the app. Rust spawns it per resolve
 * (10 s timeout, single-flight + cache in plx_pmsshim) and parses ONE JSON
 * line from stdout. stderr is for logs only; stdout must stay a single line.
 *
 * Interface:
 *   input : youtubeId as argv[2], or JSON on stdin {"youtubeId":"..."} /
 *           {"videoId":"..."} / {"id":"..."}
 *   output: {"ok":true,"kind":"muxed","url":"https://…","itag":18,"height":360}
 *            — best MUXED mp4/H264/AAC (the only progressive YouTube still
 *            ships above 144p), or
 *           {"ok":true,"kind":"dash","video":{…},"audio":{…},"durationSec":N}
 *            — HD adaptive pair (video-only + audio-only) the shim turns
 *            into a static MPEG-DASH manifest. When a ≥720p pair exists it
 *            wins over the 360p muxed file; the pair is ranked highest
 *            height first (so 2160p VP9 > 1080p h264 > 720p h264), codecs
 *            limited to h264/VP9 + AAC, or
 *           {"ok":false,"error":"<short reason>"} — fail soft, never throw.
 *
 * How it resolves (VISIONOS-first, no third-party proxy):
 *   1. GET https://www.youtube.com/ with a Safari `User-Agent`, take the
 *      page's `visitorData`, then POST `youtubei/v1/player` with the
 *      **VISIONOS** client and that visitor identity (`X-Goog-Visitor-Id`).
 *      Measured 2026-11-06: this is the only client that is served a
 *      trailer's adaptive tracks to their END. Every other anonymous client
 *      (IOS, ANDROID 20/21, iPad, MWEB) is granted exactly the first
 *      60.0 s of those tracks and answers `403` past it — which is why the
 *      DASH pair used to die mid-playback on most titles. Without
 *      `visitorData` VISIONOS itself answers `LOGIN_REQUIRED`.
 *   2. Pick the DASH pair from `adaptiveFormats`, bounded by what the
 *      TELEVISION can decode: the shim passes the device's own codec table as
 *      `PLX_TRAILER_CODECS` (preference order, `h264` always first) and
 *      `PLX_TRAILER_MAXH` (its decoder row height). A set with no VP9/AV1 row
 *      therefore never receives a 2160p track it cannot decode, and one that
 *      has the row gets the best available up to 2160p. Plus the mp4/AAC
 *      audio track. The shim re-probes the winner and asks each track for its
 *      last 64 KiB before publishing the `Media`.
 *   3. Fall back to IOS adaptive, then to the ANDROID muxed file
 *      (`formats[]`, itag 18/360p), which is served in full even when the
 *      adaptive tracks are limited. No client is granted a cipher-only
 *      response that would need the WEB `signatureCipher` + `n` JS decipher,
 *      which is still not run here (brittle on Node 16 builtins alone).
 *
 * Builtins only (`https`), ES2020 syntax for Node 16, no global `fetch`, no
 * npm deps. Read-only fetch to `www.youtube.com`, stdout only. No disk write.
 */
var https = require('https');
var INNERTUBE_KEY = 'AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8';
// One ANDROID attempt: it only supplies the muxed 360p fallback row now.
var ANDROID_VERSIONS = ['20.10.38'];
// What this television decodes, set by the shim from `devcaps` (the device's
// own codec table). Defaults are the conservative pair a host run gets.
var TRAILER_CODECS = String(process.env.PLX_TRAILER_CODECS || 'h264').toLowerCase().split(',');
var TRAILER_MAXH = parseInt(process.env.PLX_TRAILER_MAXH || '1080', 10);
// Set when YouTube tells us an id needs a signed-in account (age-restricted /
// members-only). No anonymous client can play it, so the shim must walk to the
// title's next trailer instead of treating it as a transient failure.
var LOGIN_REQUIRED = false;
function notePlayability(obj) {
  if (obj && obj.playabilityStatus && obj.playabilityStatus.status === 'LOGIN_REQUIRED') {
    LOGIN_REQUIRED = true;
  }
}
// The Safari identity youtube.com serves its own page to; the player call must
// look like the same browser for the visitor identity to be accepted.
var SAFARI_UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15';
// The client that is granted the whole adaptive track (see the header note).
var VISIONOS_CLIENT = {
  clientName: 'VISIONOS',
  clientVersion: '1.02',
  deviceMake: 'Apple',
  deviceModel: 'RealityDevice17,1',
  osName: 'visionOS',
  osVersion: '26.5.23O471',
  hl: 'en',
  gl: 'US'
};
function fail(reason) {
  try { process.stdout.write(JSON.stringify({ ok: false, error: String(reason).slice(0, 120) }) + '\n'); } catch (e) {}
  process.exit(0);
}
function readStdin() {
  return new Promise(function (resolve) {
    if (process.stdin.isTTY) { resolve(''); return; }
    var chunks = [];
    process.stdin.on('data', function (c) { chunks.push(c); });
    process.stdin.on('end', function () { resolve(Buffer.concat(chunks).toString('utf8')); });
    process.stdin.on('error', function () { resolve(''); });
  });
}
function pickId(argv, stdinText) {
  var id = argv[2] ? String(argv[2]).trim() : '';
  if (id) { return id; }
  if (stdinText) {
    try {
      var obj = JSON.parse(stdinText);
      var v = obj.youtubeId || obj.videoId || obj.id || '';
      if (typeof v === 'string' && v.trim()) { return v.trim(); }
    } catch (e) { /* fall through */ }
  }
  return '';
}
function httpsPostJson(urlStr, bodyStr, headers, timeoutMs) {
  return new Promise(function (resolve, reject) {
    var u;
    try { u = new URL(urlStr); } catch (e) { reject(e); return; }
    var req = https.request({
      hostname: u.hostname,
      path: u.pathname + u.search,
      port: 443,
      method: 'POST',
      headers: headers,
      timeout: timeoutMs
    }, function (res) {
      var chunks = [];
      res.on('data', function (c) { chunks.push(c); });
      res.on('end', function () {
        var text = Buffer.concat(chunks).toString('utf8');
        if (res.statusCode !== 200) { reject(new Error('http ' + res.statusCode)); return; }
        resolve(text);
      });
    });
    req.on('timeout', function () { req.destroy(new Error('timeout')); });
    req.on('error', reject);
    req.write(bodyStr);
    req.end();
  });
}
function pickMuxed(formats) {
  var best = null;
  if (!Array.isArray(formats)) { return null; }
  for (var i = 0; i < formats.length; i++) {
    var f = formats[i];
    if (!f || typeof f.url !== 'string' || !f.url) { continue; }
    if (f.url.indexOf('https://') !== 0) { continue; }
    var mime = String(f.mimeType || '');
    var mimeLow = mime.toLowerCase();
    if (mimeLow.indexOf('video/mp4') === -1) { continue; }
    if (mimeLow.indexOf('avc1') === -1) { continue; }
    if (mimeLow.indexOf('mp4a') === -1) { continue; }
    var h = typeof f.height === 'number' ? f.height : 0;
    if (!h && typeof f.qualityLabel === 'string') {
      var m = f.qualityLabel.match(/(\d+)p/);
      if (m) { h = parseInt(m[1], 10) || 0; }
    }
    var cand = { url: f.url, itag: typeof f.itag === 'number' ? f.itag : 0, height: h };
    if (!best || cand.height > best.height) { best = cand; }
  }
  return best;
}
function rangeOf(f) {
  return {
    init: { start: f.initRange ? Number(f.initRange.start) || 0 : 0, end: f.initRange ? Number(f.initRange.end) || 0 : 0 },
    index: { start: f.indexRange ? Number(f.indexRange.start) || 0 : 0, end: f.indexRange ? Number(f.indexRange.end) || 0 : 0 },
    contentLength: Number(f.contentLength) || 0
  };
}
function fmtDash(f) {
  return {
    url: f.url,
    itag: typeof f.itag === 'number' ? f.itag : 0,
    width: typeof f.width === 'number' ? f.width : 0,
    height: typeof f.height === 'number' ? f.height : 0,
    fps: typeof f.fps === 'number' ? f.fps : 0,
    mime: String(f.mimeType || ''),
    bitrate: typeof f.bitrate === 'number' ? f.bitrate : 0,
    range: rangeOf(f)
  };
}
// YouTube now returns one audio track per auto-dub (German, Spanish, Hindi, …)
// alongside the original, all with the same itag and bitrate. Taking the
// first one gave every trailer a dubbed soundtrack. Rank the original first:
// `audioIsDefault` is exactly the "English (US) original" entry, and a track
// flagged `isAutoDubbed` must never win over an untagged one.
function codecOf(mimeLow) {
  if (mimeLow.indexOf('avc1') !== -1) { return 'h264'; }
  if (mimeLow.indexOf('vp9') !== -1 || mimeLow.indexOf('vp09') !== -1) { return 'vp9'; }
  if (mimeLow.indexOf('av01') !== -1) { return 'av1'; }
  return '';
}
function audioScore(f) {
  var t = f.audioTrack || {};
  var rank = t.audioIsDefault === true ? 3 : (t.isAutoDubbed === true ? 1 : 2);
  return rank * 1000 + (f.itag === 140 ? 100 : 0);
}
function pickDashPair(formats) {
  var bestVideo = null;
  var audio = null;
  if (!Array.isArray(formats)) { return null; }
  for (var i = 0; i < formats.length; i++) {
    var f = formats[i];
    if (!f || typeof f.url !== 'string' || !f.url) { continue; }
    if (f.url.indexOf('https://') !== 0) { continue; }
    var mime = String(f.mimeType || '');
    var mimeLow = mime.toLowerCase();
    if (mimeLow.indexOf('audio/mp4') !== -1 && mimeLow.indexOf('mp4a') !== -1) {
      if (!audio || audioScore(f) > audioScore(audio)) { audio = f; }
      continue;
    }
    if (mimeLow.indexOf('video/') !== 0) { continue; }
    // Only codecs the television's own table lists, and never above its
    // decoder row. Height dominates; at equal height the earlier codec in
    // PLX_TRAILER_CODECS wins (h264 before vp9 before av1).
    var codec = codecOf(mimeLow);
    var ci = TRAILER_CODECS.indexOf(codec);
    if (!codec || ci === -1) { continue; }
    var h = typeof f.height === 'number' ? f.height : 0;
    if (!h || h > TRAILER_MAXH) { continue; }
    var cand = fmtDash(f);
    cand._score = h * 10 - ci;
    if (!bestVideo || cand._score > bestVideo._score) { bestVideo = cand; }
  }
  if (!bestVideo || !audio) { return null; }
  if (bestVideo.height < 720) { return null; } // DASH not worth it below HD
  delete bestVideo._score;
  return { video: bestVideo, audio: fmtDash(audio) };
}
// The visitor identity is what makes VISIONOS answer with (rather than refuse
// or truncate) the adaptive tracks. Fetched per child — cheap, and a stale id
// would silently degrade every trailer back to 60 s.
function fetchVisitorData() {
  return new Promise(function (resolve) {
    var req = https.get({
      hostname: 'www.youtube.com',
      path: '/',
      headers: { 'User-Agent': SAFARI_UA, 'Accept-Language': 'en-US,en;q=0.9' },
      // youtube.com's page carries more cookie headers than node's 16 KiB default.
      maxHeaderSize: 131072,
      timeout: 3000
    }, function (res) {
      var chunks = [];
      res.on('data', function (c) { chunks.push(c); });
      res.on('end', function () {
        var text = Buffer.concat(chunks).toString('utf8');
        var m = /"visitorData":"([^"]+)"/.exec(text);
        resolve(m ? m[1].replace(/\\u003d/g, '=').replace(/\\u0026/g, '&') : '');
      });
      res.on('error', function () { resolve(''); });
    });
    req.on('timeout', function () { req.destroy(); resolve(''); });
    req.on('error', function () { resolve(''); });
  });
}
// Ask a signed media URL for its LAST 64 KiB. googlevideo grants only the
// first 60 s of a trailer's adaptive track to most clients; such a track
// answers its head and `403`s at the end, which the player experiences as
// "playback failed" mid-clip. A 200/206 here is the grant that matters.
function tailServed(url, size, depth) {
  depth = depth || 0;
  return new Promise(function (resolve) {
    if (!url || !(size > 0)) { resolve(false); return; }
    var start = Math.max(0, size - 65536);
    var u;
    try { u = new URL(url); } catch (e) { resolve(false); return; }
    var settled = false;
    function finish(v) { if (!settled) { settled = true; resolve(v); } }
    var req = https.request({
      hostname: u.hostname,
      path: u.pathname + u.search,
      port: 443,
      method: 'GET',
      headers: { 'Range': 'bytes=' + start + '-' + (size - 1) },
      timeout: 2500
    }, function (res) {
      var code = res.statusCode;
      res.destroy();
      if (code === 206 || code === 200) { finish(true); return; }
      if ((code === 301 || code === 302) && res.headers.location && depth < 2) {
        tailServed(res.headers.location, size, depth + 1).then(finish);
        return;
      }
      finish(false);
    });
    req.on('timeout', function () { req.destroy(); finish(false); });
    req.on('error', function () { finish(false); });
    req.end();
  });
}
function pairServed(pair) {
  return Promise.all([
    tailServed(pair.video.url, pair.video.range.contentLength),
    tailServed(pair.audio.url, pair.audio.range.contentLength)
  ]).then(function (r) { return r[0] && r[1]; });
}
// One resolve, with one retry: the grant is per-request, so a limited answer
// is worth a second ask (fresh visitor) before conceding to the 360p row.
function resolveViaVisionos(videoId) {
  return visionosAttempt(videoId).then(function (first) {
    if (!first) { return null; }
    return pairServed(first).then(function (ok) {
      if (ok) { return first; }
      return visionosAttempt(videoId).then(function (second) {
        if (!second) { return first; }
        return pairServed(second).then(function (ok2) {
          return ok2 ? second : first;
        });
      });
    });
  });
}
function visionosAttempt(videoId) {
  return fetchVisitorData().then(function (visitorData) {
    if (!visitorData) { return null; }
    var client = {};
    for (var k in VISIONOS_CLIENT) { client[k] = VISIONOS_CLIENT[k]; }
    client.visitorData = visitorData;
    var url = 'https://www.youtube.com/youtubei/v1/player?key=' + INNERTUBE_KEY + '&prettyPrint=false';
    var body = JSON.stringify({
      context: {
        client: client,
        user: { lockedSafetyMode: false },
        request: { useSsl: true }
      },
      videoId: videoId,
      contentCheckOk: true,
      racyCheckOk: true
    });
    var headers = {
      'Content-Type': 'application/json',
      'Content-Length': Buffer.byteLength(body),
      'User-Agent': SAFARI_UA,
      'X-Goog-Visitor-Id': visitorData
    };
    return httpsPostJson(url, body, headers, 5000).then(function (text) {
      var obj;
      try { obj = JSON.parse(text); } catch (e) { return null; }
      notePlayability(obj);
      var sd = obj.streamingData || {};
      var pair = pickDashPair(sd.adaptiveFormats);
      if (!pair) { return null; }
      var len = 0;
      if (obj.videoDetails && obj.videoDetails.lengthSeconds) { len = Number(obj.videoDetails.lengthSeconds) || 0; }
      return { video: pair.video, audio: pair.audio, durationSec: len };
    }).catch(function () { return null; });
  });
}
function resolveViaIos(videoId) {
  var url = 'https://www.youtube.com/youtubei/v1/player?key=' + INNERTUBE_KEY + '&prettyPrint=false';
  var body = JSON.stringify({
    context: { client: { clientName: 'IOS', clientVersion: '21.03.36', hl: 'en', gl: 'US', deviceModel: 'iPhone16,2' } },
    videoId: videoId
  });
  var headers = {
    'Content-Type': 'application/json',
    'Content-Length': Buffer.byteLength(body),
    'User-Agent': 'com.google.ios.youtube/21.03.36(iPhone16,2; U; CPU iPhone OS 18_0 like Mac OS X)'
  };
  return httpsPostJson(url, body, headers, 4500).then(function (text) {
    var obj;
    try { obj = JSON.parse(text); } catch (e) { return null; }
    notePlayability(obj);
    var sd = obj.streamingData || {};
    var pair = pickDashPair(sd.adaptiveFormats);
    if (!pair) { return null; }
    var len = 0;
    if (obj.videoDetails && obj.videoDetails.lengthSeconds) { len = Number(obj.videoDetails.lengthSeconds) || 0; }
    return { video: pair.video, audio: pair.audio, durationSec: len };
  }).catch(function () { return null; });
}
function resolveViaAndroid(videoId) {
  var lastErr = 'no muxed mp4';
  var chain = Promise.resolve();
  var tried = 0;
  function attempt(version) {
    return function () {
      tried += 1;
      var url = 'https://www.youtube.com/youtubei/v1/player?key=' + INNERTUBE_KEY + '&prettyPrint=false';
      var body = JSON.stringify({
        context: { client: { clientName: 'ANDROID', clientVersion: version, androidVersion: 16, hl: 'en', gl: 'US' } },
        videoId: videoId
      });
      var headers = {
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(body),
        'User-Agent': 'com.google.android.youtube/' + version + '(Linux; U; Android 16; en_US; SM-S908E Build/TP1A.220624.014) gzip'
      };
      return httpsPostJson(url, body, headers, 4500).then(function (text) {
        var obj;
        try { obj = JSON.parse(text); } catch (e) { lastErr = 'player json parse failed'; return null; }
        notePlayability(obj);
        var sd = obj.streamingData || {};
        var best = pickMuxed(sd.formats);
        if (best) { return best; }
        var hasCipher = false;
        var fmts = sd.formats || [];
        for (var i = 0; i < fmts.length; i++) {
          if (fmts[i] && fmts[i].signatureCipher) { hasCipher = true; break; }
        }
        lastErr = hasCipher ? 'signatureCipher only (decipher fallback not run)' : 'no muxed mp4 (formats=' + fmts.length + ')';
        return null;
      }).catch(function (e) {
        lastErr = String(e && e.message || e).slice(0, 80);
        return null;
      });
    };
  }
  for (var i = 0; i < ANDROID_VERSIONS.length; i++) {
    (function (ver) {
      chain = chain.then(function (prev) {
        if (prev) { return prev; }
        return attempt(ver)();
      });
    })(ANDROID_VERSIONS[i]);
  }
  return chain.then(function (best) {
    return { best: best, err: lastErr };
  });
}
readStdin().then(function (stdinText) {
  var id = pickId(process.argv, stdinText);
  if (!id) { fail('missing youtubeId'); return; }
  if (!(id.length >= 6 && id.length <= 64 && /^[A-Za-z0-9_-]+$/.test(id))) { fail('invalid youtubeId'); return; }
  // VISIONOS adaptive first (the only client served the whole track), then IOS
  // adaptive, then the ANDROID muxed file. Best available wins: DASH HD where
  // it is granted, the 360p muxed file always plays.
  Promise.all([resolveViaVisionos(id), resolveViaIos(id), resolveViaAndroid(id)]).then(function (results) {
    var dash = results[0] || results[1];
    var android = results[2];
    if (dash) {
      var out = { ok: true, kind: 'dash', video: dash.video, audio: dash.audio, durationSec: dash.durationSec };
      if (android.best) {
        out.muxed = { url: android.best.url, itag: android.best.itag, height: android.best.height };
      }
      process.stdout.write(JSON.stringify(out) + '\n');
      process.exit(0);
      return;
    }
    if (android.best) {
      try {
        process.stdout.write(JSON.stringify({
          ok: true, kind: 'muxed',
          url: android.best.url,
          itag: android.best.itag,
          height: android.best.height,
          container: 'mp4'
        }) + '\n');
      } catch (e) { fail('output failed'); return; }
      process.exit(0);
      return;
    }
    // `LOGIN_REQUIRED` is permanent for anonymous playback: report it as its
    // own reason so the shim can skip this id and take the title's next
    // trailer instead of re-spawning for the same refusal.
    fail(LOGIN_REQUIRED && !android.best ? 'login_required' : (android.err || 'no dash and no muxed mp4'));
  }).catch(function () { fail('resolve failed'); });
}).catch(function () { fail('resolve failed'); });
