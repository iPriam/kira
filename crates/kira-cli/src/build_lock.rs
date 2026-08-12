//! One builder at a time in a package's `.kira-build` directory.
//!
//! Artifacts are named after the package, not after the process building it:
//! two `kira` invocations against one package write the same `main.o`, the same
//! `main.exe`, the same `.kbc`. Nothing about that is safe to do twice at once.
//!
//! On Unix the damage is usually invisible — a running executable keeps the
//! inode it was started from, so a relink that replaces the file underneath it
//! looks like it worked. On Windows the file is held open and the link fails
//! outright:
//!
//! ```text
//! LINK : fatal error LNK1104: cannot open file '...\.kira-build\main.exe'
//! ```
//!
//! That is the failure this exists to stop, and it is worth stopping in the
//! toolchain rather than in whichever caller happened to hit it: a test harness
//! that runs two cases in parallel, a watch mode that rebuilds while a run is
//! still going, and two terminals in one checkout are all the same collision.
//! Serializing them here fixes all three at once, and a caller never has to
//! know the rule exists.
//!
//! # Waiting rather than failing, and saying so
//!
//! A second builder waits for the first. Refusing outright would turn a
//! perfectly ordinary "two things built the same package" into an error a user
//! has to understand and work around, when the correct behaviour — do them one
//! after the other — is available and is what they meant.
//!
//! A wait is **always reported** before it begins. A compiler that goes quiet
//! for as long as another build takes, with no line saying what it is waiting
//! for, is indistinguishable from one that has hung; and the wait is also given
//! a phase of its own, so `--timings` credits it to waiting rather than to
//! whichever phase happened to be open.
//!
//! # The lock is the operating system's, not a timestamp
//!
//! The hold is an exclusive lock on the file — `flock` on Unix,
//! `LockFileEx` on Windows — taken for as long as this process lives. That is
//! what makes the two failure modes of a lock file impossible rather than
//! merely unlikely:
//!
//! A builder that is killed releases its lock, because the kernel closes its
//! handles. There is nothing to time out, so a killed build wedges nothing and
//! the next one starts immediately.
//!
//! And a lock is never taken from a builder that still holds it. Deciding by
//! clock — "this has been held too long, it must be dead" — is a guess, and
//! when the guess is wrong the result is two linkers in one directory writing
//! each other's objects: empty `.o` files, a link that fails on symbols that
//! are really there, and no diagnostic naming any of it. A slow build is slow;
//! it is not abandoned.
//!
//! The file itself is never removed. It is only the thing the lock is attached
//! to, and removing it while another process waits on that same lock would take
//! the anchor out from under the waiter.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// The lock file's name inside a build directory.
const LOCK_FILE: &str = ".build-lock";

/// An exclusive hold on one package's build directory.
///
/// Released when dropped, including when the build it guards fails or panics —
/// and released by the kernel if the process dies without dropping anything,
/// which is what a lock file compared against a clock could not promise.
#[derive(Debug)]
pub struct BuildLock {
    /// The locked file. Held open because closing it releases the lock.
    file: File,
}

impl BuildLock {
    /// Takes the lock for `directory`, waiting for any other builder.
    ///
    /// The directory is created if it does not exist, so a caller does not have
    /// to sequence "make the directory" against "lock it".
    ///
    /// Never fails for contention: a wait is reported and then waited out. It
    /// fails only when the directory or the lock file cannot be opened, which a
    /// build was going to fail on anyway.
    pub fn acquire(directory: &Path) -> Result<BuildLock, std::io::Error> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;

        if !lock::try_exclusive(&file)? {
            // Before blocking, not after: the point of the line is that the
            // silence has a reason, and a line printed once the wait is over
            // explains a pause the user has already spent.
            kira_diagnostics::progress!("waiting for another build of this package");
            eprintln!(
                "kira: another build of this package is running; waiting for it to finish\n\
                 note: it holds `{}`",
                path.display(),
            );
            lock::exclusive(&file)?;
        }

        let mut lock = BuildLock { file };
        lock.record_holder();
        Ok(lock)
    }

    /// Writes who holds the lock, for a human who opens the file.
    ///
    /// Best effort and never fatal: the hold is the operating system's lock, and
    /// the contents are a courtesy to whoever is looking at a build directory
    /// wondering which process to go and find.
    fn record_holder(&mut self) {
        let _ = self.file.set_len(0);
        let _ = writeln!(self.file, "kira build, pid {}", std::process::id());
        let _ = self.file.flush();
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        // Best effort, and the close that follows releases the lock regardless.
        // The file stays: it is the lock's anchor, and a waiter is holding on to
        // it right now.
        lock::release(&self.file);
    }
}

