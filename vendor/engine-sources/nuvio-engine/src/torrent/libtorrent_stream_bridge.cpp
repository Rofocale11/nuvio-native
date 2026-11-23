#include "torrent/libtorrent_stream_bridge.hpp"

#include "cache/verified_piece_cache.hpp"
#include "http/loopback_server.hpp"
#include "nuvio_engine/piece_scheduler.hpp"
#include "scheduler/demand_window.hpp"
#include "scheduler/stream_demand_plan.hpp"
#include "security/random_bytes.hpp"
#include "storage/payload_reader.hpp"

#include <algorithm>
#include <cerrno>
#include <atomic>
#include <chrono>
#include <cctype>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <deque>
#include <filesystem>
#include <functional>
#include <limits>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

#include <libtorrent/alert_types.hpp>
#include <libtorrent/download_priority.hpp>
#include <libtorrent/hex.hpp>
#include <libtorrent/torrent_handle.hpp>
#include <libtorrent/torrent_info.hpp>

namespace nuvio::torrent {
namespace {

constexpr std::size_t maximum_http_commands = 4096;
constexpr std::size_t stream_worker_count = 8;
constexpr std::chrono::seconds piece_wait_timeout{60};
constexpr std::uint64_t critical_window_bytes = 8 * 1024 * 1024;
constexpr std::uint32_t critical_piece_limit = 16;
constexpr std::uint64_t playback_window_bytes = 32 * 1024 * 1024;
constexpr std::uint64_t minimum_readahead_bytes = 64 * 1024 * 1024;
constexpr std::uint64_t maximum_readahead_bytes = 256 * 1024 * 1024;
constexpr std::uint64_t metadata_tail_bytes = 2 * 1024 * 1024;
constexpr std::uint64_t block_bytes = 16 * 1024;

std::uint64_t readahead_for_disk_budget(const std::uint64_t disk_capacity_bytes) {
    return std::clamp(
        disk_capacity_bytes / 4,
        minimum_readahead_bytes,
        maximum_readahead_bytes
    );
}

// Streaming window (#334). Bytes kept on disk while a file plays are:
// protected head + protected tail + what stays behind the playhead + the
// read-ahead in front of it. The window trims at high_water and stops at
// low_water, so the read-ahead must fit under low_water with the rest.
// Stremio Origin: every part scales with the window (TVs with a few dozen MB of
// storage): at 160 MB and more it is the original split; at 48 MB it is a
// 3 MB head, 6 MB tail, 6 MB behind, 6 MB critical and ~11 MB in front.
constexpr std::uint64_t mib = 1024 * 1024;
constexpr std::chrono::milliseconds window_check_interval{100};
// verified bytes are read from the payload file in pieces of this size (a
// whole 16 MB piece in one buffer per HTTP worker was too much for a TV)
constexpr std::uint64_t payload_read_chunk_bytes = 256 * 1024;

std::uint64_t window_low_water(const std::uint64_t window_bytes) {
    return window_bytes / 4 * 3;
}

std::uint64_t window_high_water(const std::uint64_t window_bytes) {
    return window_bytes / 20 * 17;
}

std::uint64_t window_head_bytes(const std::uint64_t window_bytes) {
    return std::clamp(window_bytes / 16, 2 * mib, 8 * mib);
}

std::uint64_t window_tail_bytes(const std::uint64_t window_bytes) {
    return std::clamp(window_bytes / 8, 4 * mib, 16 * mib);
}

std::uint64_t window_behind_bytes(const std::uint64_t window_bytes) {
    return std::clamp(window_bytes / 8, 4 * mib, 64 * mib);
}

std::uint64_t window_critical_bytes(const std::uint64_t window_bytes) {
    return std::clamp(window_bytes / 8, 4 * mib, critical_window_bytes);
}

// playback + read-ahead in front of the critical bytes
std::uint64_t window_front_bytes(const std::uint64_t window_bytes) {
    const auto fixed = window_head_bytes(window_bytes) + window_tail_bytes(window_bytes) +
        window_behind_bytes(window_bytes) + window_critical_bytes(window_bytes);
    const auto low = window_low_water(window_bytes);
    const auto available = low > fixed ? low - fixed : 0;
    return std::clamp(available / 4 * 3, 4 * mib, maximum_readahead_bytes);
}

scheduler::StreamWindow make_stream_window(
    const std::uint64_t disk_capacity_bytes,
    const std::uint64_t window_bytes
) {
    if (window_bytes == 0) {
        return {
            critical_window_bytes,
            critical_piece_limit,
            playback_window_bytes,
            readahead_for_disk_budget(disk_capacity_bytes) - playback_window_bytes,
            metadata_tail_bytes,
        };
    }
    const auto front = std::min(
        window_front_bytes(window_bytes),
        readahead_for_disk_budget(disk_capacity_bytes)
    );
    const auto playback = std::min(playback_window_bytes, std::max(front / 3 * 2, 4 * mib));
    return {
        window_critical_bytes(window_bytes),
        critical_piece_limit,
        std::min(playback, front),
        front > playback ? front - playback : 0,
        std::min(metadata_tail_bytes, window_tail_bytes(window_bytes)),
    };
}

std::string torrent_id(const lt::torrent_handle& handle) {
    const auto hashes = handle.info_hashes();
    if (hashes.has_v1()) {
        return lt::aux::to_hex(hashes.v1);
    }
    if (hashes.has_v2()) {
        return lt::aux::to_hex(hashes.v2);
    }
    return {};
}

std::string content_type_for_path(const std::string& path) {
    std::string normalized = path;
    std::ranges::transform(normalized, normalized.begin(), [](const unsigned char character) {
        return static_cast<char>(std::tolower(character));
    });
    if (normalized.ends_with(".mp4") || normalized.ends_with(".m4v")) {
        return "video/mp4";
    }
    if (normalized.ends_with(".mkv")) {
        return "video/x-matroska";
    }
    if (normalized.ends_with(".webm")) {
        return "video/webm";
    }
    if (normalized.ends_with(".avi")) {
        return "video/x-msvideo";
    }
    if (normalized.ends_with(".ts") || normalized.ends_with(".mpeg") ||
        normalized.ends_with(".mpg")) {
        return "video/mp2t";
    }
    if (normalized.ends_with(".mov")) {
        return "video/quicktime";
    }
    if (normalized.ends_with(".mp3")) {
        return "audio/mpeg";
    }
    if (normalized.ends_with(".flac")) {
        return "audio/flac";
    }
    return "application/octet-stream";
}

lt::download_priority_t download_priority_for(
    const scheduler::PriorityClass priority,
    const bool cold
) {
    switch (priority) {
    case scheduler::PriorityClass::blocking:
        return lt::top_priority;
    case scheduler::PriorityClass::critical:
        return lt::top_priority;
    case scheduler::PriorityClass::playback:
        return lt::default_priority;
    case scheduler::PriorityClass::metadata_tail:
        return cold ? lt::download_priority_t{6} : lt::top_priority;
    case scheduler::PriorityClass::readahead:
        return lt::low_priority;
    }
    return lt::dont_download;
}

int deadline_for(const std::size_t ordinal) {
    const auto increment = std::min<std::size_t>(ordinal * 100, 2000);
    return static_cast<int>(increment);
}

}

struct LibtorrentStreamBridge::Impl {
    struct StreamCounters {
        std::atomic_uint64_t delivered_bytes = 0;
    };

    struct StreamActivity {
        using Clock = std::chrono::steady_clock;

        explicit StreamActivity(const Clock::time_point now)
            : last_activity(now) {
        }

        void acquire_http_request() noexcept {
            std::lock_guard lock(mutex);
            ++active_http_requests;
        }

        void release_http_request() noexcept {
            std::lock_guard lock(mutex);
            if (active_http_requests == 0) {
                return;
            }
            --active_http_requests;
            if (active_http_requests == 0) {
                last_activity = Clock::now();
            }
        }

        [[nodiscard]] bool is_inactive(
            const Clock::time_point now,
            const std::chrono::milliseconds timeout
        ) const noexcept {
            std::lock_guard lock(mutex);
            return active_http_requests == 0 && now - last_activity >= timeout;
        }

        mutable std::mutex mutex;
        std::uint32_t active_http_requests = 0;
        Clock::time_point last_activity;
    };

    struct HttpRequestLease {
        explicit HttpRequestLease(std::shared_ptr<StreamActivity> activity)
            : activity_(std::move(activity)) {
        }

        ~HttpRequestLease() {
            activity_->release_http_request();
        }

        HttpRequestLease(const HttpRequestLease&) = delete;
        HttpRequestLease& operator=(const HttpRequestLease&) = delete;

    private:
        std::shared_ptr<StreamActivity> activity_;
    };

    static void add_delivered_bytes(
        const std::shared_ptr<StreamCounters>& counters,
        const std::uint64_t bytes
    ) {
        auto current = counters->delivered_bytes.load(std::memory_order_relaxed);
        while (true) {
            const auto next = bytes > std::numeric_limits<std::uint64_t>::max() - current
                ? std::numeric_limits<std::uint64_t>::max()
                : current + bytes;
            if (counters->delivered_bytes.compare_exchange_weak(
                    current,
                    next,
                    std::memory_order_relaxed
                )) {
                return;
            }
        }
    }

    struct StreamRoute {
        lt::torrent_handle handle;
        std::string torrent_id;
        std::string stream_id;
        std::string target;
        std::string content_type;
        std::uint32_t file_index = 0;
        std::uint32_t piece_size = 0;
        std::uint64_t file_offset = 0;
        std::uint64_t file_size = 0;
        std::filesystem::path payload_path;
        std::shared_ptr<StreamActivity> activity;
        // what the disk back-end knows the torrent by (info_hash_t::get_best)
        std::string window_key;
        std::shared_ptr<StreamCounters> counters = std::make_shared<StreamCounters>();
    };

