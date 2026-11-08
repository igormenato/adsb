use std::io;
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

pub(super) fn write(fd: RawFd, bytes: &[u8]) -> io::Result<usize> {
    // SAFETY: `fd` is open. `bytes` is a live slice. `write` does not keep the pointer.
    let rc = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc as usize)
    }
}

pub(super) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is open. F_GETFL only reads flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is the same open fd. The flags word came from F_GETFL on it.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn pollfd(fd: RawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

pub(super) fn poll(fds: &mut [libc::pollfd], timeout: Duration) -> io::Result<i32> {
    let millis = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    loop {
        // SAFETY: `fds` is a live `pollfd` slice. `nfds` equals that length.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(rc);
    }
}

pub(super) struct SocketFd(OwnedFd);

impl SocketFd {
    pub(super) fn new(domain: libc::c_int) -> io::Result<Self> {
        // SAFETY: `socket` returns a new fd or -1. SOCK_CLOEXEC keeps it out of exec.
        let fd = unsafe {
            libc::socket(
                domain,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a socket this function uniquely owns.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    pub(super) fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    pub(super) fn into_stream(self) -> TcpStream {
        TcpStream::from(self.0)
    }
}

pub(super) fn connect(fd: RawFd, addr: &SocketAddr) -> io::Result<()> {
    let rc = match *addr {
        SocketAddr::V4(addr) => {
            let raw = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: addr.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(*addr.ip()).to_be(),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: `fd` is an AF_INET socket. `raw` is a sockaddr_in of the given size.
            unsafe {
                libc::connect(
                    fd,
                    (&raw as *const libc::sockaddr_in).cast(),
                    size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
        }
        SocketAddr::V6(addr) => {
            let raw = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: addr.port().to_be(),
                sin6_flowinfo: addr.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: addr.ip().octets(),
                },
                sin6_scope_id: addr.scope_id(),
            };
            // SAFETY: `fd` is an AF_INET6 socket. `raw` is a sockaddr_in6 of the given size.
            unsafe {
                libc::connect(
                    fd,
                    (&raw as *const libc::sockaddr_in6).cast(),
                    size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        }
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn so_error(fd: RawFd) -> io::Result<i32> {
    let mut so_error: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `fd` is an open socket. The buffer is a `c_int` whose length is in `len`.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&mut so_error as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(so_error)
    }
}

pub(super) fn setsockopt_int(
    fd: RawFd,
    level: libc::c_int,
    name: libc::c_int,
    value: u32,
) -> io::Result<()> {
    let value = value as libc::c_int;
    // SAFETY: `fd` is an open socket. The buffer is a `c_int` of that size.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            (&value as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
