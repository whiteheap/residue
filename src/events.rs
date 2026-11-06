//! Wait for a set of files and directories to change: inotify on Linux,
//! kqueue on macOS and the BSDs. Elsewhere `wait` only sleeps, and `watch`
//! works from its timers alone.
//!
//! Callers learn only that something changed, not what: `watch` scans again
//! to find out, so a missed or coalesced event costs at most a delay.

use std::io;
use std::path::Path;
use std::time::Duration;

pub use imp::BACKEND;

pub struct Events {
    imp: imp::Events,
}

impl Events {
    pub fn new() -> io::Result<Events> {
        Ok(Events {
            imp: imp::Events::new()?,
        })
    }

    /// Report changes to `path`: entries added, removed or renamed in a
    /// directory, or a file rewritten, replaced or deleted.
    pub fn watch(&mut self, path: &Path) -> io::Result<()> {
        self.imp.watch(path)
    }

    /// Stop watching everything.
    pub fn clear(&mut self) -> io::Result<()> {
        self.imp = imp::Events::new()?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.imp.len()
    }

    /// Block until something watched changes (`true`) or `timeout` passes
    /// (`false`). A signal interrupting the wait also returns `false`.
    pub fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
        self.imp.wait(timeout)
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

#[cfg(target_os = "linux")]
mod imp {
    use super::c_path;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    pub const BACKEND: &str = "inotify";

    fn millis(timeout: Duration) -> libc::c_int {
        timeout.as_millis().min(libc::c_int::MAX as u128) as libc::c_int
    }

    pub struct Events {
        fd: libc::c_int,
        count: usize,
    }

    impl Events {
        pub fn new() -> io::Result<Events> {
            // SAFETY: plain syscall; the descriptor is owned by Events.
            let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Events { fd, count: 0 })
        }

        pub fn watch(&mut self, path: &Path) -> io::Result<()> {
            let c = c_path(path)?;
            let mask = libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_CLOSE_WRITE
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF;
            // SAFETY: `c` is a valid NUL-terminated path for the call.
            let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), mask) };
            if wd < 0 {
                return Err(io::Error::last_os_error());
            }
            self.count += 1;
            Ok(())
        }

        pub fn len(&self) -> usize {
            self.count
        }

        pub fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
            let mut pfd = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd that outlives the call.
            let n = unsafe { libc::poll(&mut pfd, 1, millis(timeout)) };
            if n < 0 {
                let e = io::Error::last_os_error();
                return if e.kind() == io::ErrorKind::Interrupted {
                    Ok(false)
                } else {
                    Err(e)
                };
            }
            if n == 0 {
                return Ok(false);
            }
            // Drain the queue; the events themselves are not needed.
            let mut buf = [0u8; 16 * 1024];
            loop {
                // SAFETY: reads into a buffer we own, within its length.
                let r = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
                if r <= 0 {
                    break;
                }
            }
            Ok(true)
        }
    }

    impl Drop for Events {
        fn drop(&mut self) {
            // SAFETY: closes the descriptor this struct owns, once.
            unsafe { libc::close(self.fd) };
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
mod imp {
    use super::c_path;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    pub const BACKEND: &str = "kqueue";

    #[cfg(target_os = "macos")]
    const OPEN_FLAGS: libc::c_int = libc::O_EVTONLY | libc::O_CLOEXEC;
    #[cfg(not(target_os = "macos"))]
    const OPEN_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_CLOEXEC;

    pub struct Events {
        kq: libc::c_int,
        fds: Vec<libc::c_int>,
    }

    impl Events {
        pub fn new() -> io::Result<Events> {
            // SAFETY: plain syscall; the descriptor is owned by Events.
            let kq = unsafe { libc::kqueue() };
            if kq < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Events {
                kq,
                fds: Vec::new(),
            })
        }

        pub fn watch(&mut self, path: &Path) -> io::Result<()> {
            let c = c_path(path)?;
            // SAFETY: `c` is a valid NUL-terminated path for the call.
            let fd = unsafe { libc::open(c.as_ptr(), OPEN_FLAGS) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: an all-zero kevent is valid; the fields set below
            // describe one vnode filter on `fd`.
            let mut change: libc::kevent = unsafe { std::mem::zeroed() };
            change.ident = fd as _;
            change.filter = libc::EVFILT_VNODE as _;
            change.flags = (libc::EV_ADD | libc::EV_CLEAR) as _;
            change.fflags = (libc::NOTE_WRITE
                | libc::NOTE_EXTEND
                | libc::NOTE_DELETE
                | libc::NOTE_RENAME
                | libc::NOTE_LINK) as _;
            // SAFETY: registers one change; no events are read back.
            let r = unsafe {
                libc::kevent(
                    self.kq,
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if r < 0 {
                let e = io::Error::last_os_error();
                // SAFETY: closes the descriptor opened above.
                unsafe { libc::close(fd) };
                return Err(e);
            }
            self.fds.push(fd);
            Ok(())
        }

        pub fn len(&self) -> usize {
            self.fds.len()
        }

        pub fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
            let ts = libc::timespec {
                tv_sec: timeout.as_secs().min(i32::MAX as u64) as _,
                tv_nsec: timeout.subsec_nanos() as _,
            };
            // SAFETY: an all-zero kevent array is valid output space.
            let mut out: [libc::kevent; 32] = unsafe { std::mem::zeroed() };
            // SAFETY: reads up to out.len() events into `out`.
            let n = unsafe {
                libc::kevent(
                    self.kq,
                    std::ptr::null(),
                    0,
                    out.as_mut_ptr(),
                    out.len() as _,
                    &ts,
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                return if e.kind() == io::ErrorKind::Interrupted {
                    Ok(false)
                } else {
                    Err(e)
                };
            }
            Ok(n > 0)
        }
    }

    impl Drop for Events {
        fn drop(&mut self) {
            // SAFETY: closes the descriptors this struct owns, once each.
            unsafe {
                for fd in &self.fds {
                    libc::close(*fd);
                }
                libc::close(self.kq);
            }
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
mod imp {
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    pub const BACKEND: &str = "none";

    pub struct Events;

    impl Events {
        pub fn new() -> io::Result<Events> {
            Ok(Events)
        }

        pub fn watch(&mut self, _path: &Path) -> io::Result<()> {
            Ok(())
        }

        pub fn len(&self) -> usize {
            0
        }

        pub fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
            std::thread::sleep(timeout);
            Ok(false)
        }
    }
}
