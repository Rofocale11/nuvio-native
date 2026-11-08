// libsbengine: the torrent engine (GPL-3.0, libtorrent 2.0 underneath) behind a small C interface
// that never blocks (src/core/sbengine.h): the app adds a magnet, then asks each frame for the
// files, the stream address (the engine serves the file itself on 127.0.0.1, Range included) and
// the transfer numbers. Only sbe_get is exported; libc++ is linked in and hidden.
#include "nuvio_engine/nuvio_engine.h"
#include "core/sbengine.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace {
struct Stream { std::string id, url, error; std::uint64_t request = 0; bool ready = false; };
struct Torrent { std::string engineId, error; std::uint64_t addRequest = 0; bool metadata = false; bool restored = false; std::vector<sbe_file> files; std::map<std::uint32_t, Stream> streams; };

nuvio_engine *engine = nullptr;
std::mutex mu;
std::map<std::string, Torrent> torrents;          // info hash -> torrent
std::map<std::uint64_t, std::string> addRequests;  // add request -> info hash
std::atomic_bool stopping{false};
std::thread events;
std::string lastError;

std::string lower(std::string s) { for (auto &c : s) c = (char)std::tolower((unsigned char)c); return s; }
Torrent *by_engine_id(const std::string &id) {
  for (auto &kv : torrents) if (kv.second.engineId == id) return &kv.second;
  auto it = torrents.find(lower(id).substr(0, 40));
  return it == torrents.end() ? nullptr : &it->second;
}
void refresh_files(Torrent &t) {
  std::size_t n = 0;
  if (nuvio_engine_get_file_count(engine, t.engineId.c_str(), &n) != NUVIO_ENGINE_STATUS_OK) return;
  t.files.clear();
  for (std::size_t i = 0; i < n; ++i) {
    nuvio_engine_file f; nuvio_engine_file_init(&f);
    if (nuvio_engine_get_file(engine, t.engineId.c_str(), i, &f) != NUVIO_ENGINE_STATUS_OK) break;
    f.path[sizeof f.path - 1] = 0;
    sbe_file o; std::snprintf(o.path, sizeof o.path, "%s", f.path); o.size = f.size; t.files.push_back(o);
  }
  t.metadata = !t.files.empty();
}
void event_loop() {
  while (!stopping.load()) {
    nuvio_engine_event e; nuvio_engine_event_init(&e);
    if (nuvio_engine_poll_event(engine, &e) != NUVIO_ENGINE_STATUS_OK) { std::this_thread::sleep_for(std::chrono::milliseconds(30)); continue; } // the app looks once a frame at most
    std::lock_guard<std::mutex> lock(mu);
    e.torrent_id[sizeof e.torrent_id - 1] = 0; e.message[sizeof e.message - 1] = 0; e.stream_id[sizeof e.stream_id - 1] = 0; e.stream_url[sizeof e.stream_url - 1] = 0;
    std::string tid = e.torrent_id;
    Torrent *t = nullptr;
    auto r = addRequests.find(e.request_id);
    switch (e.type) {
    case NUVIO_ENGINE_EVENT_TORRENT_ADDED: if (r != addRequests.end()) torrents[r->second].engineId = tid; break;
    case NUVIO_ENGINE_EVENT_TORRENT_METADATA_READY:
      t = by_engine_id(tid); if (!t && r != addRequests.end()) t = &torrents[r->second];
      if (t) {
        if (t->engineId.empty()) t->engineId = tid;
        // The message carries "metadata_source=restored" when the engine loaded
        // a kept resume file instead of doing a cold metadata fetch. The app
        // exposes nothing else that says so, and the 1 MiB prebuffer gate must
        // not fire for a restored swarm (the swarm is already known and the
        // probe must not wait for it to re-earn the gate).
        t->restored = std::strstr(e.message, "metadata_source=restored") != nullptr;
        refresh_files(*t);
      }
      break;
    case NUVIO_ENGINE_EVENT_STREAM_PREPARED:
      for (auto &kv : torrents) for (auto &s : kv.second.streams) if (s.second.request == e.request_id) { s.second.id = e.stream_id; s.second.url = e.stream_url; s.second.ready = true; }
      break;
    case NUVIO_ENGINE_EVENT_STREAM_STOPPED:
      for (auto &kv : torrents) for (auto it = kv.second.streams.begin(); it != kv.second.streams.end();) { if (it->second.id == e.stream_id) it = kv.second.streams.erase(it); else ++it; }
      break;
    case NUVIO_ENGINE_EVENT_TORRENT_ERROR: {
      { static std::string last; static auto lastT = std::chrono::steady_clock::time_point{}; auto now = std::chrono::steady_clock::now();
        if (last != e.message || now - lastT > std::chrono::seconds(10)) { last = e.message; lastT = now; // into the app's log (a storage error, a move that failed), once per 10 s when it repeats
          std::fprintf(stderr, "[p2p] engine: %s%s%s\n", tid.c_str(), tid.empty() ? "" : ": ", e.message); } }
      t = by_engine_id(tid); if (!t && r != addRequests.end()) t = &torrents[r->second];
      if (t) { bool streamErr = false; for (auto &s : t->streams) if (s.second.request == e.request_id && !s.second.ready) { s.second.error = e.message; streamErr = true; } if (!streamErr) t->error = e.message; }
      break; }
    default: break;
    }
  }
}

