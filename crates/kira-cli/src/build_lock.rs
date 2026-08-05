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
//! # Waiting rather than failing
//!
//! A second builder waits for the first. Refusing outright would turn a
//! perfectly ordinary "two things built the same package" into an error a user
//! has to understand and work around, when the correct behaviour — do them one
//! after the other — is available and is what they meant.
//!
//! # Why a lock file and not a file range lock
//!
//! Because the thing being protected is a *directory* of artifacts, not one
//! file's bytes, and because a lock file needs no platform-specific call: an
//! exclusive create is atomic on every filesystem Kira builds on. The cost is
//! that a builder killed mid-build leaves the file behind, which is what the
//! staleness rule below is for.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long to wait for another builder before treating its lock as abandoned.
///
/// Long enough that a real build is never mistaken for a dead one — the harness
/// package alone takes over a minute on a cold cache — and short enough that a
/// developer whose build was killed is not left waiting on a file nothing will
/// ever remove.
const STALE_AFTER: Duration = Duration::from_secs(300);

/// How long to sleep between attempts.
///
/// Coarse on purpose: the thing being waited for takes seconds at least, so
/// polling faster would burn a core to learn nothing sooner.
const RETRY_EVERY: Duration = Duration::from_millis(50);

/// The lock file's name inside a build directory.
const LOCK_FILE: &str = ".build-lock";

/// An exclusive hold on one package's build directory.
///
/// Released when dropped, including when the build it guards fails or panics —
/// which is the reason it is a value rather than a pair of calls. A build that
/// returned early past an unlock would leave every later build waiting for a
/// process that had already exited.
#[derive(Debug)]
pub struct BuildLock {
    path: PathBuf,
}

impl BuildLock {
    /// Takes the lock for `directory`, waiting for any other builder.
    ///
    /// The directory is created if it does not exist, so a caller does not have
    /// to sequence "make the directory" against "lock it".
    ///
    /// Never fails for contention: a lock older than [`STALE_AFTER`] belonged to
    /// a builder that is gone, and is taken over. It fails only when the
    /// directory itself cannot be written, which a build was going to fail on
    /// anyway — and it says so there rather than here.
    pub fn acquire(directory: &Path) -> Result<BuildLock, std::io::Error> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join(LOCK_FILE);
        let waiting_since = Instant::now();
        loop {
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(_) => return Ok(BuildLock { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(&path) {
                        // Best effort: if another waiter removed it first, the
                        // next create wins the race and this one keeps waiting.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if waiting_since.elapsed() > STALE_AFTER {
                        // The holder is alive but has been building longer than
                        // any build should take. Waiting further would hang a
                        // command with no way out, so the lock is taken.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(RETRY_EVERY);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        // Best effort: a lock this process cannot remove is one the staleness
        // rule collects, and a failure here has no caller left to report to.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether the lock at `path` was left behind by a builder that is gone.
///
/// Judged by age rather than by asking whether the writing process still runs:
/// a pid would have to be written, read, and trusted, and a recycled pid is a
/// worse answer than a clock. A lock nobody holds is at worst waited on for
/// [`STALE_AFTER`] once.
fn is_stale(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        // It went away between the failed create and this check, which means
        // the holder released it — not stale, just gone.
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age > STALE_AFTER,
        // Modified in the future: a clock skew or a copied tree. Treat it as
        // fresh, because the one thing worse than waiting is two linkers.
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            !dir.join(LOCK_FILE).exists(),
            "a dropped lock must leave nothing behind, or the next build waits"
        );
        // And the directory locks again immediately.
        let _second = BuildLock::acquire(&dir).expect("the directory locks again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The directory is made if it is missing, so a caller need not sequence
    /// creating it against locking it.
    #[test]
    fn acquiring_creates_the_directory() {
        let dir = scratch("create");
        assert!(!dir.exists());
        let _lock = BuildLock::acquire(&dir).expect("a missing directory is created");
        assert!(dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A lock left by a builder that died is taken over rather than waited on
    /// forever — otherwise one killed build wedges a checkout permanently.
    #[test]
    fn an_abandoned_lock_is_taken_over() {
        let dir = scratch("stale");
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(LOCK_FILE);
        std::fs::write(&path, b"").expect("a lock file");
        let old = SystemTime::now() - STALE_AFTER - Duration::from_secs(60);
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("the lock file opens");
        file.set_modified(old).expect("an old timestamp");
        drop(file);

        assert!(is_stale(&path), "a lock this old is abandoned");
        let _lock = BuildLock::acquire(&dir).expect("an abandoned lock is taken over");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh lock is not mistaken for an abandoned one, which would put two
    /// linkers in one directory — the exact thing this prevents.
    #[test]
    fn a_fresh_lock_is_not_stale() {
        let dir = scratch("fresh");
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(LOCK_FILE);
        std::fs::write(&path, b"").expect("a lock file");
        assert!(!is_stale(&path));
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
