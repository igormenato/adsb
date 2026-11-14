use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::poll::{PollFd, PollTimeout, poll as nix_poll};
use nix::sys::socket::{
    AddressFamily, SockFlag, SockType, SockaddrStorage, connect as nix_connect, getsockopt, socket,
    sockopt,
};
use nix::unistd::{self, AccessFlags, Gid, getegid, getgroups};

pub(super) fn as_fd(fd: &impl AsRawFd) -> BorrowedFd<'_> {
    // SAFETY: `fd` keeps the descriptor open for the lifetime of this borrow.
    unsafe { BorrowedFd::borrow_raw(fd.as_raw_fd()) }
}

pub(super) fn write(fd: impl AsFd, bytes: &[u8]) -> io::Result<usize> {
    unistd::write(fd, bytes).map_err(io::Error::from)
}

pub(super) fn read(fd: impl AsFd, buf: &mut [u8]) -> io::Result<usize> {
    unistd::read(fd, buf).map_err(io::Error::from)
}

pub(super) fn set_nonblocking(fd: impl AsFd) -> io::Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(fd.as_fd(), FcntlArg::F_GETFL)?);
    fcntl(fd.as_fd(), FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

pub(super) fn set_cloexec(fd: impl AsFd) -> io::Result<()> {
    fcntl(fd.as_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    Ok(())
}

pub(super) fn poll(fds: &mut [PollFd<'_>], timeout: Duration) -> io::Result<i32> {
    let timeout = PollTimeout::try_from(timeout).unwrap_or(PollTimeout::MAX);
    loop {
        match nix_poll(fds, timeout) {
            Ok(n) => return Ok(n),
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(err.into()),
        }
    }
}

pub(super) fn tcp_socket(addr: &SocketAddr) -> io::Result<OwnedFd> {
    let family = if addr.is_ipv4() {
        AddressFamily::Inet
    } else {
        AddressFamily::Inet6
    };
    socket(
        family,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK,
        None,
    )
    .map_err(io::Error::from)
}

pub(super) fn connect(fd: &impl AsRawFd, addr: &SocketAddr) -> io::Result<()> {
    nix_connect(fd.as_raw_fd(), &SockaddrStorage::from(*addr)).map_err(io::Error::from)
}

pub(super) fn so_error(fd: impl AsFd) -> io::Result<i32> {
    getsockopt(&fd, sockopt::SocketError).map_err(io::Error::from)
}

pub(super) fn present(path: &Path) -> bool {
    unistd::access(path, AccessFlags::F_OK).is_ok()
}

pub(super) fn writable(path: &Path) -> bool {
    unistd::access(path, AccessFlags::W_OK).is_ok()
}

pub(super) fn groups() -> Vec<u32> {
    let egid = getegid().as_raw();
    let mut out = match getgroups() {
        Ok(gids) => gids.into_iter().map(Gid::as_raw).collect(),
        Err(_) => vec![egid],
    };
    if !out.contains(&egid) {
        out.push(egid);
    }
    out
}
