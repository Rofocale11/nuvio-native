#ifndef NUVIO_ENGINE_LIBTORRENT_STREAM_BRIDGE_HPP
#define NUVIO_ENGINE_LIBTORRENT_STREAM_BRIDGE_HPP

#include <cstdint>
#include <chrono>
#include <functional>
#include <memory>
#include <string>
#include <vector>

#include <libtorrent/fwd.hpp>

#include "cache/verified_piece_cache.hpp"
#include "storage/streaming_window.hpp"
#include "torrent/protocol_backend.hpp"

namespace nuvio::torrent {

struct PreparedStream {
    std::string stream_id;
    std::string url;
};

struct StoppedStream {
    std::string torrent_id;
    std::string stream_id;
};

struct StreamBridgeStats {
    std::uint32_t active_streams = 0;
    std::uint32_t active_http_requests = 0;
    std::uint32_t active_demands = 0;
    std::uint32_t pending_piece_reads = 0;
    cache::PieceCacheStats cache{};
    std::vector<BackendStats::Stream> streams{};
    // streaming window (#334); all zero when the window is off
    std::uint64_t window_capacity_bytes = 0;
    std::uint64_t window_resident_bytes = 0;
    std::uint64_t window_freed_bytes = 0;
    std::uint64_t window_freed_pieces = 0;
    std::uint64_t window_refetched_pieces = 0;
    bool window_unsupported = false;
};

class LibtorrentStreamBridge {
public:
    LibtorrentStreamBridge(
        std::uint16_t requested_port,
        std::uint64_t memory_capacity_bytes,
        std::uint64_t disk_capacity_bytes,
        std::chrono::milliseconds inactivity_timeout,
        std::uint64_t streaming_window_bytes = 0,
        std::shared_ptr<storage::StreamingWindowStore> window_store = {}
    );
    ~LibtorrentStreamBridge();

    LibtorrentStreamBridge(const LibtorrentStreamBridge&) = delete;
    LibtorrentStreamBridge& operator=(const LibtorrentStreamBridge&) = delete;

    [[nodiscard]] PreparedStream register_stream(
        const lt::torrent_handle& handle,
        const std::string& torrent_id,
        std::uint32_t file_index,
        const TorrentFileInfo& file
    );
    void set_wakeup(std::function<void()> wake);
    void poll();
    // Stremio Origin: a new streaming window size (applied by the next poll)
    void set_window_bytes(std::uint64_t bytes);
    // Stremio Origin: the torrent's payload moved to an empty save path (engine thread)
    void relocated(const std::string& torrent_id, const std::string& save_path);
    [[nodiscard]] std::vector<StoppedStream> pop_expired_streams();
    void handle_read_piece(const lt::read_piece_alert& alert);
    void handle_block_finished(const lt::block_finished_alert& alert);
    void handle_hash_failed(const lt::hash_failed_alert& alert);
    [[nodiscard]] std::string stop_stream(const std::string& stream_id);
    [[nodiscard]] bool has_stream_for_torrent(const std::string& torrent_id);
    [[nodiscard]] std::vector<std::uint32_t> blocking_pieces(
        const std::string& torrent_id
    ) const;
    void remove_torrent(const std::string& torrent_id);
    [[nodiscard]] StreamBridgeStats statistics();
    void shutdown();

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

}

#endif