    using PieceKey = cache::PieceCacheKey;
    using PieceKeyHash = cache::PieceCacheKeyHash;

    struct PieceWaiter {
        std::mutex mutex;
        std::condition_variable ready;
        std::shared_ptr<std::vector<char>> data;
        std::string error;
        bool done = false;
        bool cancelled = false;
        std::uint64_t demand_id = 0;
        std::vector<bool> finished_blocks;
    };

    struct DemandState {
        std::atomic_bool cancelled = false;
    };

    enum class CommandType {
        begin_demand,
        end_demand,
        read_piece,
    };

    struct HttpCommand {
        CommandType type;
        std::uint64_t demand_id = 0;
        StreamRoute route;
        http::ByteRange range{};
        std::uint32_t piece = 0;
        std::shared_ptr<PieceWaiter> waiter;
        std::shared_ptr<DemandState> demand_state;
    };

    struct ActiveDemand {
        std::uint64_t id = 0;
        StreamRoute route;
        http::ByteRange range{};
        std::shared_ptr<DemandState> state;
        bool needs_data = true;
        // Monotonic "last served" ordering. The schedule is built around the demand the player is
        // reading NOW, which is the one whose last `read_piece` is newest — not the newest request
        // and not whichever one happened to miss a piece. A request the player has abandoned (a
        // closed seek, a probe) stops being served and must not drag the download with it.
        std::uint64_t activity = 0;
        bool served = false;
    };

    struct StreamAnchor {
        StreamRoute route;
        http::ByteRange range{};
        bool prefetch = false;
    };

    struct CurrentSchedule {
        lt::torrent_handle handle;
        std::map<std::uint32_t, scheduler::PriorityClass> priorities;
        std::vector<std::uint32_t> deadline_order;
        std::uint64_t focused_demand_id = 0;
        std::uint64_t revision = 0;
        bool cold = false;
        std::map<std::uint32_t, int> first_blocks;
    };

    struct DemandGuard {
        Impl* owner;
        std::uint64_t demand_id;

        ~DemandGuard() {
            try {
                owner->enqueue_end(demand_id);
            } catch (...) {
            }
        }
    };

    static http::ByteRange bounded_demand_range(
        const StreamRoute& route,
        const http::ByteRange response_range,
        const std::uint64_t position
    ) {
        return scheduler::blocking_demand_range(
            response_range,
            position,
            route.file_offset,
            route.piece_size
        );
    }

    Impl(
        const std::uint16_t requested_port,
        const std::uint64_t memory_capacity_bytes,
        const std::uint64_t disk_capacity_bytes,
        const std::chrono::milliseconds inactivity,
        const std::uint64_t streaming_window_bytes,
        std::shared_ptr<storage::StreamingWindowStore> streaming_window_store
    )
        : window(make_stream_window(
              disk_capacity_bytes,
              streaming_window_store ? streaming_window_bytes : 0
          )),
          disk_capacity(disk_capacity_bytes),
          inactivity_timeout(inactivity),
          piece_cache(memory_capacity_bytes),
          server(std::make_unique<http::LoopbackServer>(
              requested_port,
              [this](
                  const http::HttpRequest& request,
                  const http::LoopbackServer::Writer& writer,
                  const std::stop_token token
              ) {
                  handle_http(request, writer, token);
              },
              stream_worker_count
          )),
          window_bytes(streaming_window_store ? streaming_window_bytes : 0),
          window_store(std::move(streaming_window_store)) {
    }

    ~Impl() {
        try {
            shutdown();
        } catch (...) {
        }
    }

    PreparedStream register_stream(
        const lt::torrent_handle& handle,
        const std::string& id,
        const std::uint32_t file_index,
        const TorrentFileInfo& file
    ) {
        if (shutting_down.load()) {
            throw std::runtime_error("stream bridge is shutting down");
        }
        const auto metadata = handle.torrent_file();
        if (!metadata || metadata->piece_length() <= 0) {
            throw std::runtime_error("torrent piece metadata is unavailable");
        }
        StreamRoute route{
            handle,
            id,
            security::random_hex_token(32),
            {},
            content_type_for_path(file.path),
            file_index,
            static_cast<std::uint32_t>(metadata->piece_length()),
            file.offset,
            file.size,
            std::filesystem::path(metadata->files().file_path(
                lt::file_index_t(static_cast<int>(file_index)),
                handle.status(lt::torrent_handle::query_save_path).save_path
            )),
            std::make_shared<StreamActivity>(std::chrono::steady_clock::now()),
            lt::aux::to_hex(handle.info_hashes().get_best()),
        };
        route.target = "/stream/" + route.stream_id;
        {
            std::lock_guard lock(routes_mutex);
            save_paths.insert_or_assign(id, std::filesystem::path(
                handle.status(lt::torrent_handle::query_save_path).save_path
            ));
            relative_paths.insert_or_assign(id + "/" + std::to_string(file_index),
                std::filesystem::path(metadata->files().file_path(lt::file_index_t(static_cast<int>(file_index)))));
        }
        std::vector<std::string> previous_streams;
        {
            std::lock_guard lock(routes_mutex);
            for (const auto& [target, existing] : routes) {
                static_cast<void>(target);
                if (existing.torrent_id == id) {
                    previous_streams.push_back(existing.stream_id);
                }
            }
        }
        for (const auto& stream_id : previous_streams) {
            static_cast<void>(stop_stream(stream_id));
        }
        {
            std::lock_guard lock(routes_mutex);
            routes.insert_or_assign(route.target, route);
        }
        next_progress_sample = {};
        last_ready_pieces.erase(id);
        revoked_torrents.erase(id);
        if (route.file_size > 0) {
            anchors.insert_or_assign(id, StreamAnchor{
                route,
                bounded_demand_range(route, {0, route.file_size - 1}, 0),
                true,
            });
            recompute_schedule(id);
        }
        return {
            route.stream_id,
            "http://127.0.0.1:" + std::to_string(server->port()) + route.target,
        };
    }

    void handle_http(
        const http::HttpRequest& request,
        const http::LoopbackServer::Writer& writer,
        const std::stop_token token
    ) {
        StreamRoute route;
        {
            std::lock_guard lock(routes_mutex);
            const auto found = routes.find(request.target);
            if (found == routes.end()) {
                const auto response = http::build_error_response(http::ResponseStatus::not_found);
                static_cast<void>(writer(response));
                return;
            }
            route = found->second;
            route.activity->acquire_http_request();
        }
        const HttpRequestLease request_lease(route.activity);
        const auto response = http::build_stream_response(
            request,
            route.file_size,
            route.content_type
        );
        if (!response.include_body || !response.body_range.has_value()) {
            static_cast<void>(writer(response.headers));
            return;
        }

        const auto demand_id = next_demand_id.fetch_add(1);
        auto demand_state = std::make_shared<DemandState>();
        if (!enqueue({
                CommandType::begin_demand,
                demand_id,
                route,
                bounded_demand_range(
                    route,
                    *response.body_range,
                    response.body_range->start
                ),
                0,
                {},
                demand_state,
            })) {
            const auto unavailable = http::build_error_response(
                http::ResponseStatus::service_unavailable
            );
            static_cast<void>(writer(unavailable));
            return;
        }
        DemandGuard guard{this, demand_id};
        if (!writer(response.headers)) {
            return;
        }
        stream_body(
            demand_id,
            demand_state,
            route,
            *response.body_range,
            writer,
            token
        );
    }

    struct PieceSlice {
        std::uint64_t piece_start = 0;
        std::uint64_t begin = 0;
        std::uint64_t end = 0;
    };

    void stream_body(
        const std::uint64_t demand_id,
        const std::shared_ptr<DemandState>& demand_state,
        const StreamRoute& route,
        const http::ByteRange range,
        const http::LoopbackServer::Writer& writer,
        const std::stop_token token
    ) {
        if (range.end > std::numeric_limits<std::uint64_t>::max() - route.file_offset) {
            return;
        }
        const auto absolute_start = route.file_offset + range.start;
        const auto absolute_end = route.file_offset + range.end;
        if (absolute_end == std::numeric_limits<std::uint64_t>::max()) {
            return;
        }
        const auto absolute_end_exclusive = absolute_end + 1;
        const auto first_piece = absolute_start / route.piece_size;
        const auto last_piece = absolute_end / route.piece_size;
        if (last_piece > std::numeric_limits<std::uint32_t>::max()) {
            return;
        }
        std::optional<storage::PayloadReader> payload;
        for (auto piece = first_piece;
             piece <= last_piece && !token.stop_requested() &&
                 !demand_state->cancelled.load();
             ++piece) {
            const auto piece_start = piece * route.piece_size;
            const auto current_absolute = std::max(absolute_start, piece_start);
            if (current_absolute < route.file_offset) {
                return;
            }
            const PieceSlice slice{
                piece_start,
                current_absolute - piece_start,
                std::min(absolute_end_exclusive, piece_start + route.piece_size) - piece_start,
            };
            if (!serve_piece(
                    demand_id,
                    demand_state,
                    route,
                    static_cast<std::uint32_t>(piece),
                    slice,
                    bounded_demand_range(route, range, current_absolute - route.file_offset),
                    payload,
                    writer,
                    token
                )) {
                return;
            }
        }
    }

