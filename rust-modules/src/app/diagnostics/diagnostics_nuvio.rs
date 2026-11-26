//! the delivery column's Nuvio-specific rows.
//!
//! The upstream diagnostics panel describes a Plex adaptive session: its right column is nine
//! controller rows, and a URI playback that has no controller shows nine `unknown` placeholders.
//! Nuvio never opens an adaptive session. A torrent is fed by the local libtorrent engine, and a
//! debrid or addon link is a plain download; this child module ports the old fork's exact three-way
//! switch so each shows the facts it actually has — swarm counters for a torrent, file/rate/quality
//! for a link — instead of inert placeholders. `abr_rows` and the controller's own rows stay in the
//! parent and are called from here, so an upstream merge of that code does not conflict.
//!
//! Recorded in `docs/nuvio/MERGE-AUDIT-HISTORY.md`.

use plx_media::player::Diag;
use plx_media::route::PlaybackSession;
use plx_os::i18n::msg;
use plx_stremio::sbengine::EngineStats;
use plx_ui::widgets::Field;

/// the three-way delivery column. A torrent gets the swarm block, a live adaptive
/// session keeps the upstream controller rows, and everything else (a debrid or addon link) gets the
/// short block that says what such a route actually knows. Adapted from the old fork's `model_rows`;
/// the only boundary change is the `now` argument the parent already has on its sampling clock.
pub(super) fn model_rows(ps: &PlaybackSession, d: &Diag, now: u32) -> Vec<Field> {
    let mut v = Vec::with_capacity(super::RIGHT_ROWS);
    match (plx_media::route::is_p2p(ps), plx_stremio::sbengine::stats()) {
        // **A torrent.** File identity, then the swarm. The name and the size get a row each: a
        // release name is long enough that "name · size" wraps onto a second line anyway, and the
        // size is the number a reader compares with what the swarm has delivered.
        (true, Some(s)) => {
            v.push(Field::new(msg::browse_diagnostics_field_file(), file_name(ps)));
            v.push(Field::new(msg::browse_diagnostics_field_size(), file_size(ps)));
            swarm_rows(d, &s, &mut v);
            // The one pitch the swarm block leaves free: the set's own memory, which is the other
            // half of "is this television coping" beside the engine's window and cache.
            v.push(Field::new(msg::browse_diagnostics_field_ram(), ram_value()));
            debug_assert_eq!(v.len(), super::SWARM_ROWS + 3);
        }
        // **A live adaptive session** — never the shim, which serves no transcoder. Its controller
        // rows already spend the column's budget, so this block's File row carries name and size.
        _ if d.abr_mode != 0 => {
            v.push(Field::new(msg::browse_diagnostics_field_file(), file_line(ps)));
            // One selection snapshot for the whole block. A quality press must not leave Mode
            // saying Auto while the lower rows have already formatted the new manual state.
            super::abr_rows(ps, d, plx_media::route::quality(), &mut v);
            debug_assert_eq!(v.len(), super::RIGHT_ROWS);
        }
        // **A direct or debrid link.** No swarm, no adaptive session, so every controller row would
        // be an inert placeholder. These rows are what such a route actually knows: the file, the
        // addon that supplied it, the observed rate, whether that rate can carry the file, the
        // decoder's verdict and the set's memory. The parent starts the chart under the last row.
        _ => {
            v.push(Field::new(msg::browse_diagnostics_field_file(), file_name(ps)));
            v.push(Field::new(msg::browse_diagnostics_field_size(), file_size(ps)));
            v.push(Field::new(
                msg::browse_diagnostics_field_addon(),
                plx_media::route::addon_name(ps),
            ));
            v.push(Field::new(msg::browse_diagnostics_field_rate(), link_rate(d, now)));
            let (headroom, thin) = headroom(d, now);
            v.push(Field::new(msg::browse_diagnostics_field_headroom(), headroom).fault(thin));
            v.push(Field::new(
                msg::browse_diagnostics_field_quality(),
                super::abr_quality(d, plx_media::route::quality()),
            ));
            v.push(Field::new(msg::browse_diagnostics_field_ram(), ram_value()));
            debug_assert_eq!(v.len(), super::LINK_ROWS);
        }
    }
    v
}

