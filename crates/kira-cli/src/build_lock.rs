//! Serializes writers to a package's `.kira-build` directory.
//!
//! The OS file lock handles other processes; the local registry makes nested
//! acquisition reentrant while still serializing separate threads.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::ThreadId;

/// The lock file's name inside a build directory.
const LOCK_FILE: &str = ".build-lock";

/// An exclusive hold on one package's build directory.
///
/// Released when dropped, including when the build it guards fails or panics —
/// and released by the kernel if the process dies without dropping anything,
/// which is what a lock file compared against a clock could not promise.
#[derive(Debug)]
pub struct BuildLock {
    key: PathBuf,
    owner: ThreadId,
    locked: Option<Arc<LockedFile>>,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[derive(Debug)]
struct LockedFile {
    file: File,
}

#[derive(Debug)]
struct LocalHold {
    owner: ThreadId,
    depth: usize,
    locked: Option<Arc<LockedFile>>,
}

#[derive(Default)]
struct LocalLocks {
    holds: std::collections::HashMap<PathBuf, LocalHold>,
}

fn local_locks() -> &'static (Mutex<LocalLocks>, Condvar) {
    static LOCKS: OnceLock<(Mutex<LocalLocks>, Condvar)> = OnceLock::new();
    LOCKS.get_or_init(|| (Mutex::new(LocalLocks::default()), Condvar::new()))
}

fn report_wait(path: &Path) {
    kira_diagnostics::progress!("waiting for another build of this package");
    eprintln!(
        "kira: another build of this package is running; waiting for it to finish\n\
         note: it holds `{}`",
        path.display(),
    );
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
        let key = std::fs::canonicalize(directory)?;
        let path = key.join(LOCK_FILE);
        let owner = std::thread::current().id();
        let (local, changed) = local_locks();
        let mut state = local.lock().unwrap_or_else(|error| error.into_inner());
        let mut reported_wait = false;
        loop {
            match state.holds.get_mut(&key) {
                Some(hold) if hold.owner == owner => {
                    if let Some(locked) = hold.locked.clone() {
                        hold.depth += 1;
                        return Ok(BuildLock {
                            key,
                            owner,
                            locked: Some(locked),
                            _not_send: std::marker::PhantomData,
                        });
                    }
                    state = changed
                        .wait(state)
                        .unwrap_or_else(|error| error.into_inner());
                }
                Some(_) => {
                    if !reported_wait {
                        report_wait(&path);
                        reported_wait = true;
                    }
                    state = changed
                        .wait(state)
                        .unwrap_or_else(|error| error.into_inner());
                }
                None => {
                    state.holds.insert(
                        key.clone(),
                        LocalHold {
                            owner,
                            depth: 1,
                            locked: None,
                        },
                    );
                    break;
                }
            }
        }
        drop(state);

        let result = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .and_then(|file| {
                if lock::try_exclusive(&file)? {
                    Ok(file)
                } else {
                    report_wait(&path);
                    lock::exclusive(&file)?;
                    Ok(file)
                }
            });

        let mut file = match result {
            Ok(file) => file,
            Err(error) => {
                let mut state = local.lock().unwrap_or_else(|error| error.into_inner());
                state.holds.remove(&key);
                changed.notify_all();
                return Err(error);
            }
        };
        let _ = file.set_len(0);
        let _ = writeln!(file, "kira build, pid {}", std::process::id());
        let _ = file.flush();
        let locked = Arc::new(LockedFile { file });

        let mut state = local.lock().unwrap_or_else(|error| error.into_inner());
        state
            .holds
            .get_mut(&key)
            .expect("local build lock reservation disappeared")
            .locked = Some(Arc::clone(&locked));
        changed.notify_all();
        Ok(BuildLock {
            key,
            owner,
            locked: Some(locked),
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let Some(locked) = self.locked.take() else {
            return;
        };
        let (local, changed) = local_locks();
        let mut state = local.lock().unwrap_or_else(|error| error.into_inner());
        let Some(hold) = state.holds.get_mut(&self.key) else {
            return;
        };
        debug_assert_eq!(hold.owner, self.owner);
        if hold.depth > 1 {
            hold.depth -= 1;
            drop(state);
            drop(locked);
            return;
        }

        let hold = state.holds.remove(&self.key);
        drop(locked);
        drop(hold);
        changed.notify_all();
    }
}

impl Drop for LockedFile {
    fn drop(&mut self) {
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

    #[test]
    fn one_build_can_enter_the_same_directory_twice() {
        let dir = scratch("reentrant");
        let outer = BuildLock::acquire(&dir).expect("outer build lock");
        let inner = BuildLock::acquire(&dir).expect("nested build lock");
        drop(outer);

        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("lock file");
        assert!(!lock::try_exclusive(&file).expect("inner lock remains held"));
        drop(inner);
        assert!(lock::try_exclusive(&file).expect("last nested guard releases"));
        lock::release(&file);
        drop(file);
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
