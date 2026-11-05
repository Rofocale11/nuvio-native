// Streaming window, end to end and offline:
//
//   source file (deterministic bytes) -> .torrent with a web seed served by a
//   tiny Range-capable HTTP server in this process -> nuvio_engine with a
//   small streaming_window_bytes -> read the stream URL like a player:
//   sequential to the end, then a seek back into freed pieces.
//
// Asserts: every byte read equals the source; the payload on disk (allocated
// blocks, not the sparse size) never goes past window + margin; pieces were
// actually freed and the seek back downloaded them again.

#include "nuvio_engine/nuvio_engine.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#include <libtorrent/bencode.hpp>
#include <libtorrent/create_torrent.hpp>
#include <libtorrent/file_storage.hpp>

namespace fs = std::filesystem;
using namespace std::chrono_literals;

namespace {

constexpr std::uint64_t mib = 1024 * 1024;
constexpr std::uint64_t file_size = 320 * mib;
constexpr int piece_size = static_cast<int>(1 * mib);
// NUVIO_WINDOW_TEST_MB: the window to test (default 160; Stremio Origin runs it at 48 too)
const std::uint64_t window_bytes = (std::getenv("NUVIO_WINDOW_TEST_MB") ? std::strtoull(std::getenv("NUVIO_WINDOW_TEST_MB"), nullptr, 10) : 160) * mib;
// trims start at 85% of the window; nothing may pass the window itself
constexpr std::uint64_t margin_bytes = 0;
constexpr std::uint64_t read_pace_bytes_per_second = 48 * mib;

fs::path test_root;

[[noreturn]] void fail(const std::string& message) {
    std::cerr << "FAIL: " << message << std::endl;
    // the source and payload are hundreds of MB: never leave them behind
    std::error_code ignored;
    fs::remove_all(test_root, ignored);
    std::_Exit(1);
}

std::uint8_t source_byte(const std::uint64_t offset) {
    std::uint64_t x = (offset / 8 + 1) * 0x9E3779B97F4A7C15ULL;
    x ^= x >> 31;
    x *= 0xBF58476D1CE4E5B9ULL;
    x ^= x >> 29;
    return static_cast<std::uint8_t>(x >> ((offset % 8) * 8));
}

void write_source(const fs::path& path) {
    std::ofstream out(path, std::ios::binary);
    std::vector<char> chunk(mib);
    for (std::uint64_t base = 0; base < file_size; base += mib) {
        for (std::uint64_t i = 0; i < mib; ++i) {
            chunk[i] = static_cast<char>(source_byte(base + i));
        }
        out.write(chunk.data(), static_cast<std::streamsize>(chunk.size()));
    }
}

std::uint64_t allocated_bytes(const fs::path& root) {
    std::uint64_t total = 0;
    std::error_code error;
    for (fs::recursive_directory_iterator it(root, error), end; it != end; it.increment(error)) {
        if (error) {
            break;
        }
        struct stat information {};
        if (::lstat(it->path().c_str(), &information) == 0 && S_ISREG(information.st_mode)) {
            total += static_cast<std::uint64_t>(information.st_blocks) * 512;
        }
    }
    return total;
}

bool send_all(const int socket, const char* data, std::size_t size) {
    while (size > 0) {
        const auto sent = ::send(socket, data, size, 0);
        if (sent <= 0) {
            return false;
        }
        data += sent;
        size -= static_cast<std::size_t>(sent);
    }
    return true;
}

// Web seed: GET with Range, keep-alive, requests answered in order.
class SeedServer {
public:
    explicit SeedServer(fs::path file) : file_(std::move(file)) {
        listener_ = ::socket(AF_INET, SOCK_STREAM, 0);
        const int yes = 1;
        ::setsockopt(listener_, SOL_SOCKET, SO_REUSEADDR, &yes, sizeof(yes));
        sockaddr_in address{};
        address.sin_family = AF_INET;
        address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (::bind(listener_, reinterpret_cast<sockaddr*>(&address), sizeof(address)) != 0 ||
            ::listen(listener_, 16) != 0) {
            fail("seed server bind/listen");
        }
        socklen_t length = sizeof(address);
        ::getsockname(listener_, reinterpret_cast<sockaddr*>(&address), &length);
        port_ = ntohs(address.sin_port);
        thread_ = std::thread([this] { accept_loop(); });
    }

