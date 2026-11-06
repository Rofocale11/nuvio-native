#!/usr/bin/env node
'use strict';
/*
 * Trailer DASH track proxy child (REAL).
 *
 * The trailer DASH pair is deciphered for the IOS client (`trailer-decipher.js`
 * `resolveViaIos`: `com.google.ios.youtube/21.03.36`), whose signed googlevideo
 * URLs are bound to that user agent: the same URL fetched with the app's own
 * `PlxNative` UA answers `403`, while the ANDROID muxed URL is looser and plays
 * wherever it lands. This child is the one place that re-attaches the IOS UA
 * for track bytes: the shim registers a track, the DASH manifest's BaseURLs
 * point at this loopback server, and the player opens the loopback URL like any
 * other seekable progressive source. The shim never fetches googlevideo with
 * the wrong UA, and the media layer never learns a UA at all (`net::apply_user_agent`
 * contract untouched).
 *
 * Interface (single long-lived child, `127.0.0.1` ephemeral port):
 *   stdout (first line only): {"port":N,"node":"vX.Y.Z"}
 *   POST /register   {"u":"https://…googlevideo…","ua":"ios"|"android"}
 *                    -> 200 {"id":N}
 *   GET  /track/<id> -> the upstream googlevideo bytes for track <id>, status,
 *                    Content-Length / Content-Range / Content-Type forwarded,
 *                    `Range` forwarded verbatim. Redirects (max 5, each hop
 *                    re-validated) followed with UA + Range preserved.
 *   GET  /health     -> 200 {"ok":true}
 *
 * Rules: only `https://` URLs whose hostname is `googlevideo.com` or ends with
 * `.googlevideo.com` are registered and fetched; the registered URL and every
 * upstream header are never written to stdout/stderr (only id + status). The
 * signed URL lives in memory only (`id -> {u, ua}`, FIFO at 64 entries) so it
 * never appears in the MPD either — the manifest carries `http://127.0.0.1:…/
 * track/<id>` opaque ids. Builtins only (`http`/`https`/`fs`), ES2020 syntax
 * for the Node `v16.20.2` floor, no global `fetch`, no npm deps, no disk write.
 */
var http = require('http');
var https = require('https');
var fs = require('fs');

var IOS_UA = 'com.google.ios.youtube/21.03.36(iPhone16,2; U; CPU iPhone OS 18_0 like Mac OS X)';
var ANDROID_UA = 'com.google.android.youtube/21.03.36(Linux; U; Android 16; en_US; SM-S908E Build/TP1A.220624.014) gzip';
var MAX_REGISTRATIONS = 64;
var MAX_REDIRECTS = 5;

var nextId = 0;
var tracks = new Map(); // insertion-ordered: id -> {u, ua}
var order = [];

function isGooglevideo(urlStr) {
  var u;
  try { u = new URL(urlStr); } catch (e) { return null; }
  if (u.protocol !== 'https:') { return null; }
  var host = (u.hostname || '').toLowerCase();
  if (host !== 'googlevideo.com' && !host.endsWith('.googlevideo.com')) { return null; }
  return u;
}

function errorBody(res, status, reason) {
  var body = JSON.stringify({ ok: false, error: reason });
  res.writeHead(status, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) });
  res.end(body);
}

function register(req, res) {
  var chunks = [];
  req.on('data', function (c) { chunks.push(c); });
  req.on('end', function () {
    var obj;
    try { obj = JSON.parse(Buffer.concat(chunks).toString('utf8')); } catch (e) {
      errorBody(res, 400, 'bad json'); return;
    }
    var u = isGooglevideo(obj.u);
    if (!u) { errorBody(res, 400, 'only https googlevideo urls'); return; }
    var ua = obj.ua;
    if (ua !== 'ios' && ua !== 'android') { errorBody(res, 400, 'ua must be ios or android'); return; }
    nextId += 1;
    var id = nextId;
    tracks.set(id, { u: obj.u, ua: ua });
    order.push(id);
    while (order.length > MAX_REGISTRATIONS) {
      var old = order.shift();
      if (old !== id) { tracks.delete(old); }
    }
    var body = JSON.stringify({ ok: true, id: id });
    res.writeHead(200, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) });
    res.end(body);
  });
  req.on('error', function () { try { res.destroy(); } catch (e) {} });
}