int api() { return SBE_API_VERSION; }
const char *version() { static char v[128]; std::snprintf(v, sizeof v, "%s / libtorrent %s", nuvio_engine_version_string(), nuvio_engine_protocol_backend_version()); return v; }
int start(const char *stateDir, const char *cacheDir, uint64_t windowBytes, uint64_t uploadBps, uint64_t memCacheBytes, uint64_t segmentBytes) {
  if (engine) return 0;
  nuvio_engine_config c; nuvio_engine_config_init(&c);
  c.data_directory = stateDir; c.cache_directory = cacheDir;
  c.memory_cache_capacity_bytes = memCacheBytes;           // verified pieces kept in RAM (sized from the TV's memory by the app)
  c.disk_cache_capacity_bytes = windowBytes; c.streaming_window_bytes = windowBytes; // bounded payload: a window around the playhead
  c.streaming_segment_bytes = segmentBytes;                 // FAT32: segment files instead of holes
  c.upload_mode = NUVIO_ENGINE_UPLOAD_LIMITED; c.upload_limit_bytes_per_second = uploadBps;
  c.torrent_profile = NUVIO_ENGINE_TORRENT_PROFILE_SOFT;   // 35 peers, 35 connects a second: kind to a TV's CPU and network
  // no idle timers: a stream paused for a while must still be there; the app decides when it ends
  c.stream_inactivity_timeout_milliseconds = 0; c.warm_torrent_timeout_milliseconds = 0;
  nuvio_engine_status st = nuvio_engine_create(&c, &engine);
  if (st != NUVIO_ENGINE_STATUS_OK || !engine) { lastError = nuvio_engine_status_message(st); engine = nullptr; return -1; }
  stopping = false; events = std::thread(event_loop);
  return 0;
}
void stop() {
  if (!engine) return;
  stopping = true; if (events.joinable()) events.join();
  nuvio_engine_destroy(engine); engine = nullptr;
  std::lock_guard<std::mutex> lock(mu); torrents.clear(); addRequests.clear();
}
int open(const char *magnet, const char *ih) {
  if (!engine) return -1;
  std::lock_guard<std::mutex> lock(mu);
  std::string key = lower(ih);
  auto it = torrents.find(key);
  if (it != torrents.end() && it->second.error.empty()) return 0;   // added already (or being added)
  nuvio_engine_torrent_request q; nuvio_engine_torrent_request_init(&q); q.magnet_uri = magnet; q.source_type = NUVIO_ENGINE_TORRENT_SOURCE_MAGNET;
  std::uint64_t rid = 0;
  nuvio_engine_status st = nuvio_engine_add_torrent(engine, &q, &rid);
  if (st != NUVIO_ENGINE_STATUS_OK) { lastError = nuvio_engine_status_message(st); return -1; }
  Torrent t; t.addRequest = rid; torrents[key] = t; addRequests[rid] = key;
  return 0;
}
int files(const char *ih, sbe_file *out, int max) {
  std::lock_guard<std::mutex> lock(mu);
  auto it = torrents.find(lower(ih)); if (it == torrents.end()) return -2;
  if (!it->second.error.empty()) return -2;
  if (!it->second.metadata) return -1;
  int n = 0; for (auto &f : it->second.files) { if (n < max) out[n] = f; n++; }
  return n;
}
int restored(const char *ih) {
  std::lock_guard<std::mutex> lock(mu);
  auto it = torrents.find(lower(ih));
  return it != torrents.end() && it->second.restored ? 1 : 0;
}
int stream(const char *ih, uint32_t idx, char *url, int cap) {
  std::lock_guard<std::mutex> lock(mu);
  auto it = torrents.find(lower(ih)); if (it == torrents.end() || !engine) return -1;
  Torrent &t = it->second;
  if (!t.metadata || idx >= t.files.size()) return t.error.empty() && !t.metadata ? 0 : -1;
  auto s = t.streams.find(idx);
  if (s == t.streams.end()) {
    nuvio_engine_stream_request q; nuvio_engine_stream_request_init(&q); q.torrent_id = t.engineId.c_str(); q.file_index = idx;
    std::uint64_t rid = 0;
    if (nuvio_engine_prepare_stream(engine, &q, &rid) != NUVIO_ENGINE_STATUS_OK) return -1;
    Stream e; e.request = rid; t.streams[idx] = e; return 0;
  }
  if (!s->second.error.empty()) { t.error = s->second.error; return -1; }
  if (!s->second.ready) return 0;
  std::snprintf(url, cap, "%s", s->second.url.c_str()); return 1;
}
const char *error(const char *ih) {
  static std::string e; std::lock_guard<std::mutex> lock(mu);
  auto it = ih ? torrents.find(lower(ih)) : torrents.end();
  e = it != torrents.end() && !it->second.error.empty() ? it->second.error : lastError; return e.c_str();
}
int stats(sbe_stats *o) {
  if (!engine) return -1;
  nuvio_engine_stats st; nuvio_engine_stats_init(&st);
  if (nuvio_engine_get_stats(engine, &st) != NUVIO_ENGINE_STATUS_OK) return -1;
  o->peers = st.connected_peers; o->seeds = st.connected_seeds; o->known = st.known_peers; o->connecting = st.connecting_peers;
  o->down_bps = st.download_rate_bytes_per_second; o->up_bps = st.upload_rate_bytes_per_second; o->downloaded = st.total_payload_download_bytes;
  o->disk_used = st.disk_cache_used_bytes; o->disk_capacity = st.disk_cache_capacity_bytes;
  o->window = st.window_capacity_bytes; o->resident = st.window_resident_bytes; o->freed = st.window_freed_bytes; o->refetched = st.window_refetched_pieces;
  o->mem_cache_used = st.memory_cache_used_bytes; o->window_unsupported = st.window_unsupported;
  return 0;
}
void window(uint64_t bytes) { if (engine) nuvio_engine_set_streaming_window(engine, bytes); }
int relocate(const char *dir, uint64_t segmentBytes) { return engine && dir && nuvio_engine_relocate(engine, dir, segmentBytes) == NUVIO_ENGINE_STATUS_OK ? 0 : -1; }
int setting(const char *name, int value) { return engine && name && nuvio_engine_apply_setting(engine, name, value) == NUVIO_ENGINE_STATUS_OK ? 0 : -1; }
void limits(uint64_t down, uint64_t up) { if (engine) nuvio_engine_set_rate_limits(engine, down, up); }
void remove_one(const char *ih) {
  std::lock_guard<std::mutex> lock(mu);
  auto it = torrents.find(lower(ih)); if (it == torrents.end()) return;
  if (engine && !it->second.engineId.empty()) { std::uint64_t rid = 0; nuvio_engine_remove_torrent(engine, it->second.engineId.c_str(), &rid); }
  addRequests.erase(it->second.addRequest); torrents.erase(it);
}
void remove_all() {
  std::vector<std::string> keys; { std::lock_guard<std::mutex> lock(mu); for (auto &kv : torrents) keys.push_back(kv.first); }
  for (auto &k : keys) remove_one(k.c_str());
}
// the process ending with the engine still on (an exit path that skipped stop): join its threads first, a
// running std::thread destroyed at exit would abort
struct StopAtExit { ~StopAtExit() { stop(); } } stopAtExit;
} // namespace

extern "C" __attribute__((visibility("default"))) const sbe_api *sbe_get(void) {
  static const sbe_api a = { api, version, start, stop, open, files, stream, error, stats, limits, remove_one, remove_all, window, setting, relocate, restored };
  return &a;
}