    bool serve_piece(
        const std::uint64_t demand_id,
        const std::shared_ptr<DemandState>& demand_state,
        const StreamRoute& route,
        const std::uint32_t piece,
        const PieceSlice slice,
        const http::ByteRange demand_range,
        std::optional<storage::PayloadReader>& payload,
        const http::LoopbackServer::Writer& writer,
        const std::stop_token token
    ) {
        auto waiter = std::make_shared<PieceWaiter>();
        waiter->demand_id = demand_id;
        if (!enqueue({
                CommandType::read_piece,
                demand_id,
                route,
                demand_range,
                piece,
                waiter,
                demand_state,
            })) {
            return false;
        }
        std::stop_callback stop_wait(token, [waiter] {
            {
                std::lock_guard lock(waiter->mutex);
            }
            waiter->ready.notify_all();
        });
        const auto abandon = [&] {
            std::lock_guard lock(waiter->mutex);
            waiter->cancelled = true;
        };
        auto cursor = slice.begin;
        auto deadline = std::chrono::steady_clock::now() + piece_wait_timeout;
        // Stremio Origin: each piece tries the file again (the reader reopens when
        // the payload moved): a read that failed while a drive went away must
        // not leave the rest of the request on the slow whole-piece path
        auto payload_readable = true;
        std::vector<char> buffer;
        while (cursor < slice.end) {
            std::shared_ptr<std::vector<char>> verified;
            std::uint64_t unverified_end = cursor;
            {
                std::unique_lock lock(waiter->mutex);
                const auto unverified_ready = [&] {
                    return payload_readable &&
                        finished_run_end(waiter->finished_blocks, cursor, slice.end) > cursor;
                };
                waiter->ready.wait_until(lock, deadline, [&] {
                    return waiter->done || token.stop_requested() ||
                        demand_state->cancelled.load() || unverified_ready();
                });
                if (token.stop_requested() || demand_state->cancelled.load() ||
                    (waiter->done && !waiter->error.empty())) {
                    waiter->cancelled = true;
                    return false;
                }
                if (waiter->done) {
                    verified = waiter->data;
                } else if (unverified_ready()) {
                    unverified_end = std::min(
                        finished_run_end(waiter->finished_blocks, cursor, slice.end),
                        cursor + payload_read_chunk_bytes
                    );
                } else {
                    waiter->cancelled = true;
                    return false;
                }
            }
            if (verified) {
                if (slice.end > verified->size()) {
                    return false;
                }
                const auto bytes = std::span<const char>(*verified).subspan(
                    static_cast<std::size_t>(cursor),
                    static_cast<std::size_t>(slice.end - cursor)
                );
                if (!writer(bytes)) {
                    return false;
                }
                add_delivered_bytes(route.counters, static_cast<std::uint64_t>(bytes.size()));
                return true;
            }
            const auto absolute = slice.piece_start + cursor;
            const auto source = payload_source(route, absolute);
            if (!source || source->length == 0) {
                payload_readable = false;
                continue;
            }
            unverified_end = std::min(unverified_end, cursor + source->length);
            // Stremio Origin: the file is opened again when the payload moved, and for
            // segments every time (a freed segment comes back as a new file)
            const auto generation = storage_generation.load();
            if (!payload.has_value() || !payload->is_open() || source->segment ||
                payload->tag() != generation || payload->path() != source->path) {
                payload.emplace(source->path, generation);
            }
            buffer.resize(static_cast<std::size_t>(unverified_end - cursor));
            if (!payload->read_exact(source->offset, buffer)) {
                payload_readable = false;
                continue;
            }
            if (!writer(buffer)) {
                abandon();
                return false;
            }
            add_delivered_bytes(route.counters, static_cast<std::uint64_t>(buffer.size()));
            cursor = unverified_end;
            deadline = std::chrono::steady_clock::now() + piece_wait_timeout;
        }
        abandon();
        return true;
    }

    static std::uint64_t finished_run_end(
        const std::vector<bool>& blocks,
        const std::uint64_t cursor,
        const std::uint64_t limit
    ) {
        auto block = cursor / block_bytes;
        while (block < blocks.size() && blocks[static_cast<std::size_t>(block)]) {
            ++block;
        }
        return std::min(block * block_bytes, limit);
    }

    bool enqueue(HttpCommand command) {
        if (shutting_down.load()) {
            return false;
        }
        std::lock_guard lock(commands_mutex);
        if (commands.size() >= maximum_http_commands) {
            return false;
        }
        commands.push_back(std::move(command));
        if (wake) {
            wake();
        }
        return true;
    }

    void enqueue_end(const std::uint64_t demand_id) {
        std::lock_guard lock(commands_mutex);
        commands.push_back({CommandType::end_demand, demand_id, {}, {}, 0, {}, {}});
        if (wake) {
            wake();
        }
    }

    void set_wakeup(std::function<void()> callback) {
        std::lock_guard lock(commands_mutex);
        wake = std::move(callback);
    }

    struct PayloadSource {
        std::filesystem::path path;
        std::uint64_t offset = 0;  // in that file
        std::uint64_t length = 0;  // readable from there in that file
        bool segment = false;
    };

    // Stremio Origin: where a byte of the streamed file is now: its segment file,
    // or the torrent's own file under the current save path (it changes when
    // the payload is relocated)
    std::optional<PayloadSource> payload_source(const StreamRoute& route, const std::uint64_t absolute) {
        if (absolute < route.file_offset || absolute >= route.file_offset + route.file_size) {
            return std::nullopt;
        }
        if (window_store) {
            if (auto segment = window_store->segment_location(route.window_key, absolute)) {
                return PayloadSource{std::move(segment->path), segment->offset, segment->length, true};
            }
        }
        std::filesystem::path path = route.payload_path;
        {
            std::lock_guard lock(routes_mutex);
            const auto root = save_paths.find(route.torrent_id);
            const auto relative = relative_paths.find(route.torrent_id + "/" + std::to_string(route.file_index));
            if (root != save_paths.end() && relative != relative_paths.end()) {
                path = root->second / relative->second;
            }
        }
        return PayloadSource{
            std::move(path),
            absolute - route.file_offset,
            route.file_offset + route.file_size - absolute,
            false,
        };
    }

    // Stremio Origin: the torrent's payload now lives under save_path and starts
    // empty: what was freed or being read from the old place is forgotten,
    // and readers reopen their files
    void relocated(const std::string& id, const std::filesystem::path& save_path) {
        {
            std::lock_guard lock(routes_mutex);
            save_paths.insert_or_assign(id, save_path);
        }
        storage_generation.fetch_add(1);
        freed_pieces.erase(id);
        std::erase_if(requested_reads, [&](const auto& entry) {
            return entry.first.torrent_id == id;
        });
        std::erase_if(partial_blocks, [&](const auto& entry) {
            return entry.first.torrent_id == id;
        });
        window_unsupported = false;
        next_window_check = {};
        next_disk_retry = {};
        // every piece is missing again: the schedule is sent to libtorrent in
        // full (it only sends changes otherwise, and the pieces at the
        // playhead had none: they were there), deadlines included
        current_schedules.erase(id);
        for (auto& [demand_id, demand] : active_demands) {
            static_cast<void>(demand_id);
            if (demand.route.torrent_id == id) {
                demand.needs_data = true;
            }
        }
        recompute_schedule(id);
    }

    void set_window_bytes(const std::uint64_t bytes) {
        if (window_bytes.load() == 0 || !window_store || bytes == 0) {
            return;
        }
        pending_window_bytes.store(bytes);
        std::lock_guard lock(commands_mutex);
        if (wake) {
            wake();
        }
    }

    // Stremio Origin: a new window size from the app (storage running low, or the
    // stream's bitrate became known): new read-ahead, schedules recomputed,
    // and a trim right away
    void apply_pending_window() {
        const auto bytes = pending_window_bytes.exchange(0);
        if (bytes == 0 || bytes == window_bytes.load()) {
            return;
        }
        window_bytes.store(bytes);
        window = make_stream_window(disk_capacity, bytes);
        std::vector<std::string> ids;
        ids.reserve(current_schedules.size());
        for (const auto& [id, schedule] : current_schedules) {
            static_cast<void>(schedule);
            ids.push_back(id);
        }
        for (const auto& id : ids) {
            recompute_schedule(id);
        }
        next_window_check = {};
    }

    void poll() {
        apply_pending_window();
        std::deque<HttpCommand> available;
        {
            std::lock_guard lock(commands_mutex);
            available.swap(commands);
        }
        std::unordered_set<std::string> changed_torrents;
        for (auto& command : available) {
            switch (command.type) {
            case CommandType::begin_demand:
                if (!revoked_torrents.contains(command.route.torrent_id) &&
                    route_is_active(command.route)) {
                    const auto id = command.route.torrent_id;
                    const auto needs_data = !piece_available(
                        command.route,
                        static_cast<std::uint32_t>(
                            (command.route.file_offset + command.range.start) /
                            command.route.piece_size
                        )
                    );
                    // The anchor is where the reader is, and only a demand that is actually MISSING
                    // data may move it (below). A request's opening position alone is not enough:
                    // a probe or an abandoned seek opens one too, and pointing the fallback
                    // schedule at it downloaded bytes far from the playhead that the window then
                    // evicted and libtorrent fetched again.
                    active_demands.insert_or_assign(command.demand_id, ActiveDemand{
                        command.demand_id,
                        std::move(command.route),
                        command.range,
                        std::move(command.demand_state),
                        needs_data,
                        ++next_activity,
                        false,
                    });
                    changed_torrents.insert(id);
                } else if (command.demand_state) {
                    command.demand_state->cancelled.store(true);
                }
                break;
            case CommandType::end_demand: {
                const auto found = active_demands.find(command.demand_id);
                if (found != active_demands.end()) {
                    changed_torrents.insert(found->second.route.torrent_id);
                    active_demands.erase(found);
                }
                break;
            }
            case CommandType::read_piece:
                if (const auto demand = active_demands.find(command.demand_id);
                    demand != active_demands.end() && demand->second.state &&
                    !demand->second.state->cancelled.load()) {
                    demand->second.range = command.range;
                    demand->second.activity = ++next_activity;
                    demand->second.served = true;
                    changed_torrents.insert(demand->second.route.torrent_id);
                    process_piece_request(command, demand->second);
                } else if (command.waiter) {
                    complete_waiter(command.waiter, {}, "stream demand is no longer active");
                }
                break;
            }
        }
        prune_cancelled_waiters();
        for (const auto& id : changed_torrents) {
            recompute_schedule(id);
        }
        if (!changed_torrents.empty()) {
            // a new schedule is about to be downloaded: make room right away
            next_window_check = {};
        }
        reissue_stalled_reads();
        expire_inactive_streams();
        enforce_streaming_window();
    }

