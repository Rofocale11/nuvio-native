#ifndef NUVIO_ENGINE_STREAMING_WINDOW_HPP
#define NUVIO_ENGINE_STREAMING_WINDOW_HPP

// Streaming window: keep only a window of the file on
// disk while it plays. Two halves share this store:
//
//  - the disk back-end (WindowDiskIo, libtorrent network thread) records which
//    pieces were written and refuses to read a piece that was freed: the read
//    fails, it never returns the zeros of the hole;
//  - the stream bridge (engine thread) picks pieces far from the playhead,
//    marks them freed here, punches them out of the file and then tells
//    libtorrent to forget them (torrent_handle::nuvio_drop_piece).
//
// A freed piece that is needed again (seek backwards) is simply downloaded
// again: libtorrent no longer has it, so the scheduler asks for it like any
// other missing piece, and the first write clears the freed mark.
//
// Stremio Origin: storage that cannot free bytes inside a file and caps files at
// 4 GB (FAT32, most USB sticks) keeps a torrent as segment files of a few
// whole pieces each instead of its own files (libtorrent's mapped files: the
// same bytes, laid out differently). A segment file is deleted once none of
// its pieces is kept, which frees what a punched hole would.

#include <cstdint>
#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include <libtorrent/fwd.hpp>
#include <libtorrent/io_context.hpp>

namespace nuvio::storage {

class StreamingWindowStore {
public:
    // Keys are hex(info_hash_t::get_best()): the hash libtorrent hands the
    // disk back-end (v2 truncated for hybrid/v2 torrents, v1 otherwise).
    static std::string key_for(const std::string& torrent_id);

    // segment files of about this many bytes for torrents added from now on;
    // 0 = the torrent's own files, freed by punching holes
    void set_segment_bytes(std::uint64_t bytes);
    [[nodiscard]] std::uint64_t segment_bytes() const;

    // disk back-end side (libtorrent network thread); segments is null for a
    // torrent kept in its own files
    void attach(
        int storage,
        const std::string& torrent_id,
        std::shared_ptr<const lt::file_storage> segments = {},
        std::string save_path = {},
        std::uint32_t segment_pieces = 0
    );
    void detach(int storage);
    [[nodiscard]] bool is_freed(int storage, std::uint32_t piece) const;
    // a block is being written: the piece is no longer freed (it is being
    // re-downloaded). Does NOT mark it resident yet — only verified pieces
    // count, so in-flight partials cannot inflate the window past its cap.
    void note_write(int storage, std::uint32_t piece);
    // the piece hashed cleanly: it is now verified payload and counts toward
    // the resident window, so the trim can free it once it leaves the playhead.
    void note_verified(int storage, std::uint32_t piece);

    // bridge side
    [[nodiscard]] std::vector<std::uint32_t> resident_pieces(const std::string& torrent_id) const;
    // moves a resident piece to "freed"; false if it was not resident
    bool mark_freed(const std::string& torrent_id, std::uint32_t piece);
    // undo mark_freed when the bytes could not be released
    void unmark_freed(const std::string& torrent_id, std::uint32_t piece);
    [[nodiscard]] bool is_freed(const std::string& torrent_id, std::uint32_t piece) const;

    struct Location {
        std::string path;
        std::uint64_t offset = 0;  // in that file
        std::uint64_t length = 0;  // bytes from there to the file's end
    };
    // where a torrent byte lives when the torrent is in segments (nullopt
    // when it is in its own files)
    [[nodiscard]] std::optional<Location> segment_location(
        const std::string& torrent_id,
        std::uint64_t absolute
    ) const;
    // after mark_freed: the piece's segment files go once none of the
    // segment's pieces is kept; false when the torrent is not in segments
    bool release_segment(const std::string& torrent_id, std::uint32_t piece);
    // the storage moved to an empty folder: nothing is kept any more
    void relocate(const std::string& torrent_id, const std::string& save_path);

private:
    struct Torrent {
        std::set<std::uint32_t> resident;
        std::unordered_set<std::uint32_t> freed;
        std::shared_ptr<const lt::file_storage> segments;
        std::string save_path;
        std::uint32_t segment_pieces = 0;
    };

    mutable std::mutex mutex_;
    std::unordered_map<int, std::string> storages_;
    std::unordered_map<std::string, Torrent> torrents_;
    std::uint64_t segment_bytes_ = 0;
};

// posix_disk_io wrapped with the store above.
std::unique_ptr<lt::disk_interface> make_window_disk_io(
    lt::io_context& io,
    const lt::settings_interface& settings,
    lt::counters& counters,
    std::shared_ptr<StreamingWindowStore> store
);

// Frees [offset, offset + length) of the file without changing its size.
// Only whole filesystem blocks inside the range are released. 64-bit offsets
// on every ABI (armv7 included). Returns false (and errno-style text in
// *error) when the filesystem cannot do it.
bool punch_hole(
    const std::filesystem::path& path,
    std::uint64_t offset,
    std::uint64_t length,
    std::string* error
);

}

#endif