/// The one platform-specific part: an exclusive lock on an open file.
///
/// Two calls each way — try, and wait — because the wait is what has to be
/// announced, so a caller has to be able to learn that it is about to happen.
#[cfg(unix)]
mod lock {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    /// Takes the lock if it is free, answering whether it was.
    pub(super) fn try_exclusive(file: &File) -> Result<bool, std::io::Error> {
        // SAFETY: `file` is open for the duration of the call, so its
        // descriptor is valid; `flock` touches nothing else.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if taken == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            // Held by someone else, which is the answer rather than a failure.
            Some(libc::EWOULDBLOCK) => Ok(false),
            _ => Err(error),
        }
    }

    /// Waits for the lock however long the holder takes.
    pub(super) fn exclusive(file: &File) -> Result<(), std::io::Error> {
        // SAFETY: as above.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error())
    }

    /// Releases the lock, for the drop that does not want to wait for the close.
    pub(super) fn release(file: &File) {
        // SAFETY: as above; a failed unlock is undone by the close that follows.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// The Windows half, on `LockFileEx`.
///
/// The lock covers one byte at offset zero rather than the whole file, because
/// the range only has to be the same one on both sides for the two builders to
/// exclude each other, and a range past the end of a file locks fine — the file
/// starts empty and the holder writes its pid into it afterwards.
#[cfg(windows)]
mod lock {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    /// The one byte both sides lock.
    const RANGE: u32 = 1;

    /// Takes the lock if it is free, answering whether it was.
    pub(super) fn try_exclusive(file: &File) -> Result<bool, std::io::Error> {
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: `file` is open for the duration of the call, so its handle is
        // valid; `overlapped` is a live zeroed structure of the size the call
        // expects and is not used after it returns.
        let taken = unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                RANGE,
                0,
                &mut overlapped,
            )
        };
        if taken != 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            return Ok(false);
        }
        Err(error)
    }

    /// Waits for the lock however long the holder takes.
    pub(super) fn exclusive(file: &File) -> Result<(), std::io::Error> {
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: as above.
        let taken = unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK,
                0,
                RANGE,
                0,
                &mut overlapped,
            )
        };
        if taken != 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error())
    }

    /// Releases the lock, for the drop that does not want to wait for the close.
    pub(super) fn release(file: &File) {
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: as above; a failed unlock is undone by the close that follows.
        unsafe { UnlockFileEx(file.as_raw_handle() as HANDLE, 0, RANGE, 0, &mut overlapped) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kira-build-lock-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_lock_is_released_when_it_is_dropped() {
        let dir = scratch("release");
        let lock = BuildLock::acquire(&dir).expect("a fresh directory locks");
        assert!(dir.join(LOCK_FILE).exists());
        drop(lock);
        // The file stays — it is the lock's anchor — and the directory locks
        // again immediately, which is the property that matters.
        assert!(dir.join(LOCK_FILE).exists());
        let _second = BuildLock::acquire(&dir).expect("the directory locks again");
        drop(_second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The directory is made if it is missing, so a caller need not sequence
    /// creating it against locking it.
    #[test]
    fn acquiring_creates_the_directory() {
        let dir = scratch("create");
        assert!(!dir.exists());
        let lock = BuildLock::acquire(&dir).expect("a missing directory is created");
        assert!(dir.exists());
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A lock file left behind by a builder that is gone holds nothing: the
    /// kernel released the lock when that process died, so the next build takes
    /// it without waiting and without any staleness rule to get wrong.
    #[test]
    fn a_lock_file_left_behind_holds_nothing() {
        let dir = scratch("abandoned");
        std::fs::create_dir_all(&dir).expect("scratch");
        std::fs::write(dir.join(LOCK_FILE), b"kira build, pid 999999\n").expect("a lock file");

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(LOCK_FILE))
            .expect("the lock file opens");
        assert!(
            lock::try_exclusive(&file).expect("locking answers"),
            "a file nobody holds locks immediately"
        );
        lock::release(&file);
        drop(file);

        let lock = BuildLock::acquire(&dir).expect("an abandoned lock file is taken over");
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A held lock is reported as held rather than taken. This is the property
    /// the old timestamp rule could not offer: no elapsed time makes a live
    /// holder's lock available, so two builders can never be in one directory.
    #[test]
    fn a_held_lock_is_never_available() {
        let dir = scratch("held");
        let held = BuildLock::acquire(&dir).expect("the directory locks");

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(LOCK_FILE))
            .expect("the lock file opens");
        assert!(
            !lock::try_exclusive(&file).expect("locking answers"),
            "a lock this process holds must not be available to another opener"
        );
        drop(file);
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two builders in one directory run one after the other, never together.
    #[test]
    fn a_second_builder_waits_for_the_first() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = Arc::new(scratch("contended"));
        let inside = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut threads = Vec::new();
        for _ in 0..4 {
            let dir = Arc::clone(&dir);
            let inside = Arc::clone(&inside);
            let peak = Arc::clone(&peak);
            threads.push(std::thread::spawn(move || {
                let _lock = BuildLock::acquire(&dir).expect("the directory locks");
                let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                inside.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for thread in threads {
            thread.join().expect("a builder thread");
        }

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "two builders were in one build directory at once"
        );
        let _ = std::fs::remove_dir_all(dir.as_path());
    }
}