    // ---- streaming window (#334) ------------------------------------------

    struct WindowCandidate {
        std::uint32_t piece = 0;
        std::uint64_t distance = 0;
    };

    void enforce_streaming_window() {
        if (window_bytes == 0 || !window_store || window_unsupported) {
            return;
        }
        const auto now = std::chrono::steady_clock::now();
        if (now < next_window_check) {
            return;
        }
        next_window_check = now + window_check_interval;
        std::vector<StreamRoute> active;
        {
            std::lock_guard lock(routes_mutex);
            for (const auto& [target, route] : routes) {
                static_cast<void>(target);
                if (std::ranges::none_of(active, [&](const StreamRoute& other) {
                        return other.torrent_id == route.torrent_id;
                    })) {
                    active.push_back(route);
                }
            }
        }
        std::uint64_t resident_total = 0;
        for (const auto& route : active) {
            resident_total += trim_route(route);
        }
        // Stremio Origin: a write that hit a full disk put the torrent in upload
        // mode (libtorrent's answer to ENOSPC), and nothing takes it out for a
        // torrent that is not auto-managed: the stream would wait forever.
        // The window frees room (and the app shrinks it), so try again every
        // two seconds; a disk still full only sends it back.
        if (now >= next_disk_retry) {
            next_disk_retry = now + std::chrono::seconds(2);
            for (const auto& route : active) {
                try {
                    if (route.handle.is_valid() &&
                        (route.handle.flags() & lt::torrent_flags::upload_mode)) {
                        route.handle.unset_flags(lt::torrent_flags::upload_mode);
                        disk_full_retries.fetch_add(1, std::memory_order_relaxed);
                    }
                    // Stremio Origin: a read or write that hit a vanished drive paused
                    // the torrent with an error; once the payload has moved (the
                    // app does it within seconds) it carries on in the new place
                    if (route.handle.is_valid() && storage_generation.load() > 0) {
                        const auto status = route.handle.status({});
                        if (status.errc && storage_gone(status.errc)) {
                            route.handle.clear_error();
                            route.handle.resume();
                        }
                    }
                } catch (...) {
                }
            }
        }
        window_resident_bytes.store(resident_total, std::memory_order_relaxed);
    }

    // Diagnostic only: the absolute position the window is anchored to, so a
    // freed or refetched piece can be reported against the playhead (p2p log).
    std::optional<std::uint64_t> playhead_for(
        const std::string& id,
        const StreamRoute& route
    ) const {
        std::optional<std::uint64_t> playhead;
        const auto consider = [&](const std::uint64_t absolute) {
            if (!playhead || absolute > *playhead) {
                playhead = absolute;
            }
        };
        for (const auto& [demand_id, demand] : active_demands) {
            static_cast<void>(demand_id);
            if (demand.route.torrent_id == id) {
                consider(route.file_offset + demand.range.start);
            }
        }
        if (const auto anchor = anchors.find(id); anchor != anchors.end()) {
            consider(route.file_offset + anchor->second.range.start);
        }
        return playhead;
    }

    // returns the bytes left resident for this torrent
    std::uint64_t trim_route(const StreamRoute& route) {        const auto& id = route.torrent_id;
        const auto resident = window_store->resident_pieces(route.window_key);
        const std::uint64_t piece_size = route.piece_size;
        if (piece_size == 0 || route.file_size == 0) {
            return 0;
        }
        auto resident_bytes = static_cast<std::uint64_t>(resident.size()) * piece_size;
        // the whole file fits in the window: keep everything, as without one
        if (route.file_size <= window_bytes) {
            return resident_bytes;
        }
        // a piece freed earlier and written again was downloaded twice
        if (auto freed = freed_pieces.find(id); freed != freed_pieces.end()) {
            for (const auto piece : resident) {
                if (freed->second.erase(piece) > 0) {
                    window_refetched_pieces.fetch_add(1, std::memory_order_relaxed);
                    const auto at = static_cast<std::uint64_t>(piece) * piece_size;
                    const auto playhead = playhead_for(id, route);
                    const auto ahead = playhead
                        ? (static_cast<std::int64_t>(*playhead) -
                           static_cast<std::int64_t>(at)) / static_cast<std::int64_t>(mib)
                        : 0;
                    std::fprintf(
                        stderr,
                        "[p2p] window refetch piece=%u at=%lluMB playhead=%+lldMB\n",
                        piece,
                        static_cast<unsigned long long>(at / mib),
                        static_cast<long long>(ahead)
                    );
                }
            }
        }
        // what will be on disk once the current schedule is downloaded
        const auto schedule = current_schedules.find(id);
        auto committed_bytes = resident_bytes;
        if (schedule != current_schedules.end()) {
            for (const auto& [piece, priority] : schedule->second.priorities) {
                static_cast<void>(priority);
                if (!std::ranges::binary_search(resident, piece)) {
                    committed_bytes += piece_size;
                }
            }
        }
        if (committed_bytes <= window_high_water(window_bytes)) {
            return resident_bytes;
        }
        const auto target = window_low_water(window_bytes);
        const auto piece_bytes = static_cast<std::uint64_t>(piece_size);
        // Whole pieces are the unit of the schedule and of eviction. A cushion smaller than
        // the pieces it protects frees the piece the reader has just left, so any re-read of
        // it (container re-sync, a heal, a small step back) costs that whole piece over the
        // swarm again: the refetch loop that kept the engine pulling while the player's queue
        // sat full. Two pieces behind the playhead.
        const auto behind = std::max(window_behind_bytes(window_bytes), 2 * piece_bytes);
        const auto ahead = window.critical_bytes + window.playback_bytes +
            window.readahead_bytes + 2 * piece_bytes;
        const auto file_start = route.file_offset;
        const auto file_end = route.file_offset + route.file_size;
        const auto head_end = file_start + std::min(window_head_bytes(window_bytes), route.file_size);
        const auto tail_start = file_end - std::min(window_tail_bytes(window_bytes), route.file_size);

        // playheads, in absolute torrent bytes
        std::vector<std::uint64_t> positions;
        for (const auto& [demand_id, demand] : active_demands) {
            static_cast<void>(demand_id);
            if (demand.route.torrent_id == id) {
                positions.push_back(route.file_offset + demand.range.start);
            }
        }
        if (const auto anchor = anchors.find(id); anchor != anchors.end()) {
            positions.push_back(route.file_offset + anchor->second.range.start);
        }
        if (positions.empty()) {
            return resident_bytes;
        }

        std::vector<WindowCandidate> candidates;
        for (const auto piece : resident) {
            const auto piece_start = static_cast<std::uint64_t>(piece) * piece_size;
            const auto piece_end = piece_start + piece_size;
            // only pieces that lie entirely inside this file: border pieces
            // carry bytes of other files
            if (piece_start < file_start || piece_end > file_end) {
                continue;
            }
            if (piece_start < head_end || piece_end > tail_start) {
                continue;
            }
            const PieceKey key{id, piece};
            if (piece_waiters.contains(key) || requested_reads.contains(key) ||
                partial_blocks.contains(key)) {
                continue;
            }
            if (schedule != current_schedules.end() &&
                schedule->second.priorities.contains(piece)) {
                continue;
            }
            auto nearest = std::numeric_limits<std::uint64_t>::max();
            bool keep = false;
            for (const auto position : positions) {
                if (piece_end <= position) {
                    const auto distance = position - piece_end;
                    keep = keep || distance < behind;
                    nearest = std::min(nearest, distance);
                } else if (piece_start >= position) {
                    const auto distance = piece_start - position;
                    keep = keep || distance < ahead;
                    nearest = std::min(nearest, distance);
                } else {
                    keep = true;
                }
            }
            if (!keep) {
                candidates.push_back({piece, nearest});
            }
        }
        std::ranges::sort(candidates, [](const WindowCandidate& left, const WindowCandidate& right) {
            return left.distance > right.distance;
        });

        for (const auto& candidate : candidates) {
            if (committed_bytes <= target) {
                break;
            }
            const auto result = free_piece(route, candidate.piece);
            if (result == FreeResult::unsupported) {
                break;
            }
            if (result == FreeResult::freed) {
                resident_bytes -= piece_size;
                committed_bytes -= piece_size;
                const auto at = static_cast<std::uint64_t>(candidate.piece) * piece_size;
                const auto playhead = playhead_for(id, route);
                const auto behind = playhead
                    ? (static_cast<std::int64_t>(*playhead) -
                       static_cast<std::int64_t>(at)) / static_cast<std::int64_t>(mib)
                    : 0;
                std::fprintf(
                    stderr,
                    "[p2p] window free piece=%u at=%lluMB playhead=%+lldMB resident=%lluMB\n",
                    candidate.piece,
                    static_cast<unsigned long long>(at / mib),
                    static_cast<long long>(behind),
                    static_cast<unsigned long long>(resident_bytes / mib)
                );
            }
        }
        return resident_bytes;
    }

    enum class FreeResult { freed, skipped, unsupported };