function forwardHeaders(upstream) {
  var out = {};
  ['content-length', 'content-range', 'accept-ranges', 'content-type', 'content-encoding'].forEach(function (name) {
    var value = upstream.headers[name];
    if (value !== undefined) { out[name] = value; }
  });
  return out;
}

function fetchUpstream(urlStr, uaName, range, hopsLeft, res, id, triedAlt) {
  var ua = uaName === 'android' ? ANDROID_UA : IOS_UA;
  var u = isGooglevideo(urlStr);
  if (!u) { errorBody(res, 400, 'bad upstream'); return; }
  // Referer + Accept-Language mirror the iOS app; family 4 keeps the fetch
  // on the same IPv4 egress the decipher's youtubei POST saw (an IPv6 edge
  // with an IPv4 `ip=` param looks like a mismatch to the edge validator).
  // A 403 retries once with the alternate UA: some edges accept the IOS
  // signature with either UA, so the stranded-403 case costs one extra
  // loopback round-trip, not a missed trailer.
  var headers = { 'User-Agent': ua, 'Accept': '*/*', 'Accept-Language': 'en-US,en;q=0.9', 'Referer': 'https://www.youtube.com/' };
  if (range) { headers['Range'] = range; }
  var upReq = https.request(
    { hostname: u.hostname, path: u.pathname + u.search, port: 443, method: 'GET', headers: headers, family: 4, timeout: 15000 },
    function (upRes) {
      var status = upRes.statusCode || 502;
      var location = upRes.headers['location'];
      var isRedirect = status === 301 || status === 302 || status === 303 || status === 307 || status === 308;
      if (isRedirect && location && hopsLeft > 0) {
        upRes.resume();
        var next;
        try { next = new URL(location, urlStr).toString(); } catch (e) { next = null; }
        if (!next || !isGooglevideo(next)) { errorBody(res, 400, 'bad redirect'); return; }
        fetchUpstream(next, uaName, range, hopsLeft - 1, res, id, triedAlt);
        return;
      }
      if (status === 403 && !triedAlt) {
        upRes.resume();
        var alt = uaName === 'android' ? 'ios' : 'android';
        fetchUpstream(urlStr, alt, range, hopsLeft, res, id, true);
        return;
      }
      res.writeHead(status, forwardHeaders(upRes));
      upRes.pipe(res);
    }
  );
  upReq.on('timeout', function () { try { upReq.destroy(); } catch (e) {} });
  upReq.on('error', function () {
    if (!res.headersSent) { errorBody(res, 502, 'upstream error id=' + id); }
    else { try { res.destroy(); } catch (e) {} }
  });
  res.on('close', function () { try { upReq.destroy(); } catch (e) {} });
  upReq.end();
}

function serveTrack(req, res, idParam) {
  var id = parseInt(idParam, 10);
  var track = tracks.get(id);
  if (!track) { errorBody(res, 404, 'unknown track'); return; }
  fetchUpstream(track.u, track.ua, req.headers['range'] || null, MAX_REDIRECTS, res, id);
}

var server = http.createServer(function (req, res) {
  var url = req.url || '';
  if (req.method === 'POST' && url === '/register') { register(req, res); return; }
  if (req.method === 'GET' && url === '/health') {
    var body = JSON.stringify({ ok: true });
    res.writeHead(200, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) });
    res.end(body);
    return;
  }
  if (req.method === 'GET') {
    var m = url.match(/^\/track\/(\d+)$/);
    if (m) { serveTrack(req, res, m[1]); return; }
  }
  errorBody(res, 404, 'not found');
});

server.listen(0, '127.0.0.1', function () {
  var addr = server.address();
  var line = JSON.stringify({ port: addr.port, node: process.version });
  try { fs.writeSync(1, line + '\n'); } catch (e) {}
});
