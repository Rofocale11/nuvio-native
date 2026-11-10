//! The two loopback fixtures every test server and every test that needs a FAILING connection
//! shares: [`accept`] for the one accept a nonblocking listener may make, and [`dead_port`] for
//! a port that refuses connections and cannot be taken away.
//!
//! A fixture makes its listener nonblocking so the acceptor can poll a stop flag or a
//! deadline. Darwin hands the accepted socket the listener's O_NONBLOCK; Linux does not. A
//! reader that assumed blocking I/O then sees `WouldBlock` whenever a parallel suite accepts
//! before the request line is buffered, and a writer can drop a body on a full send buffer —
//! flakes that exist on the Mac only. Every such acceptor accepts through here, so the
//! connection behaves as on Linux and read/write timeouts mean what they say.

/// `listener.accept()`, with the accepted socket put back in blocking mode.
pub fn accept(
    listener: &std::net::TcpListener,
) -> std::io::Result<(std::net::TcpStream, std::net::SocketAddr)> {
    let (socket, peer) = listener.accept()?;
    socket.set_nonblocking(false)?;
    Ok((socket, peer))
}

/// **A loopback port that refuses every connection, and that no other test can take.**
///
/// "Bind `127.0.0.1:0`, read the number, drop the listener" is NOT a refused-connection
/// fixture: the port goes straight back to the host's ephemeral pool, so any of the dozens of
/// mock servers this suite starts — in the same binary or in one of the five `make check` runs
/// beside it — can be handed it, and the connect that was supposed to FAIL then succeeds. It
/// happened: `app::input::menu_play_season_tests::a_failed_season_fetch_opens_the_page_instead_of_playing`
/// read `cur_season == 1` where it asserts `0`, because a neighbour's PMS fixture had answered a
/// season load that its "refused" port was meant to be unable to answer.
///
/// So the port is HELD here instead: a socket is bound to it and never `listen`s. A bound socket
/// with no listener owns the number (no other `bind` can have it — `AddrInUse`), while the kernel
/// answers every inbound SYN with RST, which is exactly the `ECONNREFUSED` callers depend on. One
/// port per process, made on first use and kept for the process's life — the descriptor is
/// deliberately never closed, so do not use this where a test counts descriptors across the
/// FIRST call.
///
/// Use this wherever a test needs a connection to fail, and not where it needs one to be
/// accepted: a test that wants a connection to succeed and then misbehave wants its own listener.
pub fn dead_port() -> u16 {
    static DEAD: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    *DEAD.get_or_init(|| {
        // SAFETY: plain BSD socket calls; every descriptor this creates is owned by this
        // function and either kept for the process's life (success) or the calls below panic.
        unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
            assert!(fd >= 0, "dead_port: socket: {}", std::io::Error::last_os_error());
            // No child of ours should inherit it.
            let _ = libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            let mut sa: libc::sockaddr_in = std::mem::zeroed();
            sa.sin_family = libc::AF_INET as libc::sa_family_t;
            sa.sin_port = 0; // whichever port the host has free
            sa.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
            // Darwin's `sockaddr` carries its own length (and `bind` checks it); Linux's has no
            // such field, so this block is compiled only where the field exists.
            #[cfg(any(
                target_os = "macos",
                target_os = "ios",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
            ))]
            {
                sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
            }
            let rc = libc::bind(
                fd,
                &sa as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            );
            assert_eq!(rc, 0, "dead_port: bind: {}", std::io::Error::last_os_error());
            let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            let rc = libc::getsockname(fd, &mut sa as *mut _ as *mut libc::sockaddr, &mut len);
            assert_eq!(rc, 0, "dead_port: getsockname: {}", std::io::Error::last_os_error());
            // NO `listen()`: that is the whole fixture. A LISTENing socket queues the connection
            // and the client proceeds; a bound socket that never listens answers RST. `fd` is
            // intentionally leaked — closing it would hand the port back to the pool.
            let port = u16::from_be(sa.sin_port);
            assert_ne!(port, 0, "dead_port: the kernel handed back port 0");
            port
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The invariant that fixes the flake**: the fixture's port REFUSES a connection and cannot
    /// be bound by anybody else while the fixture lives. A bound-then-closed port satisfies
    /// neither — see [`dead_port`] for the failure that proved it.
    #[test]
    fn the_dead_port_refuses_and_cannot_be_taken() {
        let port = dead_port();
        assert_eq!(dead_port(), port, "one port per process");
        let err = std::net::TcpStream::connect(("127.0.0.1", port))
            .expect_err("a connection to the fixture must be REFUSED");
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused);
        let taken = std::net::TcpListener::bind(("127.0.0.1", port));
        assert!(taken.is_err(), "a neighbour must not be able to take the fixture's port");
        assert_eq!(taken.unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
    }
}