    FreeResult free_piece(const StreamRoute& route, const std::uint32_t piece) {
        const auto index = lt::piece_index_t(static_cast<int>(piece));
        try {
            // only verified pieces: a piece still downloading may be read
            // block by block from the file by an HTTP worker
            if (!route.handle.is_valid() || !route.handle.have_piece(index)) {
                return FreeResult::skipped;
            }
        } catch (...) {
            return FreeResult::skipped;
        }
        // 1. reads of this piece fail from now on (never the hole's zeros)
        if (!window_store->mark_freed(route.window_key, piece)) {
            return FreeResult::skipped;
        }
        // 2. release the bytes: a hole in the file, or (segments) the segment
        // file once none of its pieces is kept
        const auto piece_start = static_cast<std::uint64_t>(piece) * route.piece_size;
        std::string error = "payload path unknown";
        bool released = window_store->release_segment(route.window_key, piece);
        if (!released) {
            const auto source = payload_source(route, piece_start);
            released = source && storage::punch_hole(source->path, source->offset, route.piece_size, &error);
        }
        if (!released) {
            window_store->unmark_freed(route.window_key, piece);
            window_unsupported = true;
            window_unsupported_reason = error;
            return FreeResult::unsupported;
        }
        // 3. libtorrent forgets it; a later demand downloads it again. The
        // deadline and the explicit priority must go with the bytes: a
        // time-critical piece is downloaded again because it is in libtorrent's
        // time-critical list, not because a priority asks for it, and clearing
        // "have" alone leaves the piece wanted. On the television that was the
        // window evicting one piece far from the playhead over and over (the
        // file header at 4 MB and the seek target): freed, fetched again at the
        // swarm's pace, freed again. Every freed piece is outside the schedule
        // and the trim's protected set, so dropping both wants is safe.
        int priority = -1;
        try {
            priority = static_cast<int>(static_cast<std::uint8_t>(
                route.handle.piece_priority(index)
            ));
            route.handle.reset_piece_deadline(index);
            std::vector<std::pair<lt::piece_index_t, lt::download_priority_t>> none{
                {index, lt::dont_download},
            };
            route.handle.prioritize_pieces(none);
        } catch (...) {
        }
        std::fprintf(
            stderr,
            "[p2p] window drop piece=%u priority=%d\n",
            piece,
            priority
        );
        try {
            route.handle.nuvio_drop_piece(index);
        } catch (...) {
        }
        requested_reads.erase(PieceKey{route.torrent_id, piece});
        freed_pieces[route.torrent_id].insert(piece);
        window_freed_pieces.fetch_add(1, std::memory_order_relaxed);
        window_freed_bytes.fetch_add(route.piece_size, std::memory_order_relaxed);
        return FreeResult::freed;
    }

    void reissue_stalled_reads() {
        const auto now = std::chrono::steady_clock::now();
        if (piece_waiters.empty() || now < next_read_watchdog) {
            return;
        }
        next_read_watchdog = now + std::chrono::seconds(1);
        for (const auto& [key, waiters] : piece_waiters) {
            static_cast<void>(waiters);
            const auto requested = requested_reads.find(key);
            if (requested != requested_reads.end() &&
                now - requested->second < std::chrono::seconds(2)) {
                continue;
            }
            const auto demand = std::ranges::find_if(active_demands, [&](const auto& entry) {
                return entry.second.route.torrent_id == key.torrent_id;
            });
            if (demand == active_demands.end()) {
                continue;
            }
            const auto& handle = demand->second.route.handle;
            try {
                const auto index = lt::piece_index_t(static_cast<int>(key.piece));
                if (handle.have_piece(index)) {
                    handle.read_piece(index);
                    requested_reads.insert_or_assign(key, now);
                }
            } catch (...) {
            }
        }
    }

    void expire_inactive_streams() {
        if (inactivity_timeout.count() == 0) {
            return;
        }
        const auto now = std::chrono::steady_clock::now();
        if (now < next_inactivity_check) {
            return;
        }
        const auto check_interval = std::clamp(
            inactivity_timeout / 2,
            std::chrono::milliseconds(25),
            std::chrono::milliseconds(1000)
        );
        next_inactivity_check = now + check_interval;
        std::vector<StoppedStream> expired;
        {
            std::lock_guard lock(routes_mutex);
            for (auto route_entry = routes.begin(); route_entry != routes.end();) {
                const auto& route = route_entry->second;
                const auto has_active_demand = std::ranges::any_of(
                    active_demands,
                    [&](const auto& demand) {
                        return demand.second.route.stream_id == route.stream_id;
                    }
                );
                if (has_active_demand ||
                    !route.activity->is_inactive(now, inactivity_timeout)) {
                    ++route_entry;
                    continue;
                }
                expired.push_back({route.torrent_id, route.stream_id});
                route_entry = routes.erase(route_entry);
                next_progress_sample = {};
            }
        }
        for (auto& stream : expired) {
            erase_anchor(stream.stream_id);
            recompute_schedule(stream.torrent_id);
            expired_streams.push_back(std::move(stream));
        }
    }

    std::vector<StoppedStream> pop_expired_streams() {
        std::vector<StoppedStream> result;
        result.swap(expired_streams);
        return result;
    }

    bool route_is_active(const StreamRoute& route) {
        std::lock_guard lock(routes_mutex);
        const auto active = routes.find(route.target);
        return active != routes.end() &&
            active->second.stream_id == route.stream_id &&
            active->second.torrent_id == route.torrent_id;
    }

    void cancel_demand_waiters(
        const std::uint64_t demand_id,
        const std::string& error
    ) {
        for (auto entry = piece_waiters.begin(); entry != piece_waiters.end();) {
            auto& waiters = entry->second;
            for (const auto& waiter : waiters) {
                if (waiter->demand_id == demand_id) {
                    complete_waiter(waiter, {}, error);
                }
            }
            std::erase_if(waiters, [&](const std::shared_ptr<PieceWaiter>& waiter) {
                return waiter->demand_id == demand_id;
            });
            if (waiters.empty()) {
                partial_blocks.erase(entry->first);
                entry = piece_waiters.erase(entry);
            } else {
                ++entry;
            }
        }
    }

    bool piece_available(const StreamRoute& route, const std::uint32_t piece) {
        if (piece_cache.contains(PieceKey{route.torrent_id, piece})) {
            return true;
        }
        try {
            return route.handle.have_piece(lt::piece_index_t(static_cast<int>(piece)));
        } catch (...) {
            return false;
        }
    }

    void erase_anchor(const std::string& stream_id) {
        std::erase_if(anchors, [&](const auto& anchor) {
            return anchor.second.route.stream_id == stream_id;
        });
    }

    void process_piece_request(const HttpCommand& command, ActiveDemand& demand) {
        if (!command.waiter) {
            return;
        }
        if (revoked_torrents.contains(command.route.torrent_id) ||
            !command.route.handle.is_valid() || !command.route.handle.in_session()) {
            complete_waiter(command.waiter, {}, "torrent is unavailable");
            return;
        }
        const auto metadata = command.route.handle.torrent_file();
        if (!metadata || command.piece >= static_cast<std::uint32_t>(metadata->num_pieces())) {
            complete_waiter(command.waiter, {}, "torrent piece is out of range");
            return;
        }
        const PieceKey key{command.route.torrent_id, command.piece};
        if (auto cached = piece_cache.get(key)) {
            last_ready_pieces.insert_or_assign(command.route.torrent_id, command.piece);
            demand.needs_data = false;
            complete_waiter(command.waiter, std::move(cached), {});
            return;
        }
        piece_waiters[key].push_back(command.waiter);
        demand.needs_data = !piece_available(command.route, command.piece);
        if (!demand.needs_data) {
            // Stremio Origin: a verified piece is served by the HTTP worker straight from the payload
            // file (pread, as for unverified blocks) instead of read_piece(), which copies the
            // whole piece into RAM twice: 64 MB per read for the 32 MB pieces of a 100 GB torrent,
            // on a TV with ~1 GB for everything. Pieces freed by the streaming window are not
            // "have", so this never reads a punched hole.
            last_ready_pieces.insert_or_assign(command.route.torrent_id, command.piece);
            const auto index = lt::piece_index_t(static_cast<int>(command.piece));
            const auto piece_bytes = static_cast<std::uint64_t>(std::max(metadata->piece_size(index), 0));
            publish_blocks(
                command.waiter,
                std::vector<bool>(static_cast<std::size_t>((piece_bytes + block_bytes - 1) / block_bytes), true)
            );
            return;
        }
        auto partial = partial_blocks.find(key);
        if (partial == partial_blocks.end()) {
            partial = partial_blocks.emplace(
                key,
                written_blocks(command.route.handle, *metadata, command.piece)
            ).first;
        }
        publish_blocks(command.waiter, partial->second);
    }

    static std::vector<bool> written_blocks(
        const lt::torrent_handle& handle,
        const lt::torrent_info& metadata,
        const std::uint32_t piece
    ) {
        const auto index = lt::piece_index_t(static_cast<int>(piece));
        const auto piece_bytes = static_cast<std::uint64_t>(
            std::max(metadata.piece_size(index), 0)
        );
        std::vector<bool> blocks(
            static_cast<std::size_t>((piece_bytes + block_bytes - 1) / block_bytes),
            false
        );
        try {
            std::vector<lt::partial_piece_info> queue;
            handle.get_download_queue(queue);
            for (const auto& partial : queue) {
                if (partial.piece_index != index) {
                    continue;
                }
                const auto count = std::min(
                    blocks.size(),
                    static_cast<std::size_t>(std::max(partial.blocks_in_piece, 0))
                );
                for (std::size_t block = 0; block < count; ++block) {
                    blocks[block] = partial.blocks[block].state == lt::block_info::finished;
                }
            }
        } catch (...) {
        }
        return blocks;
    }

    static void publish_blocks(
        const std::shared_ptr<PieceWaiter>& waiter,
        const std::vector<bool>& blocks
    ) {
        {
            std::lock_guard lock(waiter->mutex);
            if (waiter->done || waiter->cancelled) {
                return;
            }
            waiter->finished_blocks = blocks;
        }
        waiter->ready.notify_all();
    }

