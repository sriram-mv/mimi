//! PTY layer, built directly on POSIX (`posix_openpt`/`fork`/`execvp`).
//!
//! mimi is fish-first: [`find_shell`] locates fish across the usual macOS
//! install locations (Homebrew Apple Silicon / Intel, MacPorts, system
//! paths) and falls back to the user's `$SHELL` with a warning flag so the
//! UI can tell the user fish wasn't found.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

/// Where we found the shell, so the app can surface a notice when fish is
/// missing rather than silently degrading.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellChoice {
    Fish(PathBuf),
    Fallback(PathBuf),
}

impl ShellChoice {
    pub fn path(&self) -> &Path {
        match self {
            ShellChoice::Fish(p) | ShellChoice::Fallback(p) => p,
        }
    }

    pub fn is_fish(&self) -> bool {
        matches!(self, ShellChoice::Fish(_))
    }
}

const FISH_CANDIDATES: &[&str] = &[
    "/opt/homebrew/bin/fish", // Homebrew, Apple Silicon
    "/usr/local/bin/fish",    // Homebrew, Intel
    "/opt/local/bin/fish",    // MacPorts
    "/usr/bin/fish",
    "/bin/fish",
];

/// Locate the shell to run. Order: `$MIMI_SHELL` override, fish in known
/// locations, fish on `$PATH`, then `$SHELL`, then `/bin/sh`.
pub fn find_shell() -> ShellChoice {
    if let Ok(over) = std::env::var("MIMI_SHELL") {
        if !over.is_empty() {
            let p = PathBuf::from(&over);
            return if p.file_name().is_some_and(|n| n == "fish") {
                ShellChoice::Fish(p)
            } else {
                ShellChoice::Fallback(p)
            };
        }
    }
    for cand in FISH_CANDIDATES {
        let p = Path::new(cand);
        if p.exists() {
            return ShellChoice::Fish(p.to_path_buf());
        }
    }
    if let Some(p) = which("fish") {
        return ShellChoice::Fish(p);
    }
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() {
            return ShellChoice::Fallback(PathBuf::from(shell));
        }
    }
    ShellChoice::Fallback(PathBuf::from("/bin/sh"))
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}

pub struct Pty {
    master: OwnedFd,
    pub child: libc::pid_t,
    pub shell: ShellChoice,
}

