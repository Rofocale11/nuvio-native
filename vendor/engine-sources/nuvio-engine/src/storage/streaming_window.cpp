#include "storage/streaming_window.hpp"

#include <algorithm>
#include <cerrno>
#include <cstring>
#include <functional>
#include <map>
#include <unordered_set>
#include <utility>

#include <boost/asio/error.hpp>
#include <boost/asio/post.hpp>

#include <libtorrent/disk_interface.hpp>
#include <libtorrent/file_storage.hpp>
#include <libtorrent/hex.hpp>
#include <libtorrent/peer_request.hpp>
#include <libtorrent/posix_disk_io.hpp>
#include <libtorrent/storage_defs.hpp>
#include <libtorrent/torrent_status.hpp>

#if !defined(_WIN32)
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace nuvio::storage {

std::string StreamingWindowStore::key_for(const std::string& torrent_id) {
    return torrent_id.substr(0, std::min<std::size_t>(torrent_id.size(), 40));
}

void StreamingWindowStore::set_segment_bytes(const std::uint64_t bytes) {
    std::lock_guard lock(mutex_);
    segment_bytes_ = bytes;
}

std::uint64_t StreamingWindowStore::segment_bytes() const {
    std::lock_guard lock(mutex_);
    return segment_bytes_;
}

void StreamingWindowStore::attach(
    const int storage,
    const std::string& torrent_id,
    std::shared_ptr<const lt::file_storage> segments,
    std::string save_path,
    const std::uint32_t segment_pieces
) {
    std::lock_guard lock(mutex_);
    const auto key = key_for(torrent_id);
    storages_.insert_or_assign(storage, key);
    if (segments) {
        auto& torrent = torrents_[key];
        torrent.segments = std::move(segments);
        torrent.save_path = std::move(save_path);
        torrent.segment_pieces = std::max<std::uint32_t>(segment_pieces, 1);
    }
}

void StreamingWindowStore::detach(const int storage) {
    std::lock_guard lock(mutex_);
    const auto found = storages_.find(storage);
    if (found == storages_.end()) {
        return;
    }
    const auto key = found->second;
    storages_.erase(found);
    const auto still_used = std::ranges::any_of(storages_, [&](const auto& entry) {
        return entry.second == key;
    });
    if (!still_used) {
        torrents_.erase(key);
    }
}

bool StreamingWindowStore::is_freed(const int storage, const std::uint32_t piece) const {
    std::lock_guard lock(mutex_);
    const auto found = storages_.find(storage);
    if (found == storages_.end()) {
        return false;
    }
    const auto torrent = torrents_.find(found->second);
    return torrent != torrents_.end() && torrent->second.freed.contains(piece);
}

void StreamingWindowStore::note_write(const int storage, const std::uint32_t piece) {
    std::lock_guard lock(mutex_);
    const auto found = storages_.find(storage);
    if (found == storages_.end()) {
        return;
    }
    auto& torrent = torrents_[found->second];
    // the piece is being downloaded again: it is no longer freed. Its bytes are
    // not counted as resident yet — only note_verified does that, once the hash
    // passes — so an in-flight partial piece cannot inflate the window.
    torrent.freed.erase(piece);
}

void StreamingWindowStore::note_verified(const int storage, const std::uint32_t piece) {
    std::lock_guard lock(mutex_);
    const auto found = storages_.find(storage);
    if (found == storages_.end()) {
        return;
    }
    torrents_[found->second].resident.insert(piece);
}

std::vector<std::uint32_t> StreamingWindowStore::resident_pieces(
    const std::string& torrent_id
) const {
    std::lock_guard lock(mutex_);
    const auto torrent = torrents_.find(key_for(torrent_id));
    if (torrent == torrents_.end()) {
        return {};
    }
    return {torrent->second.resident.begin(), torrent->second.resident.end()};
}

bool StreamingWindowStore::mark_freed(const std::string& torrent_id, const std::uint32_t piece) {
    std::lock_guard lock(mutex_);
    const auto torrent = torrents_.find(key_for(torrent_id));
    if (torrent == torrents_.end() || torrent->second.resident.erase(piece) == 0) {
        return false;
    }
    torrent->second.freed.insert(piece);
    return true;
}

void StreamingWindowStore::unmark_freed(const std::string& torrent_id, const std::uint32_t piece) {
    std::lock_guard lock(mutex_);
    const auto torrent = torrents_.find(key_for(torrent_id));
    if (torrent == torrents_.end() || torrent->second.freed.erase(piece) == 0) {
        return;
    }
    torrent->second.resident.insert(piece);
}

bool StreamingWindowStore::is_freed(const std::string& torrent_id, const std::uint32_t piece) const {
    std::lock_guard lock(mutex_);
    const auto torrent = torrents_.find(key_for(torrent_id));
    return torrent != torrents_.end() && torrent->second.freed.contains(piece);
}

std::optional<StreamingWindowStore::Location> StreamingWindowStore::segment_location(
    const std::string& torrent_id,
    const std::uint64_t absolute
) const {
    std::lock_guard lock(mutex_);
    const auto torrent = torrents_.find(key_for(torrent_id));
    if (torrent == torrents_.end() || !torrent->second.segments) {
        return std::nullopt;
    }
    const auto& files = *torrent->second.segments;
    if (absolute >= static_cast<std::uint64_t>(files.total_size())) {
        return std::nullopt;
    }
    const auto index = files.file_index_at_offset(static_cast<std::int64_t>(absolute));
    const auto start = static_cast<std::uint64_t>(files.file_offset(index));
    const auto size = static_cast<std::uint64_t>(files.file_size(index));
    return Location{
        files.file_path(index, torrent->second.save_path),
        absolute - start,
        start + size - absolute,
    };
}

bool StreamingWindowStore::release_segment(const std::string& torrent_id, const std::uint32_t piece) {
    // under the lock: a write to this segment marks its piece resident (also
    // under the lock) before it starts, so a file is never deleted under a
    // write, and a write after the delete simply creates the file again
    std::lock_guard lock(mutex_);
    const auto found = torrents_.find(key_for(torrent_id));
    if (found == torrents_.end() || !found->second.segments) {
        return false;
    }
    const auto& torrent = found->second;
    const auto& files = *torrent.segments;
    const auto per = torrent.segment_pieces;
    const auto first = piece / per * per;
    const auto last = std::min<std::uint64_t>(
        static_cast<std::uint64_t>(first) + per,
        static_cast<std::uint64_t>(std::max(files.num_pieces(), 0))
    );
    for (auto other = static_cast<std::uint64_t>(first); other < last; ++other) {
        if (torrent.resident.contains(static_cast<std::uint32_t>(other))) {
            return true;
        }
    }
    const auto piece_length = static_cast<std::uint64_t>(files.piece_length());
    const auto begin = static_cast<std::uint64_t>(first) * piece_length;
    const auto end = std::min<std::uint64_t>(last * piece_length, static_cast<std::uint64_t>(files.total_size()));
    if (begin >= end) {
        return true;
    }
    for (auto index = files.file_index_at_offset(static_cast<std::int64_t>(begin));
         index < files.end_file() && static_cast<std::uint64_t>(files.file_offset(index)) < end;
         ++index) {
        if (files.pad_file_at(index)) {
            continue;
        }
        std::error_code ignored;
        std::filesystem::remove(files.file_path(index, torrent.save_path), ignored);
    }
    return true;
}

void StreamingWindowStore::relocate(const std::string& torrent_id, const std::string& save_path) {
    std::lock_guard lock(mutex_);
    const auto found = torrents_.find(key_for(torrent_id));
    if (found == torrents_.end()) {
        return;
    }
    found->second.resident.clear();
    found->second.freed.clear();
    found->second.save_path = save_path;
}

namespace {

class WindowDiskIo final : public lt::disk_interface {
public:
    WindowDiskIo(
        lt::io_context& io,
        const lt::settings_interface& settings,
        lt::counters& counters,
        std::shared_ptr<StreamingWindowStore> store
    )
        : io_(io),
          inner_(lt::posix_disk_io_constructor(io, settings, counters)),
          store_(std::move(store)) {
    }

    ~WindowDiskIo() override {
        // the inner holders call back into inner_, which must still exist
        holders_.clear();
    }

    lt::storage_holder new_torrent(
        const lt::storage_params& params,
        const std::shared_ptr<void>& torrent
    ) override {
        const auto segment_bytes = store_->segment_bytes();
        const auto& files = params.files;
        if (segment_bytes == 0 || params.mapped_files || files.num_pieces() <= 0 ||
            files.piece_length() <= 0 || files.total_size() <= 0) {
            auto inner = inner_->new_torrent(params, torrent);
            const lt::storage_index_t index = inner;
            store_->attach(static_cast<int>(index), lt::aux::to_hex(params.info_hash));
            holders_.insert_or_assign(index, std::move(inner));
            return lt::storage_holder(index, *this);
        }
        // segments of whole pieces, split where the torrent's own files meet
        // (a segment is then one or a few files, all deleted together); pad
        // files stay pad files, never written
        const auto piece_length = static_cast<std::int64_t>(files.piece_length());
        const auto per = std::max<std::int64_t>(1, static_cast<std::int64_t>(segment_bytes) / piece_length);
        const auto segment = per * piece_length;
        auto segments = std::make_shared<lt::file_storage>();
        auto origin = std::make_shared<std::vector<lt::file_index_t>>(); // segment file -> the torrent's file
        segments->set_piece_length(files.piece_length());
        segments->set_name("seg");
        for (const auto file : files.file_range()) {
            auto offset = files.file_offset(file);
            auto left = files.file_size(file);
            if (left <= 0) {
                continue;
            }
            if (files.pad_file_at(file)) {
                segments->add_file("seg/pad" + std::to_string(static_cast<int>(file)), left, lt::file_storage::flag_pad_file);
                origin->push_back(file);
                continue;
            }
            while (left > 0) {
                const auto number = offset / segment;
                const auto bytes = std::min(left, (number + 1) * segment - offset);
                segments->add_file(
                    "seg/" + std::to_string(number) + "-" + std::to_string(static_cast<int>(file)),
                    bytes
                );
                origin->push_back(file);
                offset += bytes;
                left -= bytes;
            }
        }
        segments->set_num_pieces(files.num_pieces());
        // priorities are per file of the torrent, not per segment: every
        // written byte goes to a segment (no part file)
        const lt::aux::vector<lt::download_priority_t, lt::file_index_t> none;
        const lt::storage_params mapped(files, segments.get(), params.path, params.mode, none, params.info_hash);
        auto inner = inner_->new_torrent(mapped, torrent);
        const lt::storage_index_t index = inner;
        store_->attach(
            static_cast<int>(index),
            lt::aux::to_hex(params.info_hash),
            segments,
            params.path,
            static_cast<std::uint32_t>(per)
        );
        segmented_.insert(index);
        origin_.insert_or_assign(index, std::move(origin));
        holders_.insert_or_assign(index, std::move(inner));
        return lt::storage_holder(index, *this);
    }

    void remove_torrent(const lt::storage_index_t index) override {
        store_->detach(static_cast<int>(index));
        segmented_.erase(index);
        origin_.erase(index);
        holders_.erase(index);
    }

    void async_read(
        const lt::storage_index_t storage,
        const lt::peer_request& request,
        std::function<void(lt::disk_buffer_holder, const lt::storage_error&)> handler,
        const lt::disk_job_flags_t flags
    ) override {
        if (store_->is_freed(static_cast<int>(storage), piece_of(request))) {
            // Never hand out the hole. operation_aborted makes libtorrent
            // reject the peer request (and send dont_have) without pausing
            // the torrent; read_piece reports the error to the bridge.
            boost::asio::post(io_, [handler = std::move(handler)] {
                handler(
                    lt::disk_buffer_holder{},
                    lt::storage_error(
                        lt::error_code(boost::asio::error::operation_aborted),
                        lt::operation_t::file_read
                    )
                );
            });
            return;
        }
        if (const auto origin = origin_of(storage)) {
            handler = [handler = std::move(handler), origin](lt::disk_buffer_holder buffer, const lt::storage_error& error) mutable {
                handler(std::move(buffer), to_torrent(error, *origin));
            };
        }
        inner_->async_read(storage, request, std::move(handler), flags);
    }

    bool async_write(
        const lt::storage_index_t storage,
        const lt::peer_request& request,
        const char* buffer,
        std::shared_ptr<lt::disk_observer> observer,
        std::function<void(const lt::storage_error&)> handler,
        const lt::disk_job_flags_t flags
    ) override {
        store_->note_write(static_cast<int>(storage), piece_of(request));
        if (const auto origin = origin_of(storage)) {
            handler = [handler = std::move(handler), origin](const lt::storage_error& error) { handler(to_torrent(error, *origin)); };
        }
        return inner_->async_write(
            storage,
            request,
            buffer,
            std::move(observer),
            std::move(handler),
            flags
        );
    }

    void async_hash(
        const lt::storage_index_t storage,
        const lt::piece_index_t piece,
        const lt::span<lt::sha256_hash> v2,
        const lt::disk_job_flags_t flags,
        std::function<void(lt::piece_index_t, const lt::sha1_hash&, const lt::storage_error&)> handler
    ) override {
        if (const auto origin = origin_of(storage)) {
            handler = [handler = std::move(handler), origin](lt::piece_index_t p, const lt::sha1_hash& h, const lt::storage_error& error) {
                handler(p, h, to_torrent(error, *origin));
            };
        }
        // Only verified pieces count toward the resident window: an in-flight
        // partial cannot be freed (punching it would corrupt the file), so
        // counting it inflated resident far past the cap during fast downloads.
        const auto store = store_;
        const auto storage_index = static_cast<int>(storage);
        const auto piece_index = static_cast<std::uint32_t>(static_cast<int>(piece));
        handler = [store, storage_index, piece_index, handler = std::move(handler)](
            lt::piece_index_t p,
            const lt::sha1_hash& h,
            const lt::storage_error& error
        ) mutable {
            if (!error) {
                store->note_verified(storage_index, piece_index);
            }
            handler(p, h, error);
        };
        inner_->async_hash(storage, piece, v2, flags, std::move(handler));
    }

    void async_hash2(
        const lt::storage_index_t storage,
        const lt::piece_index_t piece,
        const int offset,
        const lt::disk_job_flags_t flags,
        std::function<void(lt::piece_index_t, const lt::sha256_hash&, const lt::storage_error&)> handler
    ) override {
        if (const auto origin = origin_of(storage)) {
            handler = [handler = std::move(handler), origin](lt::piece_index_t p, const lt::sha256_hash& h, const lt::storage_error& error) {
                handler(p, h, to_torrent(error, *origin));
            };
        }
        inner_->async_hash2(storage, piece, offset, flags, std::move(handler));
    }

    void async_move_storage(
        const lt::storage_index_t storage,
        std::string path,
        const lt::move_flags_t flags,
        std::function<void(lt::status_t, const std::string&, const lt::storage_error&)> handler
    ) override {
        if (const auto origin = origin_of(storage)) {
            handler = [handler = std::move(handler), origin](lt::status_t st, const std::string& p, const lt::storage_error& error) {
                handler(st, p, to_torrent(error, *origin));
            };
        }
        inner_->async_move_storage(storage, std::move(path), flags, std::move(handler));
    }

    void async_release_files(
        const lt::storage_index_t storage,
        std::function<void()> handler
    ) override {
        inner_->async_release_files(storage, std::move(handler));
    }

    void async_check_files(
        const lt::storage_index_t storage,
        const lt::add_torrent_params* resume_data,
        lt::aux::vector<std::string, lt::file_index_t> links,
        std::function<void(lt::status_t, const lt::storage_error&)> handler
    ) override {
        if (const auto origin = origin_of(storage)) {
            handler = [handler = std::move(handler), origin](lt::status_t st, const lt::storage_error& error) {
                handler(st, to_torrent(error, *origin));
            };
        }
        inner_->async_check_files(storage, resume_data, std::move(links), std::move(handler));
    }

    void async_stop_torrent(
        const lt::storage_index_t storage,
        std::function<void()> handler
    ) override {
        inner_->async_stop_torrent(storage, std::move(handler));
    }

    void async_rename_file(
        const lt::storage_index_t storage,
        const lt::file_index_t index,
        std::string name,
        std::function<void(const std::string&, lt::file_index_t, const lt::storage_error&)> handler
    ) override {
        if (const auto origin = origin_of(storage)) {
            handler = [handler = std::move(handler), origin](const std::string& n, lt::file_index_t f, const lt::storage_error& error) {
                handler(n, f, to_torrent(error, *origin));
            };
        }
        inner_->async_rename_file(storage, index, std::move(name), std::move(handler));
    }

    void async_delete_files(
        const lt::storage_index_t storage,
        const lt::remove_flags_t options,
        std::function<void(const lt::storage_error&)> handler
    ) override {
        if (const auto origin = origin_of(storage)) {
            handler = [handler = std::move(handler), origin](const lt::storage_error& error) { handler(to_torrent(error, *origin)); };
        }
        inner_->async_delete_files(storage, options, std::move(handler));
    }

    void async_set_file_priority(
        const lt::storage_index_t storage,
        lt::aux::vector<lt::download_priority_t, lt::file_index_t> priorities,
        std::function<void(const lt::storage_error&, lt::aux::vector<lt::download_priority_t, lt::file_index_t>)> handler
    ) override {
        if (segmented_.contains(storage)) {
            // segments have no per-file part files: taken as they are
            boost::asio::post(io_, [handler = std::move(handler), priorities = std::move(priorities)]() mutable {
                handler(lt::storage_error{}, std::move(priorities));
            });
            return;
        }
        inner_->async_set_file_priority(storage, std::move(priorities), std::move(handler));
    }

    void async_clear_piece(
        const lt::storage_index_t storage,
        const lt::piece_index_t index,
        std::function<void(lt::piece_index_t)> handler
    ) override {
        inner_->async_clear_piece(storage, index, std::move(handler));
    }

    void update_stats_counters(lt::counters& counters) const override {
        inner_->update_stats_counters(counters);
    }

    std::vector<lt::open_file_state> get_status(const lt::storage_index_t storage) const override {
        return inner_->get_status(storage);
    }

    void abort(const bool wait) override {
        inner_->abort(wait);
    }

    void submit_jobs() override {
        inner_->submit_jobs();
    }

    void settings_updated() override {
        inner_->settings_updated();
    }

private:
    using Origin = std::shared_ptr<const std::vector<lt::file_index_t>>;
    Origin origin_of(const lt::storage_index_t storage) const {
        const auto found = origin_.find(storage);
        return found == origin_.end() ? Origin{} : found->second;
    }
    // Stremio Origin: libtorrent reads an error's file index against the
    // torrent's own files; in segments it is a segment's index (one past the
    // torrent's files read out of bounds when a drive vanished)
    static lt::storage_error to_torrent(const lt::storage_error& error, const std::vector<lt::file_index_t>& origin) {
        auto mapped = error;
        const auto file = static_cast<int>(error.file());
        if (file >= 0) {
            mapped.file(file < static_cast<int>(origin.size()) ? origin[static_cast<std::size_t>(file)] : lt::torrent_status::error_file_none);
        }
        return mapped;
    }

    static std::uint32_t piece_of(const lt::peer_request& request) {
        return static_cast<std::uint32_t>(static_cast<int>(request.piece));
    }

    lt::io_context& io_;
    std::unique_ptr<lt::disk_interface> inner_;
    std::shared_ptr<StreamingWindowStore> store_;
    std::map<lt::storage_index_t, lt::storage_holder> holders_;
    std::set<lt::storage_index_t> segmented_;
    std::map<lt::storage_index_t, Origin> origin_;
};

}

std::unique_ptr<lt::disk_interface> make_window_disk_io(
    lt::io_context& io,
    const lt::settings_interface& settings,
    lt::counters& counters,
    std::shared_ptr<StreamingWindowStore> store
) {
    return std::make_unique<WindowDiskIo>(io, settings, counters, std::move(store));
}

#if defined(__linux__)
#ifndef FALLOC_FL_KEEP_SIZE
#define FALLOC_FL_KEEP_SIZE 0x01
#endif
#ifndef FALLOC_FL_PUNCH_HOLE
#define FALLOC_FL_PUNCH_HOLE 0x02
#endif
#endif

bool punch_hole(
    const std::filesystem::path& path,
    const std::uint64_t offset,
    const std::uint64_t length,
    std::string* const error
) {
#if defined(_WIN32)
    static_cast<void>(path);
    static_cast<void>(offset);
    static_cast<void>(length);
    if (error) {
        *error = "punch hole is not implemented on Windows";
    }
    return false;
#else
    const auto fail = [&](const char* step) {
        if (error) {
            *error = std::string(step) + ": " + std::strerror(errno);
        }
        return false;
    };
#if defined(__linux__)
    // open64/fstat64/fallocate64: off_t is 32-bit on armv7 without
    // _FILE_OFFSET_BITS=64, and the file is larger than 2 GB (#202).
    const int descriptor = ::open64(path.c_str(), O_WRONLY | O_CLOEXEC);
    if (descriptor < 0) {
        return fail("open");
    }
    struct stat64 information {};
    if (::fstat64(descriptor, &information) != 0) {
        const auto result = fail("fstat");
        ::close(descriptor);
        return result;
    }
#else
    const int descriptor = ::open(path.c_str(), O_WRONLY | O_CLOEXEC);
    if (descriptor < 0) {
        return fail("open");
    }
    struct stat information {};
    if (::fstat(descriptor, &information) != 0) {
        const auto result = fail("fstat");
        ::close(descriptor);
        return result;
    }
#endif
    const auto block = static_cast<std::uint64_t>(
        information.st_blksize > 0 ? information.st_blksize : 4096
    );
    const auto begin = (offset + block - 1) / block * block;
    const auto end = (offset + length) / block * block;
    if (end <= begin) {
        ::close(descriptor);
        return true;
    }
#if defined(__linux__)
    const int result = ::fallocate64(
        descriptor,
        FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE,
        static_cast<off64_t>(begin),
        static_cast<off64_t>(end - begin)
    );
#elif defined(__APPLE__)
    fpunchhole_t hole{};
    hole.fp_flags = 0;
    hole.fp_offset = static_cast<off_t>(begin);
    hole.fp_length = static_cast<off_t>(end - begin);
    const int result = ::fcntl(descriptor, F_PUNCHHOLE, &hole);
#else
    errno = ENOTSUP;
    const int result = -1;
#endif
    if (result != 0) {
        const auto failed = fail("punch");
        ::close(descriptor);
        return failed;
    }
    ::close(descriptor);
    return true;
#endif
}

}