    void publish_partial(const PieceKey& key) {
        const auto partial = partial_blocks.find(key);
        const auto waiters = piece_waiters.find(key);
        if (partial == partial_blocks.end() || waiters == piece_waiters.end()) {
            return;
        }
        for (const auto& waiter : waiters->second) {
            publish_blocks(waiter, partial->second);
        }
    }

    void handle_block_finished(const lt::block_finished_alert& alert) {
        if (partial_blocks.empty()) {
            return;
        }
        const PieceKey key{
            torrent_id(alert.handle),
            static_cast<std::uint32_t>(static_cast<int>(alert.piece_index)),
        };
        const auto partial = partial_blocks.find(key);
        if (partial == partial_blocks.end() || alert.block_index < 0 ||
            static_cast<std::size_t>(alert.block_index) >= partial->second.size()) {
            return;
        }
        const auto first_data = std::ranges::none_of(partial->second, [](const bool finished) {
            return finished;
        });
        partial->second[static_cast<std::size_t>(alert.block_index)] = true;
        publish_partial(key);
        if (first_data) {
            recompute_schedule(key.torrent_id);
        }
    }

    void handle_hash_failed(const lt::hash_failed_alert& alert) {
        const PieceKey key{
            torrent_id(alert.handle),
            static_cast<std::uint32_t>(static_cast<int>(alert.piece_index)),
        };
        const auto partial = partial_blocks.find(key);
        if (partial == partial_blocks.end()) {
            return;
        }
        std::fill(partial->second.begin(), partial->second.end(), false);
        publish_partial(key);
    }

    void recompute_schedule(const std::string& id) {
        std::map<std::uint32_t, scheduler::PriorityClass> combined;
        std::optional<lt::torrent_handle> handle;
        std::vector<scheduler::StreamDemand> demands;
        for (const auto& entry : active_demands) {
            const auto& demand = entry.second;
            if (demand.route.torrent_id != id || !demand.needs_data) {
                continue;
            }
            handle = demand.route.handle;
            const auto blocking_piece = static_cast<std::uint32_t>(
                (demand.route.file_offset + demand.range.start) / demand.route.piece_size
            );
            const auto partial = partial_blocks.find(PieceKey{id, blocking_piece});
            // `activity`, not `demand_id`: the plan focuses the demand that was SERVED most
            // recently, and only that demand gets the read-ahead. A younger request that is
            // missing data must not move the download away from the reader: its own blocking
            // piece is still scheduled below.
            demands.push_back({
                demand.served ? (demand.activity | (std::uint64_t{1} << 63)) : demand.activity,
                demand.route.file_offset,
                demand.route.file_size,
                demand.route.piece_size,
                demand.range,
                scheduler::DemandKind::reader,
                partial == partial_blocks.end() ||
                    std::ranges::none_of(partial->second, [](const bool finished) {
                        return finished;
                    }),
            });
        }
        const auto anchor = anchors.find(id);
        if (demands.empty() && anchor != anchors.end()) {
            handle = anchor->second.route.handle;
            demands.push_back({
                0,
                anchor->second.route.file_offset,
                anchor->second.route.file_size,
                anchor->second.route.piece_size,
                anchor->second.range,
                anchor->second.prefetch
                    ? scheduler::DemandKind::prefetch
                    : scheduler::DemandKind::anchor,
            });
        }
        std::map<std::uint32_t, int> first_blocks;
        for (const auto& demand : demands) {
            const auto absolute = demand.file_offset + demand.range.start;
            first_blocks.insert_or_assign(
                static_cast<std::uint32_t>(absolute / demand.piece_size),
                static_cast<int>((absolute % demand.piece_size) / block_bytes)
            );
        }
        scheduler::StreamDemandPlan plan;
        try {
            plan = scheduler::build_stream_demand_plan(std::move(demands), window);
            for (const auto& priority : plan.pieces) {
                combined.insert_or_assign(priority.piece, priority.priority);
            }
        } catch (...) {
            return;
        }
        // The anchor is the fallback schedule when nothing is missing data, so it must be the
        // playhead and nothing else: the position of the MOST RECENTLY SERVED demand that is
        // missing data. Taking the plan's focused demand instead let a younger request (a probe, an
        // abandoned seek) keep the download pointed at its own offset, and every piece that was
        // fetched there and evicted by the window came back over the swarm again.
        const ActiveDemand* playhead = nullptr;
        for (const auto& entry : active_demands) {
            const auto& candidate = entry.second;
            if (candidate.route.torrent_id != id || !candidate.needs_data ||
                !candidate.served) {
                continue;
            }
            if (playhead == nullptr || candidate.activity > playhead->activity) {
                playhead = &candidate;
            }
        }
        if (playhead != nullptr) {
            anchors.insert_or_assign(id, StreamAnchor{playhead->route, playhead->range, false});
        }
        const auto previous = current_schedules.find(id);
        if (!handle.has_value() && previous != current_schedules.end()) {
            handle = previous->second.handle;
        }
        if (!handle.has_value() || !handle->is_valid()) {
            current_schedules.erase(id);
            return;
        }
        if (combined.empty() && previous != current_schedules.end() &&
            has_stream_for_torrent(id)) {
            combined = previous->second.priorities;
            for (auto& [piece, priority] : combined) {
                static_cast<void>(piece);
                if (priority == scheduler::PriorityClass::blocking ||
                    priority == scheduler::PriorityClass::critical) {
                    priority = scheduler::PriorityClass::playback;
                }
            }
        }

        try {
            if (previous == current_schedules.end()) {
                const auto metadata = handle->torrent_file();
                if (metadata) {
                    std::vector<lt::download_priority_t> priorities(
                        static_cast<std::size_t>(metadata->num_pieces()),
                        lt::dont_download
                    );
                    for (const auto& [piece, priority] : combined) {
                        if (piece < priorities.size()) {
                            priorities[piece] = download_priority_for(priority, plan.cold);
                        }
                    }
                    handle->prioritize_pieces(priorities);
                }
            } else {
                std::vector<std::pair<lt::piece_index_t, lt::download_priority_t>> updates;
                updates.reserve(previous->second.priorities.size() + combined.size());
                for (const auto& [piece, priority] : previous->second.priorities) {
                    static_cast<void>(priority);
                    if (!combined.contains(piece)) {
                        updates.emplace_back(
                            lt::piece_index_t(static_cast<int>(piece)),
                            lt::dont_download
                        );
                    }
                }
                for (const auto& [piece, priority] : combined) {
                    const auto old = previous->second.priorities.find(piece);
                    if (old == previous->second.priorities.end() ||
                        download_priority_for(old->second, previous->second.cold) !=
                            download_priority_for(priority, plan.cold)) {
                        updates.emplace_back(
                            lt::piece_index_t(static_cast<int>(piece)),
                            download_priority_for(priority, plan.cold)
                        );
                    }
                }
                if (!updates.empty()) {
                    handle->prioritize_pieces(updates);
                }
            }
        } catch (...) {
        }

        static const std::vector<std::uint32_t> no_deadlines;
        const auto& previous_deadlines = previous == current_schedules.end()
            ? no_deadlines
            : previous->second.deadline_order;
        const auto& next_deadlines = plan.deadline_order;
        const auto in_next = [&](const std::uint32_t piece) {
            return std::ranges::find(next_deadlines, piece) != next_deadlines.end();
        };
        const auto in_previous = [&](const std::uint32_t piece) {
            return std::ranges::find(previous_deadlines, piece) != previous_deadlines.end();
        };
        const auto reset_deadline = [&](const std::uint32_t piece) {
            try {
                handle->reset_piece_deadline(lt::piece_index_t(static_cast<int>(piece)));
            } catch (...) {
            }
            requested_reads.erase(PieceKey{id, piece});
        };
        const auto jumped = !previous_deadlines.empty() && !next_deadlines.empty() &&
            std::ranges::none_of(previous_deadlines, in_next);
        if (jumped) {
            for (const auto piece : previous_deadlines) {
                reset_deadline(piece);
            }
        }
        for (std::size_t ordinal = 0; ordinal < next_deadlines.size(); ++ordinal) {
            const auto piece = next_deadlines[ordinal];
            const PieceKey key{id, piece};
            const auto read_requested = requested_reads.contains(key);
            const auto wants_read = piece_waiters.contains(key) && !read_requested;
            const auto unchanged = !jumped &&
                ordinal < previous_deadlines.size() &&
                previous_deadlines[ordinal] == piece;
            if (unchanged && !wants_read) {
                continue;
            }
            try {
                handle->set_piece_deadline(
                    lt::piece_index_t(static_cast<int>(piece)),
                    deadline_for(ordinal),
                    wants_read || read_requested
                        ? lt::torrent_handle::alert_when_available
                        : lt::deadline_flags_t{}
                );
                if (wants_read) {
                    requested_reads.try_emplace(key, std::chrono::steady_clock::now());
                }
            } catch (const std::exception& error) {
                fail_piece(key, error.what());
            } catch (...) {
                fail_piece(key, "failed to request blocking piece");
            }
        }
        if (!jumped) {
            for (const auto piece : previous_deadlines) {
                if (!in_next(piece)) {
                    reset_deadline(piece);
                }
            }
        }
        std::map<std::uint32_t, int> applied_first_blocks;
        for (const auto piece : next_deadlines) {
            const auto wanted = first_blocks.find(piece);
            const auto block = wanted == first_blocks.end() ? 0 : wanted->second;
            const auto previous_block = [&] {
                if (jumped || previous == current_schedules.end()) {
                    return 0;
                }
                const auto found = previous->second.first_blocks.find(piece);
                return found == previous->second.first_blocks.end() ? 0 : found->second;
            }();
            if (block != previous_block || (block > 0 && !in_previous(piece))) {
                try {
                    handle->set_piece_first_block(
                        lt::piece_index_t(static_cast<int>(piece)),
                        block
                    );
                } catch (...) {
                }
            }
            if (block > 0) {
                applied_first_blocks.emplace(piece, block);
            }
        }

        if (combined.empty()) {
            current_schedules.erase(id);
        } else {
            const auto revision = previous == current_schedules.end()
                ? std::uint64_t{1}
                : previous->second.revision +
                    static_cast<std::uint64_t>(
                        previous->second.priorities != combined ||
                        previous->second.deadline_order != plan.deadline_order ||
                        previous->second.focused_demand_id != plan.focused_demand_id
                    );
            current_schedules.insert_or_assign(
                id,
                CurrentSchedule{
                    *handle,
                    std::move(combined),
                    std::move(plan.deadline_order),
                    plan.focused_demand_id,
                    revision,
                    plan.cold,
                    std::move(applied_first_blocks),
                }
            );
        }
    }