    ~SeedServer() {
        stopping_ = true;
        ::shutdown(listener_, SHUT_RDWR);
        ::close(listener_);
        thread_.join();
        std::lock_guard lock(mutex_);
        for (const auto socket : clients_) {
            ::shutdown(socket, SHUT_RDWR);
        }
        for (auto& worker : workers_) {
            worker.join();
        }
    }

    [[nodiscard]] std::uint16_t port() const { return port_; }
    [[nodiscard]] std::uint64_t served_bytes() const { return served_.load(); }

private:
    void accept_loop() {
        while (!stopping_) {
            const int client = ::accept(listener_, nullptr, nullptr);
            if (client < 0) {
                return;
            }
            std::lock_guard lock(mutex_);
            clients_.push_back(client);
            workers_.emplace_back([this, client] { serve(client); });
        }
    }

    void serve(const int client) {
        std::ifstream in(file_, std::ios::binary);
        std::string pending;
        char buffer[4096];
        std::vector<char> body;
        while (!stopping_) {
            auto end = pending.find("\r\n\r\n");
            while (end == std::string::npos) {
                const auto received = ::recv(client, buffer, sizeof(buffer), 0);
                if (received <= 0) {
                    ::close(client);
                    return;
                }
                pending.append(buffer, static_cast<std::size_t>(received));
                end = pending.find("\r\n\r\n");
            }
            const auto request = pending.substr(0, end);
            pending.erase(0, end + 4);
            std::uint64_t first = 0;
            std::uint64_t last = file_size - 1;
            bool ranged = false;
            std::string lower = request;
            std::ranges::transform(lower, lower.begin(), [](unsigned char c) {
                return static_cast<char>(std::tolower(c));
            });
            if (const auto at = lower.find("range: bytes="); at != std::string::npos) {
                ranged = true;
                unsigned long long a = 0;
                unsigned long long b = 0;
                const auto count = std::sscanf(request.c_str() + at + 13, "%llu-%llu", &a, &b);
                first = a;
                if (count == 2) {
                    last = std::min<std::uint64_t>(b, file_size - 1);
                }
            }
            const auto length = last - first + 1;
            std::string headers = std::string(ranged ? "HTTP/1.1 206 Partial Content\r\n" : "HTTP/1.1 200 OK\r\n") +
                "Content-Length: " + std::to_string(length) + "\r\n" +
                (ranged ? "Content-Range: bytes " + std::to_string(first) + "-" +
                        std::to_string(last) + "/" + std::to_string(file_size) + "\r\n"
                        : std::string()) +
                "Content-Type: application/octet-stream\r\nConnection: keep-alive\r\n\r\n";
            if (!send_all(client, headers.data(), headers.size())) {
                break;
            }
            body.resize(static_cast<std::size_t>(length));
            in.seekg(static_cast<std::streamoff>(first));
            in.read(body.data(), static_cast<std::streamsize>(length));
            if (!send_all(client, body.data(), body.size())) {
                break;
            }
            served_ += length;
        }
        ::close(client);
    }

