// A loopback BitTorrent seeder for `tools/host-engine-check.sh`.
//
// The host check needs a torrent that resolves and serves REAL bytes without depending on a
// public swarm. This binary makes the torrent from one local file, seeds it on 127.0.0.1, and
// prints the info hash + listen port; the script composes a magnet with `x.pe=` so the engine
// under test connects straight to it (no tracker, no DHT).
//
// Usage: host_seed <file> [port]
// Prints HASH=..., HOST=... and PORT=... then READY, and runs until killed.

#include <libtorrent/add_torrent_params.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/bencode.hpp>
#include <libtorrent/create_torrent.hpp>
#include <libtorrent/error_code.hpp>
#include <libtorrent/session.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/torrent_info.hpp>

#include <algorithm>
#include <chrono>
#include <cstdlib>
#include <iostream>
#include <iterator>
#include <memory>
#include <string>
#include <thread>
#include <vector>

namespace lt = libtorrent;

int main(int argc, char** argv) {
    if (argc != 2 && argc != 3) {
        std::cerr << "usage: host_seed <file> [port]\n";
        return 2;
    }
    const std::string file = argv[1];
    const auto slash = file.find_last_of('/');
    const std::string parent = slash == std::string::npos ? "." : file.substr(0, slash);

    lt::error_code ec;
    lt::file_storage fs;
    lt::add_files(fs, file);
    if (fs.num_files() != 1) {
        std::cerr << "host_seed: expected one file, got " << fs.num_files() << "\n";
        return 1;
    }
    lt::create_torrent ct(fs);
    lt::set_piece_hashes(ct, parent, ec);
    if (ec) {
        std::cerr << "host_seed: hashing failed: " << ec.message() << "\n";
        return 1;
    }
    std::vector<char> metadata;
    lt::bencode(std::back_inserter(metadata), ct.generate());
    lt::torrent_info ti(metadata.data(), static_cast<int>(metadata.size()));

    lt::settings_pack sp;
    // ONE explicit port on all interfaces. With port 0 libtorrent expands the listener over
    // every interface (a different port each, device-bound), and on WSL2 the resulting
    // loopback listener refused every TCP connection — the engine then saw only refusals.
    const int wanted_port = argc == 3 ? std::atoi(argv[2]) : 0;
    sp.set_str(
        lt::settings_pack::listen_interfaces,
        "0.0.0.0:" + std::to_string(wanted_port)
    );
    sp.set_bool(lt::settings_pack::enable_dht, false);
    sp.set_bool(lt::settings_pack::enable_lsd, false);
    sp.set_bool(lt::settings_pack::enable_upnp, false);
    sp.set_bool(lt::settings_pack::enable_natpmp, false);
    sp.set_int(
        lt::settings_pack::alert_mask,
        lt::alert_category::error | lt::alert_category::peer
            | lt::alert_category::status | lt::alert_category::connect
    );
    lt::session ses(sp);

    lt::add_torrent_params atp;
    atp.ti = std::make_shared<lt::torrent_info>(ti);
    atp.save_path = parent;
    atp.flags |= lt::torrent_flags::seed_mode;
    ses.async_add_torrent(atp);

    // READY must mean a listener exists. The engine dials the peer once and, if nothing is
    // listening yet, sits out its reconnect floor.
    std::vector<std::string> listen_messages;
    for (int i = 0; i < 200; ++i) {
        std::vector<lt::alert*> alerts;
        ses.pop_alerts(&alerts);
        for (const auto* a : alerts) {
            const std::string message = a->message();
            if (message.find("listening") != std::string::npos
                && message.find("[TCP]") != std::string::npos) {
                listen_messages.push_back(message);
            }
        }
        const bool routable = std::any_of(
            listen_messages.begin(), listen_messages.end(), [](const std::string& message) {
                const auto open = message.find("[TCP] ");
                if (open == std::string::npos) return false;
                const std::string candidate = message.substr(open + 6);
                return candidate.rfind("127.", 0) != 0
                    && candidate.rfind("10.255.255.254:", 0) != 0;
            });
        if (routable) break;
        std::this_thread::sleep_for(std::chrono::milliseconds(25));
    }
    // libtorrent expands the wildcard over every interface and DEVICE-BINDS each listener. A
    // loopback-bound one is unreachable from an ordinary socket in this sandbox (it only
    // accepts clients that bound themselves to `lo`), so advertise a routable interface.
    std::string address;
    for (const auto& message : listen_messages) {
        const auto open = message.find("[TCP] ");
        if (open == std::string::npos) continue;
        const std::string candidate = message.substr(open + 6);
        if (candidate.rfind("127.", 0) == 0 || candidate.rfind("10.255.255.254:", 0) == 0) {
            continue;
        }
        address = candidate;
        break;
    }
    if (address.empty() && !listen_messages.empty()) {
        const auto open = listen_messages.front().find("[TCP] ");
        if (open != std::string::npos) address = listen_messages.front().substr(open + 6);
    }
    const auto colon = address.find_last_of(':');
    const std::string host = colon == std::string::npos ? "" : address.substr(0, colon);
    const int port = colon == std::string::npos ? 0 : std::atoi(address.c_str() + colon + 1);
    if (port == 0) {
        std::cerr << "host_seed: session never started listening\n";
        return 1;
    }

    // Diagnostic only: the check prints this on failure to explain a peerless-looking seed.
    std::thread([&ses] {
        for (;;) {
            std::vector<lt::alert*> alerts;
            ses.pop_alerts(&alerts);
            for (const auto* a : alerts) {
                const std::string message = a->message();
                if (message.find("block ") == std::string::npos) {
                    std::cerr << "ALERT: " << message << "\n";
                }
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(200));
        }
    }).detach();

    std::cout << "HASH=" << ti.info_hashes().v1 << "\n"
              << "HOST=" << host << "\n"
              << "PORT=" << port << "\n"
              << "READY\n"
              << std::flush;
    for (;;) std::this_thread::sleep_for(std::chrono::seconds(1));
}