    void handle_read_piece(const lt::read_piece_alert& alert) {
        const PieceKey key{
            torrent_id(alert.handle),
            static_cast<std::uint32_t>(static_cast<int>(alert.piece)),
        };
        requested_reads.erase(key);
        partial_blocks.erase(key);
        if (alert.error && storage_gone(alert.error)) {
            // Stremio Origin: the piece's bytes are gone with its drive (the app
            // moves the payload within seconds; a piece verified in that very
            // moment can also be missing in the new place): it is forgotten and
            // downloaded again, the schedule sent in full so it gets its
            // deadline, and the stream waits for it instead of failing
            try {
                alert.handle.nuvio_drop_piece(alert.piece);
            } catch (...) {
            }
            current_schedules.erase(key.torrent_id);
            recompute_schedule(key.torrent_id);
            return;
        }
        if (alert.error || !alert.buffer || alert.size <= 0) {
            fail_piece(key, alert.error ? alert.error.message() : "verified piece is empty");
            return;
        }
        auto data = std::make_shared<std::vector<char>>(
            alert.buffer.get(),
            alert.buffer.get() + alert.size
        );
        last_ready_pieces.insert_or_assign(key.torrent_id, key.piece);
        piece_cache.put(key, data);
        const auto found = piece_waiters.find(key);
        if (found == piece_waiters.end()) {
            return;
        }
        for (const auto& waiter : found->second) {
            complete_waiter(waiter, data, {});
        }
        piece_waiters.erase(found);
    }

    static bool storage_gone(const lt::error_code& error) {
        if (error.category() != boost::system::generic_category() &&
            error.category() != boost::system::system_category()) {
            return false;
        }
        const auto value = error.value();
        return value == ENOENT || value == EIO || value == ENODEV || value == ENXIO ||
            value == ENOTDIR || value == EACCES || value == EROFS || value == ESTALE;
    }

    static void complete_waiter(
        const std::shared_ptr<PieceWaiter>& waiter,
        std::shared_ptr<std::vector<char>> data,
        std::string error
    ) {
        {
            std::lock_guard lock(waiter->mutex);
            if (waiter->cancelled || waiter->done) {
                return;
            }
            waiter->data = std::move(data);
            waiter->error = std::move(error);
            waiter->done = true;
        }
        waiter->ready.notify_all();
    }

    void fail_piece(const PieceKey& key, const std::string& error) {
        partial_blocks.erase(key);
        const auto found = piece_waiters.find(key);
        if (found == piece_waiters.end()) {
            return;
        }
        for (const auto& waiter : found->second) {
            complete_waiter(waiter, {}, error);
        }
        piece_waiters.erase(found);
    }

    void prune_cancelled_waiters() {
        for (auto entry = piece_waiters.begin(); entry != piece_waiters.end();) {
            auto& waiters = entry->second;
            std::erase_if(waiters, [](const std::shared_ptr<PieceWaiter>& waiter) {
                std::lock_guard lock(waiter->mutex);
                return waiter->cancelled;
            });
            if (waiters.empty()) {
                partial_blocks.erase(entry->first);
                entry = piece_waiters.erase(entry);
            } else {
                ++entry;
            }
        }
    }

    void remove_torrent(const std::string& id) {
        {
            std::lock_guard lock(routes_mutex);
            save_paths.erase(id);
            std::erase_if(relative_paths, [&](const auto& entry) {
                return entry.first.starts_with(id + "/");
            });
        }
        revoked_torrents.insert(id);
        freed_pieces.erase(id);
        anchors.erase(id);
        piece_cache.erase_torrent(id);
        last_ready_pieces.erase(id);
        std::erase_if(requested_reads, [&](const auto& entry) {
            return entry.first.torrent_id == id;
        });
        std::erase_if(partial_blocks, [&](const auto& entry) {
            return entry.first.torrent_id == id;
        });
        {
            std::lock_guard lock(routes_mutex);
            std::erase_if(routes, [&](const auto& route) {
                return route.second.torrent_id == id;
            });
            next_progress_sample = {};
        }
        for (auto demand = active_demands.begin(); demand != active_demands.end();) {
            if (demand->second.route.torrent_id == id) {
                if (demand->second.state) {
                    demand->second.state->cancelled.store(true);
                }
                demand = active_demands.erase(demand);
            } else {
                ++demand;
            }
        }
        recompute_schedule(id);
        for (auto entry = piece_waiters.begin(); entry != piece_waiters.end();) {
            if (entry->first.torrent_id == id) {
                for (const auto& waiter : entry->second) {
                    complete_waiter(waiter, {}, "torrent was removed");
                }
                entry = piece_waiters.erase(entry);
            } else {
                ++entry;
            }
        }
    }

    bool has_stream_for_torrent(const std::string& id) {
        std::lock_guard lock(routes_mutex);
        return std::ranges::any_of(routes, [&](const auto& route) {
            return route.second.torrent_id == id;
        });
    }

    std::vector<std::uint32_t> blocking_pieces(const std::string& id) const {
        const auto schedule = current_schedules.find(id);
        if (schedule == current_schedules.end()) {
            return {};
        }
        return blocking_pieces_of(schedule->second);
    }

    static std::vector<std::uint32_t> blocking_pieces_of(const CurrentSchedule& schedule) {
        std::vector<std::uint32_t> result;
        for (const auto piece : schedule.deadline_order) {
            const auto priority = schedule.priorities.find(piece);
            if (priority != schedule.priorities.end() &&
                priority->second == scheduler::PriorityClass::blocking) {
                result.push_back(piece);
            }
        }
        return result;
    }

    std::string stop_stream(const std::string& stream_id) {
        std::string id;
        const auto target = "/stream/" + stream_id;
        erase_anchor(stream_id);
        {
            std::lock_guard lock(routes_mutex);
            const auto route = routes.find(target);
            if (route != routes.end()) {
                id = route->second.torrent_id;
                routes.erase(route);
                next_progress_sample = {};
            }
        }
        for (auto demand = active_demands.begin(); demand != active_demands.end();) {
            if (demand->second.route.stream_id != stream_id) {
                ++demand;
                continue;
            }
            if (id.empty()) {
                id = demand->second.route.torrent_id;
            }
            if (demand->second.state) {
                demand->second.state->cancelled.store(true);
            }
            cancel_demand_waiters(demand->first, "stream was stopped");
            demand = active_demands.erase(demand);
        }
        if (!id.empty()) {
            recompute_schedule(id);
        }
        return id;
    }

    BackendStats::Stream progress_for(const StreamRoute& route) const {
        BackendStats::Stream result{};
        result.stream_id = route.stream_id;
        result.file_index = route.file_index;
        result.file_size = route.file_size;
        result.piece_size = route.piece_size;
        result.delivered_bytes = route.counters->delivered_bytes.load(
            std::memory_order_relaxed
        );
        std::vector<const ActiveDemand*> demands;
        for (const auto& [demand_id, demand] : active_demands) {
            static_cast<void>(demand_id);
            if (demand.route.stream_id == route.stream_id) {
                demands.push_back(&demand);
            }
        }
        std::ranges::sort(demands, [](const ActiveDemand* left, const ActiveDemand* right) {
            return left->id > right->id;
        });
        result.active_demands = saturating_count(demands.size());
        if (!demands.empty()) {
            result.primary_demand_start = demands[0]->range.start;
            result.primary_demand_end = demands[0]->range.end;
        }
        if (demands.size() > 1) {
            result.secondary_demand_start = demands[1]->range.start;
            result.secondary_demand_end = demands[1]->range.end;
        }
        if (const auto schedule = current_schedules.find(route.torrent_id);
            schedule != current_schedules.end()) {
            result.scheduled_pieces = saturating_count(schedule->second.priorities.size());
            result.schedule_revision = schedule->second.revision;
            const auto blocking = blocking_pieces_of(schedule->second);
            result.blocking_pieces = saturating_count(blocking.size());
            if (!blocking.empty()) {
                result.primary_blocking_piece = blocking[0];
            }
            if (blocking.size() > 1) {
                result.secondary_blocking_piece = blocking[1];
            }
        }
        if (const auto ready = last_ready_pieces.find(route.torrent_id);
            ready != last_ready_pieces.end()) {
            result.last_ready_piece = ready->second;
        }
        if (route.file_size == 0 || !route.handle.is_valid() ||
            !route.handle.in_session()) {
            return result;
        }
        try {
            const auto metadata = route.handle.torrent_file();
            if (!metadata || metadata->piece_length() <= 0 ||
                route.file_size > std::numeric_limits<std::uint64_t>::max() -
                    route.file_offset) {
                return result;
            }
            const auto file_end = route.file_offset + route.file_size;
            const auto first_piece = route.file_offset / route.piece_size;
            const auto last_piece = (file_end - 1) / route.piece_size;
            if (last_piece >= static_cast<std::uint64_t>(metadata->num_pieces())) {
                return result;
            }
            const auto status = route.handle.status(lt::torrent_handle::query_pieces);
            bool contiguous = true;
            for (auto piece = first_piece; piece <= last_piece; ++piece) {
                const auto piece_number = static_cast<int>(piece);
                const auto index = lt::piece_index_t(piece_number);
                const auto ready = status.is_seeding ||
                    (piece_number < status.pieces.size() && status.pieces[index]);
                if (!ready) {
                    contiguous = false;
                    continue;
                }
                const auto piece_start = piece * route.piece_size;
                const auto raw_piece_bytes = metadata->piece_size(index);
                if (raw_piece_bytes <= 0) {
                    result.contiguous_ready_bytes = 0;
                    result.verified_file_bytes = 0;
                    return result;
                }
                const auto piece_bytes = static_cast<std::uint64_t>(raw_piece_bytes);
                const auto piece_end = piece_start + piece_bytes;
                const auto overlap_start = std::max(route.file_offset, piece_start);
                const auto overlap_end = std::min(file_end, piece_end);
                if (overlap_end <= overlap_start) {
                    continue;
                }
                const auto overlap = overlap_end - overlap_start;
                result.verified_file_bytes += overlap;
                if (contiguous) {
                    result.contiguous_ready_bytes += overlap;
                }
            }
        } catch (...) {
        }
        return result;
    }