/// the DELIVERY column while a torrent plays — what the swarm and the local engine
/// are doing, in the slots the adaptive model would otherwise spend on "controller idle"
/// placeholders. Rates are engine-wide, which during a play is this title's own downloading plus
/// the window it is keeping resident.
fn swarm_rows(d: &Diag, s: &EngineStats, v: &mut Vec<Field>) {
    v.push(Field::new(
        msg::browse_diagnostics_field_peers(),
        msg::browse_diagnostics_peers(
            &s.peers.to_string(),
            &s.connecting.to_string(),
            &s.known.to_string(),
        ),
    ));
    v.push(Field::new(
        msg::browse_diagnostics_field_seeds(),
        msg::browse_diagnostics_seeds(
            &s.peers.max(s.seeds).to_string(),
            &s.seeds.to_string(),
        ),
    ));
    v.push(Field::new(
        msg::browse_diagnostics_field_rate(),
        msg::browse_diagnostics_rate(&super::mb(s.down_bps as i64), &super::mb(s.up_bps as i64)),
    ));
    v.push(Field::new(
        msg::browse_diagnostics_field_window(),
        super::mb(s.window as i64),
    ));
    v.push(Field::new(
        msg::browse_diagnostics_field_engine(),
        msg::browse_diagnostics_engine_io(
            &super::mb(s.session_downloaded as i64),
            &super::mb(s.delivered as i64),
        ),
    ));
    // Pieces the engine had to fetch again after eviction: the one swarm number that says the
    // window is thrashing rather than just working. `Downloaded` is gone — the left column's
    // Transfer row already carries the bytes this playback received — and `Disk` is gone because
    // its "free" was the window budget minus a used figure that may legally exceed it.
    v.push(Field::new(
        msg::browse_diagnostics_field_refetch(),
        s.refetched.to_string(),
    ));
    // The one row of the old block that was never inert: what the decoder actually received.
    v.push(Field::new(
        msg::browse_diagnostics_field_quality(),
        super::abr_quality(d, plx_media::route::quality()),
    ));
}

/// The File row: **which file the route is carrying**. The shim names the release itself; a real
/// PMS would name an absolute path, of which only the last component is useful (and safe) to show.
fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or("").trim()
}

fn file_name(ps: &PlaybackSession) -> String {
    match base_name(&plx_media::route::source_file(ps)) {
        "" => msg::browse_diagnostics_no_file().to_string(),
        name => name.to_string(),
    }
}

/// The Size row's value, in the same units as every other byte figure here. `0` is the plan saying
/// nothing, not an empty file.
fn file_size(ps: &PlaybackSession) -> String {
    size_value(plx_media::route::source_bytes(ps))
}

fn size_value(bytes: i64) -> String {
    if bytes > 0 { super::mb(bytes) } else { "—".to_string() }
}

/// The one-row form, kept for the adaptive-model block: its controller rows plus one File row
/// already spend the column's budget, so that block cannot afford a Size row of its own.
pub(super) fn file_line(ps: &PlaybackSession) -> String {
    file_value(&plx_media::route::source_file(ps), plx_media::route::source_bytes(ps))
}

fn file_value(path: &str, bytes: i64) -> String {
    let name = base_name(path);
    match (name.is_empty(), bytes) {
        (true, _) => msg::browse_diagnostics_no_file().to_string(),
        (false, 0) => name.to_string(),
        (false, b) => format!("{} · {}", name, super::mb(b)),
    }
}