    fs::path file_;
    int listener_ = -1;
    std::uint16_t port_ = 0;
    std::atomic_bool stopping_ = false;
    std::atomic_uint64_t served_ = 0;
    std::thread thread_;
    std::mutex mutex_;
    std::vector<int> clients_;
    std::vector<std::thread> workers_;
};

struct Engine {
    nuvio_engine* handle = nullptr;
    ~Engine() {
        if (handle != nullptr) {
            nuvio_engine_destroy(handle);
        }
    }
};

nuvio_engine_event wait_event(nuvio_engine* engine, const nuvio_engine_event_type type) {
    const auto deadline = std::chrono::steady_clock::now() + 30s;
    while (std::chrono::steady_clock::now() < deadline) {
        nuvio_engine_event event;
        nuvio_engine_event_init(&event);
        const auto status = nuvio_engine_poll_event(engine, &event);
        if (status == NUVIO_ENGINE_STATUS_OK) {
            if (event.type == type) {
                return event;
            }
            if (event.type == NUVIO_ENGINE_EVENT_TORRENT_ERROR) {
                std::cerr << "  engine event: error " << event.message << std::endl;
            }
            continue;
        }
        std::this_thread::sleep_for(10ms);
    }
    fail("timed out waiting for engine event " + std::to_string(type));
}

nuvio_engine_stats stats_of(nuvio_engine* engine) {
    nuvio_engine_stats stats;
    nuvio_engine_stats_init(&stats);
    if (nuvio_engine_get_stats(engine, &stats) != NUVIO_ENGINE_STATUS_OK) {
        fail("get_stats");
    }
    return stats;
}

struct UrlParts {
    std::uint16_t port = 0;
    std::string path;
};

UrlParts parse_url(const std::string& url) {
    const auto host = url.find("127.0.0.1:");
    if (host == std::string::npos) {
        fail("unexpected stream url " + url);
    }
    const auto colon = host + 10;
    const auto slash = url.find('/', colon);
    return {static_cast<std::uint16_t>(std::stoi(url.substr(colon, slash - colon))), url.substr(slash)};
}

// Reads [first, last] from the stream like a player would (one request,
// paced), compares every byte with the source. Returns bytes verified.
std::uint64_t read_range(const UrlParts& url, const std::uint64_t first, const std::uint64_t last) {
    const int socket = ::socket(AF_INET, SOCK_STREAM, 0);
    sockaddr_in address{};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = htons(url.port);
    if (::connect(socket, reinterpret_cast<sockaddr*>(&address), sizeof(address)) != 0) {
        fail("connect to stream");
    }
    timeval timeout{70, 0};
    ::setsockopt(socket, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    const auto request = "GET " + url.path + " HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes=" +
        std::to_string(first) + "-" + std::to_string(last) + "\r\nConnection: close\r\n\r\n";
    send_all(socket, request.data(), request.size());
    std::string headers;
    char byte = 0;
    while (headers.find("\r\n\r\n") == std::string::npos) {
        if (::recv(socket, &byte, 1, 0) != 1) {
            fail("stream closed before headers");
        }
        headers.push_back(byte);
    }
    if (headers.find(" 206 ") == std::string::npos) {
        fail("stream answered " + headers.substr(0, headers.find('\r')));
    }
    std::vector<char> buffer(256 * 1024);
    auto position = first;
    const auto started = std::chrono::steady_clock::now();
    while (position <= last) {
        const auto want = std::min<std::uint64_t>(buffer.size(), last - position + 1);
        const auto received = ::recv(socket, buffer.data(), static_cast<std::size_t>(want), 0);
        if (received <= 0) {
            fail("stream ended at " + std::to_string(position) + " of " + std::to_string(last + 1) +
                " (" + std::strerror(errno) + ")");
        }
        for (ssize_t i = 0; i < received; ++i) {
            if (static_cast<std::uint8_t>(buffer[static_cast<std::size_t>(i)]) !=
                source_byte(position + static_cast<std::uint64_t>(i))) {
                fail("byte mismatch at offset " + std::to_string(position + static_cast<std::uint64_t>(i)));
            }
        }
        position += static_cast<std::uint64_t>(received);
        // pace like a player: never faster than read_pace_bytes_per_second
        const auto expected = std::chrono::duration<double>(
            static_cast<double>(position - first) / static_cast<double>(read_pace_bytes_per_second)
        );
        const auto elapsed = std::chrono::steady_clock::now() - started;
        if (elapsed < expected) {
            std::this_thread::sleep_for(expected - elapsed);
        }
    }
    ::close(socket);
    return position - first;
}

}

int main() {
    // NUVIO_WINDOW_TEST_DIR: where the ~500 MB of scratch go (default: temp)
    const char* scratch = std::getenv("NUVIO_WINDOW_TEST_DIR");
    const auto root = (scratch != nullptr && scratch[0] != '\0'
        ? fs::path(scratch)
        : fs::temp_directory_path()) / ("nuvio-window-" + std::to_string(::getpid()));
    test_root = root;
    fs::remove_all(root);
    fs::create_directories(root / "seed");
    fs::create_directories(root / "engine-data");
    fs::create_directories(root / "engine-cache");
    const auto source = root / "seed" / "movie.mkv";
    write_source(source);

    SeedServer seed(source);

    lt::file_storage files;
    lt::add_files(files, source.string());
    lt::create_torrent creator(files, piece_size);
    creator.add_url_seed("http://127.0.0.1:" + std::to_string(seed.port()) + "/movie.mkv");
    lt::set_piece_hashes(creator, (root / "seed").string());
    std::vector<char> torrent;
    lt::bencode(std::back_inserter(torrent), creator.generate());

    const auto payload_root = root / "engine-cache";
    Engine engine;
    {
        nuvio_engine_config config;
        nuvio_engine_config_init(&config);
        const auto data = (root / "engine-data").string();
        const auto cache = payload_root.string();
        config.data_directory = data.c_str();
        config.cache_directory = cache.c_str();
        config.upload_mode = NUVIO_ENGINE_UPLOAD_DISABLED;
        config.streaming_window_bytes = window_bytes;
        if (nuvio_engine_create(&config, &engine.handle) != NUVIO_ENGINE_STATUS_OK) {
            fail("nuvio_engine_create");
        }
    }
    if (nuvio_engine_api_version() != 4) {
        fail("API version is not 4");
    }

    nuvio_engine_torrent_request request;
    nuvio_engine_torrent_request_init(&request);
    request.source_type = NUVIO_ENGINE_TORRENT_SOURCE_DATA;
    request.torrent_data = reinterpret_cast<const std::uint8_t*>(torrent.data());
    request.torrent_data_size = torrent.size();
    std::uint64_t request_id = 0;
    if (nuvio_engine_add_torrent(engine.handle, &request, &request_id) != NUVIO_ENGINE_STATUS_OK) {
        fail("add_torrent");
    }
    const auto ready = wait_event(engine.handle, NUVIO_ENGINE_EVENT_TORRENT_METADATA_READY);
    nuvio_engine_stream_request stream;
    nuvio_engine_stream_request_init(&stream);
    stream.torrent_id = ready.torrent_id;
    stream.file_index = 0;
    if (nuvio_engine_prepare_stream(engine.handle, &stream, &request_id) != NUVIO_ENGINE_STATUS_OK) {
        fail("prepare_stream");
    }
    const auto prepared = wait_event(engine.handle, NUVIO_ENGINE_EVENT_STREAM_PREPARED);
    const auto url = parse_url(prepared.stream_url);
    std::cout << "stream " << prepared.stream_url << " (" << prepared.file_size / mib << " MiB)\n";

    std::atomic_bool sampling = true;
    std::atomic_uint64_t peak = 0;
    std::thread sampler([&] {
        while (sampling) {
            const auto now = allocated_bytes(payload_root);
            auto seen = peak.load();
            while (now > seen && !peak.compare_exchange_weak(seen, now)) {
            }
            std::this_thread::sleep_for(20ms);
        }
    });

    const auto t0 = std::chrono::steady_clock::now();
    std::uint64_t verified = 0;
    // 1. header + tail like a player probing the container
    verified += read_range(url, 0, 1 * mib - 1);
    verified += read_range(url, file_size - 2 * mib, file_size - 1);
    // 2. play sequentially to the end
    verified += read_range(url, 0, file_size - 1);
    const auto after_play = stats_of(engine.handle);
    const auto peak_play = peak.load();
    std::cout << "played " << file_size / mib << " MiB: peak on disk " << peak_play / mib
              << " MiB, now " << allocated_bytes(payload_root) / mib << " MiB, freed "
              << after_play.window_freed_pieces << " pieces ("
              << after_play.window_freed_bytes / mib << " MiB)\n";
    // 3. seek back to 25%..40% (freed long ago) and play from there
    const auto back_first = file_size / 4;
    const auto back_last = file_size / 4 * 2 - 1;
    verified += read_range(url, back_first, back_last);
    sampling = false;
    sampler.join();
    const auto after_seek = stats_of(engine.handle);
    const auto elapsed = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();

    std::cout << "seek back " << back_first / mib << ".." << (back_last + 1) / mib
              << " MiB: refetched " << after_seek.window_refetched_pieces
              << " pieces; peak on disk " << peak.load() / mib << " MiB (window "
              << window_bytes / mib << " MiB + margin " << margin_bytes / mib << ")\n";
    std::cout << "verified " << verified / mib << " MiB byte-for-byte; web seed served "
              << seed.served_bytes() / mib << " MiB; " << elapsed << " s\n";

    if (after_seek.window_unsupported) {
        fail("filesystem refused to punch holes");
    }
    if (after_play.window_capacity_bytes != window_bytes) {
        fail("engine did not report the window");
    }
    if (after_play.window_freed_pieces == 0) {
        fail("no piece was freed");
    }
    if (after_seek.window_refetched_pieces == 0) {
        fail("seek back did not download freed pieces again");
    }
    if (peak.load() > window_bytes + margin_bytes) {
        fail("payload on disk went past the window: " + std::to_string(peak.load() / mib) + " MiB");
    }
    engine = Engine{};
    fs::remove_all(root);
    std::cout << "PASS\n";
    return 0;
}