    StreamBridgeStats statistics() {
        StreamBridgeStats result{};
        {
            std::lock_guard lock(routes_mutex);
            result.active_streams = saturating_count(routes.size());
            const auto now = std::chrono::steady_clock::now();
            if (now >= next_progress_sample) {
                stream_progress_cache.clear();
                stream_progress_cache.reserve(routes.size());
                for (const auto& [target, route] : routes) {
                    static_cast<void>(target);
                    stream_progress_cache.push_back(progress_for(route));
                }
                next_progress_sample = now + std::chrono::milliseconds(250);
            }
            result.streams = stream_progress_cache;
        }
        result.active_http_requests = server->active_request_count();
        result.active_demands = saturating_count(active_demands.size());
        std::uint64_t pending_reads = 0;
        for (const auto& [key, waiters] : piece_waiters) {
            static_cast<void>(key);
            pending_reads = std::min(
                pending_reads + static_cast<std::uint64_t>(waiters.size()),
                static_cast<std::uint64_t>(std::numeric_limits<std::uint32_t>::max())
            );
        }
        result.pending_piece_reads = static_cast<std::uint32_t>(pending_reads);
        result.cache = piece_cache.stats();
        result.window_capacity_bytes = window_bytes;
        result.window_resident_bytes = window_resident_bytes.load(std::memory_order_relaxed);
        result.window_freed_bytes = window_freed_bytes.load(std::memory_order_relaxed);
        result.window_freed_pieces = window_freed_pieces.load(std::memory_order_relaxed);
        result.window_refetched_pieces =
            window_refetched_pieces.load(std::memory_order_relaxed);
        result.window_unsupported = window_unsupported;
        return result;
    }

    void shutdown() {
        if (shutting_down.exchange(true)) {
            return;
        }
        for (auto& [key, waiters] : piece_waiters) {
            static_cast<void>(key);
            for (const auto& waiter : waiters) {
                complete_waiter(waiter, {}, "engine is shutting down");
            }
        }
        piece_waiters.clear();
        server->stop();
        {
            std::lock_guard lock(commands_mutex);
            commands.clear();
            wake = nullptr;
        }
        for (const auto& [id, schedule] : current_schedules) {
            static_cast<void>(id);
            try {
                schedule.handle.clear_piece_deadlines();
            } catch (...) {
            }
        }
        current_schedules.clear();
        requested_reads.clear();
        partial_blocks.clear();
        anchors.clear();
        last_ready_pieces.clear();
        piece_cache.clear();
        for (auto& [id, demand] : active_demands) {
            static_cast<void>(id);
            if (demand.state) {
                demand.state->cancelled.store(true);
            }
        }
        active_demands.clear();
        expired_streams.clear();
        {
            std::lock_guard lock(routes_mutex);
            routes.clear();
        }
        stream_progress_cache.clear();
    }

    scheduler::StreamWindow window;          // engine thread
    const std::uint64_t disk_capacity;
    const std::chrono::milliseconds inactivity_timeout;
    std::mutex routes_mutex;
    // Stremio Origin: each torrent's save path and each streamed file's path in it
    // (under routes_mutex); bumped when payloads move
    std::unordered_map<std::string, std::filesystem::path> save_paths;
    std::unordered_map<std::string, std::filesystem::path> relative_paths;
    std::atomic<std::uint64_t> storage_generation{0};
    std::unordered_map<std::string, StreamRoute> routes;
    std::vector<BackendStats::Stream> stream_progress_cache;
    std::chrono::steady_clock::time_point next_progress_sample{};
    std::mutex commands_mutex;
    std::deque<HttpCommand> commands;
    std::atomic_uint64_t next_demand_id = 1;
    std::uint64_t next_activity = 1;
    std::unordered_map<std::uint64_t, ActiveDemand> active_demands;
    std::unordered_map<std::string, StreamAnchor> anchors;
    std::unordered_map<std::string, CurrentSchedule> current_schedules;
    std::unordered_map<std::string, std::uint32_t> last_ready_pieces;
    std::unordered_map<PieceKey, std::vector<std::shared_ptr<PieceWaiter>>, PieceKeyHash>
        piece_waiters;
    std::unordered_map<PieceKey, std::chrono::steady_clock::time_point, PieceKeyHash>
        requested_reads;
    std::chrono::steady_clock::time_point next_read_watchdog{};
    std::unordered_map<PieceKey, std::vector<bool>, PieceKeyHash> partial_blocks;
    std::function<void()> wake;
    std::unordered_set<std::string> revoked_torrents;
    std::vector<StoppedStream> expired_streams;
    std::chrono::steady_clock::time_point next_inactivity_check{};
    cache::VerifiedPieceCache piece_cache;
    std::atomic_bool shutting_down = false;
    std::unique_ptr<http::LoopbackServer> server;
    std::atomic_uint64_t window_bytes;           // set on the engine thread, read anywhere
    std::atomic_uint64_t pending_window_bytes = 0; // Stremio Origin: a live change, applied by poll()
    const std::shared_ptr<storage::StreamingWindowStore> window_store;
    std::chrono::steady_clock::time_point next_window_check{};
    std::chrono::steady_clock::time_point next_disk_retry{};
    std::atomic_uint64_t disk_full_retries = 0;
    std::unordered_map<std::string, std::unordered_set<std::uint32_t>> freed_pieces;
    std::atomic_uint64_t window_resident_bytes = 0;
    std::atomic_uint64_t window_freed_bytes = 0;
    std::atomic_uint64_t window_freed_pieces = 0;
    std::atomic_uint64_t window_refetched_pieces = 0;
    std::atomic_bool window_unsupported = false;
    std::string window_unsupported_reason;

    static std::uint32_t saturating_count(const std::size_t count) {
        return static_cast<std::uint32_t>(std::min(
            count,
            static_cast<std::size_t>(std::numeric_limits<std::uint32_t>::max())
        ));
    }
};

LibtorrentStreamBridge::LibtorrentStreamBridge(
    const std::uint16_t requested_port,
    const std::uint64_t memory_capacity_bytes,
    const std::uint64_t disk_capacity_bytes,
    const std::chrono::milliseconds inactivity_timeout,
    const std::uint64_t streaming_window_bytes,
    std::shared_ptr<storage::StreamingWindowStore> window_store
)
    : impl_(std::make_unique<Impl>(
          requested_port,
          memory_capacity_bytes,
          disk_capacity_bytes,
          inactivity_timeout,
          streaming_window_bytes,
          std::move(window_store)
      )) {
}

void LibtorrentStreamBridge::set_wakeup(std::function<void()> wake) {
    impl_->set_wakeup(std::move(wake));
}

LibtorrentStreamBridge::~LibtorrentStreamBridge() = default;

PreparedStream LibtorrentStreamBridge::register_stream(
    const lt::torrent_handle& handle,
    const std::string& torrent_id,
    const std::uint32_t file_index,
    const TorrentFileInfo& file
) {
    return impl_->register_stream(handle, torrent_id, file_index, file);
}

void LibtorrentStreamBridge::poll() {
    impl_->poll();
}

void LibtorrentStreamBridge::set_window_bytes(const std::uint64_t bytes) {
    impl_->set_window_bytes(bytes);
}

void LibtorrentStreamBridge::relocated(const std::string& torrent_id, const std::string& save_path) {
    impl_->relocated(torrent_id, save_path);
}

std::vector<StoppedStream> LibtorrentStreamBridge::pop_expired_streams() {
    return impl_->pop_expired_streams();
}

void LibtorrentStreamBridge::handle_read_piece(const lt::read_piece_alert& alert) {
    impl_->handle_read_piece(alert);
}

void LibtorrentStreamBridge::handle_block_finished(const lt::block_finished_alert& alert) {
    impl_->handle_block_finished(alert);
}

void LibtorrentStreamBridge::handle_hash_failed(const lt::hash_failed_alert& alert) {
    impl_->handle_hash_failed(alert);
}

std::string LibtorrentStreamBridge::stop_stream(const std::string& stream_id) {
    return impl_->stop_stream(stream_id);
}

bool LibtorrentStreamBridge::has_stream_for_torrent(const std::string& torrent_id) {
    return impl_->has_stream_for_torrent(torrent_id);
}

std::vector<std::uint32_t> LibtorrentStreamBridge::blocking_pieces(
    const std::string& torrent_id
) const {
    return impl_->blocking_pieces(torrent_id);
}

void LibtorrentStreamBridge::remove_torrent(const std::string& torrent_id) {
    impl_->remove_torrent(torrent_id);
}

StreamBridgeStats LibtorrentStreamBridge::statistics() {
    return impl_->statistics();
}

void LibtorrentStreamBridge::shutdown() {
    impl_->shutdown();
}

}