/// A direct/debrid link's delivery rate: bytes handed to FFmpeg over the time since the request
/// opened. A mean, not an instantaneous sample — the chart's NETWORK ACTIVITY lane is the
/// per-sample figure, and a plain HTTP source has no swarm to ask for one.
fn link_rate(d: &Diag, now: u32) -> String {
    let ms = super::since(d.load_at, now);
    if d.net_rx <= 0 || ms < 1_000 {
        return "—".to_string();
    }
    let bytes_per_s = i64::try_from(d.net_rx)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000)
        / i64::from(ms);
    msg::browse_diagnostics_link_rate(&super::mb(bytes_per_s))
}

/// Whether the observed mean delivery rate can carry the file's declared bitrate. Both halves are
/// facts this tree already has: `source_kbps` comes from the resolve, and the observed rate is the
/// one the Rate row reports. Unlike the removed budget/demand lane there is no fictional controller
/// ceiling here — `1.0x` means the file's own average bitrate, nothing more. `fault` marks a link
/// that is on average too thin to sustain it.
fn headroom(d: &Diag, now: u32) -> (String, bool) {
    if d.source_kbps <= 0 {
        return ("—".to_string(), false);
    }
    let ms = super::since(d.load_at, now);
    if d.net_rx <= 0 || ms < 1_000 {
        return ("—".to_string(), false);
    }
    let bytes_per_s = i64::try_from(d.net_rx).unwrap_or(i64::MAX).saturating_mul(1_000) / i64::from(ms);
    let observed_kbps = bytes_per_s.saturating_mul(8) / 1_000;
    let ratio = observed_kbps as f64 / d.source_kbps as f64;
    (
        msg::browse_diagnostics_headroom(
            &format!("{ratio:.1}"),
            &plx_ui::fmt::bitrate(d.source_kbps),
        ),
        // A mean slightly under the file's average is normal for VBR — a 0.9x still plays with a
        // healthy buffer. Only a clearly starved link earns the fault tint.
        ratio < 0.75,
    )
}