/// A cheap, cloneable writer handle onto the PTY master.
#[derive(Clone)]
pub struct PtyWriter {
    fd: RawFd,
}

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe { libc::write(self.fd, buf.as_ptr() as *const _, buf.len()) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn cerr(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

impl Pty {
    /// Open a PTY and spawn `shell` in it with the given size and extra
    /// environment. `extra_env` entries are `(key, value)` pairs set in the
    /// child only.
    pub fn spawn(
        shell: ShellChoice,
        cols: u16,
        rows: u16,
        extra_env: &[(String, String)],
        cwd: Option<&Path>,
    ) -> io::Result<Pty> {
        let master = unsafe { cerr(libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY))? };
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        unsafe {
            cerr(libc::grantpt(master.as_raw_fd()))?;
            cerr(libc::unlockpt(master.as_raw_fd()))?;
        }

        let mut name_buf = [0u8; 256];
        #[cfg(target_os = "macos")]
        let slave_path = unsafe {
            let ptr = libc::ptsname(master.as_raw_fd());
            if ptr.is_null() {
                return Err(io::Error::last_os_error());
            }
            CStr::from_ptr(ptr).to_owned()
        };
        #[cfg(not(target_os = "macos"))]
        let slave_path = unsafe {
            cerr(libc::ptsname_r(
                master.as_raw_fd(),
                name_buf.as_mut_ptr() as *mut _,
                name_buf.len(),
            ))?;
            CStr::from_bytes_until_nul(&name_buf)
                .map_err(|_| io::Error::other("bad pts name"))?
                .to_owned()
        };
        let _ = &mut name_buf; // silence unused on macos

        let shell_path = CString::new(shell.path().as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "shell path"))?;
        // argv[0] starting with '-' marks a login shell.
        let argv0 = {
            let name = shell
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "sh".into());
            CString::new(format!("-{name}")).unwrap()
        };
        let env_strings: Vec<CString> = extra_env
            .iter()
            .map(|(k, v)| CString::new(format!("{k}={v}")).unwrap())
            .collect();
        let cwd_c = cwd.and_then(|p| CString::new(p.as_os_str().as_encoded_bytes()).ok());

        let winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let pid = unsafe { cerr(libc::fork())? };
        if pid == 0 {
            // Child. Only async-signal-safe calls from here on.
            unsafe {
                libc::setsid();
                let slave = libc::open(slave_path.as_ptr(), libc::O_RDWR);
                if slave < 0 {
                    libc::_exit(126);
                }
                libc::ioctl(slave, libc::TIOCSCTTY, 0);
                libc::ioctl(slave, libc::TIOCSWINSZ, &winsize);
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::dup2(slave, 2);
                if slave > 2 {
                    libc::close(slave);
                }
                // The master fd closes in the child via CLOEXEC-less close:
                libc::close(master.as_raw_fd());
                if let Some(dir) = &cwd_c {
                    libc::chdir(dir.as_ptr());
                }
                for kv in &env_strings {
                    libc::putenv(kv.as_ptr() as *mut _);
                }
                let argv = [argv0.as_ptr(), std::ptr::null()];
                libc::execvp(shell_path.as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }
        }

        Ok(Pty {
            master,
            child: pid,
            shell,
        })
    }

    pub fn writer(&self) -> PtyWriter {
        PtyWriter {
            fd: self.master.as_raw_fd(),
        }
    }

    /// Blocking reader over a dup of the master fd — hand this to the reader
    /// thread.
    pub fn reader(&self) -> io::Result<File> {
        let fd = unsafe { cerr(libc::dup(self.master.as_raw_fd()))? };
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub fn resize(&self, cols: u16, rows: u16, px_width: u16, px_height: u16) -> io::Result<()> {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: px_width,
            ws_ypixel: px_height,
        };
        cerr(unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) })?;
        Ok(())
    }

    /// Block until the child exits; returns its exit status. Call from a
    /// dedicated thread.
    pub fn wait(&self) -> i32 {
        let mut status: libc::c_int = 0;
        unsafe { libc::waitpid(self.child, &mut status, 0) };
        if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            -1
        }
    }

    pub fn kill(&self) {
        unsafe {
            libc::kill(self.child, libc::SIGHUP);
        }
    }
}

/// Convenience: read chunks from `reader` until EOF, invoking `on_data`.
pub fn read_loop<R: Read>(mut reader: R, mut on_data: impl FnMut(&[u8])) {
    let mut buf = [0u8; 65536];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => on_data(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // On Linux, read() on the master returns EIO when the child
            // exits; treat as EOF.
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_shell_returns_something_executable() {
        let choice = find_shell();
        assert!(
            choice.path().exists(),
            "shell path should exist: {:?}",
            choice
        );
    }

    #[test]
    fn spawn_echo_roundtrip() {
        // Force a predictable shell for the test environment.
        let shell = ShellChoice::Fallback(PathBuf::from("/bin/sh"));
        let pty = Pty::spawn(
            shell,
            80,
            24,
            &[("MIMI_TEST_VAR".into(), "hello-pty".into())],
            None,
        )
        .expect("spawn sh");

        let mut w = pty.writer();
        w.write_all(b"echo $MIMI_TEST_VAR; exit\n").unwrap();

        let mut out = Vec::new();
        let reader = pty.reader().unwrap();
        read_loop(reader, |chunk| out.extend_from_slice(chunk));
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("hello-pty"), "pty output: {text:?}");
        assert_eq!(pty.wait(), 0);
    }

    #[test]
    fn resize_succeeds() {
        let shell = ShellChoice::Fallback(PathBuf::from("/bin/sh"));
        let pty = Pty::spawn(shell, 80, 24, &[], None).unwrap();
        pty.resize(120, 40, 960, 800).unwrap();
        let mut w = pty.writer();
        w.write_all(b"exit\n").unwrap();
        pty.wait();
    }
}