fn mem_usage() -> Option<(u64, u64)> {
    parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

/// The RAM row's value, in whole megabytes. `—` where the set does not answer, rather than a zero
/// that would read as an empty machine.
fn ram_value() -> String {
    match mem_usage() {
        Some((used, total)) => {
            const MIB: u64 = 1024 * 1024;
            msg::browse_diagnostics_ram(
                &(total / MIB).to_string(),
                &(used / MIB).to_string(),
            )
        }
        None => "—".to_string(),
    }
}

fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().strip_suffix("kB"))
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|kb| kb * 1024)
    };
    let total = field("MemTotal:")?;
    // A kernel without MemAvailable (or a truncated read) reports everything as used rather than
    // inventing free memory.
    let available = field("MemAvailable:").unwrap_or(0).min(total);
    Some((total - available, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_name_takes_the_last_component_of_either_separator() {
        assert_eq!(base_name("/Volumes/DAS/Movies/A Movie.mkv"), "A Movie.mkv");
        assert_eq!(base_name("C:\\Movies\\A Movie.mkv"), "A Movie.mkv");
        assert_eq!(base_name("   "), "");
    }

    #[test]
    fn file_value_names_the_release_and_only_adds_a_size_when_there_is_one() {
        let no_file = msg::browse_diagnostics_no_file();
        assert_eq!(file_value("", 0), no_file);
        assert_eq!(file_value("/media/Release.Group.mkv", 0), "Release.Group.mkv");
        let with_size = file_value("/media/Release.Group.mkv", 2 * 1024 * 1024);
        assert!(with_size.starts_with("Release.Group.mkv · "), "{with_size}");
    }

    #[test]
    fn link_rate_is_a_mean_over_the_open_duration_and_absent_before_a_second() {
        let d = Diag { load_at: 4_000, net_rx: 2_000_000, ..Default::default() };
        assert_eq!(link_rate(&d, 4_500), "—", "no sample before one second");
        let s = link_rate(&d, 6_000);
        assert!(s.starts_with("down "), "{s}");
    }

    #[test]
    fn meminfo_reads_used_from_available_and_never_invents_memory() {
        assert_eq!(
            parse_meminfo("MemTotal: 2048 kB\nMemFree: 0 kB\nMemAvailable: 512 kB\n"),
            Some((1536 * 1024, 2048 * 1024))
        );
        assert_eq!(
            parse_meminfo("MemTotal: 2048 kB\n"),
            Some((2048 * 1024, 2048 * 1024))
        );
        assert_eq!(parse_meminfo("garbage"), None);
    }

    #[test]
    fn headroom_compares_the_observed_mean_with_the_files_own_bitrate() {
        let d = Diag { load_at: 4_000, net_rx: 5_000_000, source_kbps: 40_000, ..Default::default() };
        // 5 MB over 2 s = 20 Mbit/s against a 40 Mbps file = 0.5x.
        let (value, thin) = headroom(&d, 6_000);
        assert!(thin, "a link half the file's bitrate cannot sustain it");
        assert_eq!(value, "0.5x of 40.0 Mbps", "{value}");
        let marginal = Diag { load_at: 4_000, net_rx: 9_000_000, source_kbps: 40_000, ..Default::default() };
        let (value, thin) = headroom(&marginal, 6_000);
        assert!(!thin, "0.9x of a VBR file with a healthy buffer is not a fault");
        assert!(value.starts_with("0.9x "), "{value}");
        let thin_link = Diag { load_at: 4_000, net_rx: 1, source_kbps: 40_000, ..Default::default() };
        assert!(headroom(&thin_link, 6_000).1);
        let no_bitrate = Diag { load_at: 4_000, net_rx: 5_000_000, source_kbps: 0, ..Default::default() };
        assert_eq!(headroom(&no_bitrate, 6_000), ("—".to_string(), false));
        let no_sample = Diag { load_at: 4_000, net_rx: 0, source_kbps: 40_000, ..Default::default() };
        assert_eq!(headroom(&no_sample, 6_000), ("—".to_string(), false));
    }

    /// The old fork's exact swarm fixture and expectations: seven rows between File/Size and Ram,
    /// with the engine's own words for peers, seeds, rates, window, I/O and refetch.
    #[test]
    fn the_swarm_block_fills_the_model_slots_when_a_torrent_plays() {
        let s = EngineStats {
            peers: 9,
            seeds: 7,
            known: 41,
            connecting: 3,
            down_bps: 2 << 20,
            up_bps: 0,
            downloaded: 108 << 20,
            disk_used: 8 << 20,
            disk_capacity: 100 << 20,
            window: 32 << 20,
            resident: 8 << 20,
            freed: 0,
            refetched: 33,
            mem_cache_used: 0,
            window_unsupported: 0,
            scheduled: 2,
            active_demands: 1,
            delivered: 6 << 20,
            piece_bytes: 4 << 20,
            primary_demand_start: 0,
            secondary_demand_start: 0,
            session_downloaded: 108 << 20,
        };
        let mut v = Vec::new();
        swarm_rows(&Diag::default(), &s, &mut v);
        assert_eq!(v.len(), super::super::SWARM_ROWS, "seven rows between File/Size and Ram");
        let val = |key: &str| v.iter().find(|f| f.key == key).and_then(|f| f.val.as_deref()).unwrap_or("missing");
        assert_eq!(val(msg::browse_diagnostics_field_peers()), "9 connected · 3 connecting · 41 known");
        assert_eq!(val(msg::browse_diagnostics_field_seeds()), "7 of 9 peers");
        assert_eq!(val(msg::browse_diagnostics_field_rate()), "down 2.0 MB/s · up 0 B/s");
        assert_eq!(val(msg::browse_diagnostics_field_window()), "32.0 MB");
        assert_eq!(val(msg::browse_diagnostics_field_engine()), "108.0 MB got · 6.0 MB delivered");
        assert_eq!(val(msg::browse_diagnostics_field_refetch()), "33");
    }
}
