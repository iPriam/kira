use super::*;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{Config, Event, RecommendedWatcher, Watcher};

/// A group of raw notifications that will be compared as one source change.
#[derive(Default)]
struct PendingEvents {
    paths: BTreeSet<PathBuf>,
    rescan: bool,
}

/// The number of watcher recreations attempted after its event channel closes.
const MAX_RECONNECT_ATTEMPTS: usize = 3;

/// The pause between watcher recreations when the platform closes the channel.
const RECONNECT_BACKOFF: Duration = Duration::from_millis(25);

/// The maximum number of queued notifications retained before the producer waits
/// for the watcher to make progress.
const EVENT_CHANNEL_CAPACITY: usize = 4096;

/// The maximum number of notifications consumed for one non-blocking batch.
const MAX_EVENTS_PER_BATCH: usize = 4096;

impl SourceWatcher {
    /// Starts watching `set`, taking the current state as the baseline.
    pub fn new(set: WatchSet) -> Result<SourceWatcher, WatchError> {
        let (watcher, events) = create_watcher()?;

        let mut source_watcher = SourceWatcher {
            set,
            seen: BTreeMap::new(),
            _watcher: watcher,
            events,
            registered: Vec::new(),
            watching_for_arrival: false,
        };
        let set = source_watcher.set.clone();
        source_watcher.register_roots(&set)?;
        source_watcher.discard_startup_events()?;
        source_watcher.seen = snapshot(&source_watcher.set);
        Ok(source_watcher)
    }

    /// The set being watched.
    pub fn set(&self) -> &WatchSet {
        &self.set
    }

    /// Adds any new roots in `set` while retaining the current source baseline.
    ///
    /// A rebuild can discover a dependency package that was not in the first
    /// source graph. Retaining the baseline lets queued events from the build
    /// become the next rebuild rather than silently turning them into startup
    /// state.
    pub fn update_set(&mut self, set: WatchSet) -> Result<(), WatchError> {
        self.register_roots(&set)?;
        self.set = set;
        let current = snapshot(&self.set);
        let previous = std::mem::take(&mut self.seen);
        let previous_by_key: BTreeMap<String, Stamp> = previous
            .into_iter()
            .map(|(path, stamp)| (path_key(&path), stamp))
            .collect();
        self.seen = current
            .into_iter()
            .map(|(path, stamp)| {
                let baseline = previous_by_key
                    .get(&path_key(&path))
                    .copied()
                    .unwrap_or(stamp);
                (path, baseline)
            })
            .collect();
        Ok(())
    }

    /// Recreates the platform watcher after a transient backend failure.
    ///
    /// The existing source baseline is retained, so edits made while the
    /// backend was unavailable are returned as changes instead of becoming a
    /// new startup baseline.
    pub fn recover(&mut self) -> Result<(), WatchError> {
        let (watcher, events) = create_watcher()?;
        self._watcher = watcher;
        self.events = events;
        self.registered.clear();
        let set = self.set.clone();
        self.register_roots(&set)?;
        self.discard_startup_events()?;
        Ok(())
    }

    /// Returns changes already delivered by the platform without waiting.
    pub fn poll(&mut self) -> Result<Vec<Change>, WatchError> {
        let mut pending = PendingEvents::default();
        for _ in 0..MAX_EVENTS_PER_BATCH {
            match self.events.try_recv() {
                Ok(event) => self.record_result(event, &mut pending)?,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.recover_with_retry()?;
                    return self.finish_pending(PendingEvents {
                        rescan: true,
                        ..PendingEvents::default()
                    });
                }
            }
        }
        self.finish_pending(pending)
    }

    /// Waits up to `timeout` for a debounced batch of changes.
    ///
    /// Notifications for ignored paths are consumed without becoming changes.
    /// A timeout is a normal empty result, not a watcher failure, so the caller
    /// can also service its runner and session deadline.
    pub fn wait_for(&mut self, timeout: Duration) -> Result<Vec<Change>, WatchError> {
        let deadline = Instant::now() + timeout;
        let mut reconnects = 0;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let first = match self.events.recv_timeout(remaining) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    // A root that does not exist yet cannot be watched; the
                    // fallback watches the nearest existing ancestor and waits
                    // for the tree to appear. Installing a recursive inotify
                    // watch is not atomic with the directories being created,
                    // so `mkdir -p a/b && write a/b/f` can finish before the
                    // watch on `a` exists and the creation is never reported.
                    //
                    // Events are a hint; the snapshot is the truth. Reconciling
                    // against it on the way out costs a walk only on the timeout
                    // path, and only while a root is still missing — once the
                    // tree exists the watches are real and this never runs.
                    return self.reconcile_on_arrival();
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if reconnects >= MAX_RECONNECT_ATTEMPTS {
                        return Err(WatchError::Disconnected);
                    }
                    reconnects += 1;
                    self.recover_with_retry()?;
                    let changes = self.finish_pending(PendingEvents {
                        rescan: true,
                        ..PendingEvents::default()
                    })?;
                    if !changes.is_empty() {
                        return Ok(changes);
                    }
                    let backoff =
                        RECONNECT_BACKOFF.min(deadline.saturating_duration_since(Instant::now()));
                    std::thread::sleep(backoff);
                    continue;
                }
            };

            let mut pending = PendingEvents::default();
            self.record_result(first, &mut pending)?;
            self.collect_debounced(&deadline, &mut pending)?;
            let changes = self.finish_pending(pending)?;
            if !changes.is_empty() {
                return Ok(changes);
            }
            if Instant::now() >= deadline {
                return self.reconcile_on_arrival();
            }
        }
    }

    /// The changes a walk finds when the events could not be trusted.
    ///
    /// Both ways out of `wait_for` with nothing to report come here. Events for
    /// paths outside the source set are filtered, so a batch can arrive, leave
    /// `pending` empty, and short-circuit before any snapshot is taken — which
    /// is the same blind spot as receiving no event at all.
    fn reconcile_on_arrival(&mut self) -> Result<Vec<Change>, WatchError> {
        if !self.watching_for_arrival {
            return Ok(Vec::new());
        }
        let changes = self.finish_pending(PendingEvents {
            rescan: true,
            ..PendingEvents::default()
        })?;
        // Every root exists now, so the platform watches are real ones and the
        // events can be trusted again.
        if self.set.roots().iter().all(|root| root.exists()) {
            self.watching_for_arrival = false;
        }
        Ok(changes)
    }

    /// Keeps events arriving during one save in the same batch.
    fn collect_debounced(
        &mut self,
        deadline: &Instant,
        pending: &mut PendingEvents,
    ) -> Result<(), WatchError> {
        let batch_deadline = Instant::now() + MAX_DEBOUNCE;
        let mut events = 0;
        loop {
            let remaining = batch_deadline
                .min(*deadline)
                .saturating_duration_since(Instant::now());
            if remaining.is_zero() || events >= MAX_EVENTS_PER_BATCH {
                return Ok(());
            }
            match self.events.recv_timeout(DEBOUNCE_WINDOW.min(remaining)) {
                Ok(event) => {
                    events += 1;
                    self.record_result(event, pending)?;
                }
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => {
                    self.recover_with_retry()?;
                    pending.rescan = true;
                    return Ok(());
                }
            }
        }
    }

    /// Records one platform result if it may affect this watch set.
    fn record_result(
        &mut self,
        result: notify::Result<Event>,
        pending: &mut PendingEvents,
    ) -> Result<(), WatchError> {
        match result {
            Ok(event) => self.record_event(event, pending),
            Err(error) if self.event_error_is_transient(&error) => {
                let set = self.set.clone();
                self.register_roots(&set)?;
                pending.rescan = true;
                Ok(())
            }
            Err(error) => {
                let paths = error
                    .paths
                    .iter()
                    .map(|path| normalize_path(path))
                    .collect();
                Err(WatchError::Event {
                    paths,
                    source: error,
                })
            }
        }
    }

    /// Records one platform event if it may affect this watch set.
    fn record_event(
        &mut self,
        event: Event,
        pending: &mut PendingEvents,
    ) -> Result<(), WatchError> {
        if event.need_rescan() {
            pending.rescan = true;
        }
        if !event_kind_can_change_files(&event.kind) {
            return Ok(());
        }
        let set = self.set.clone();
        self.register_roots(&set)?;
        if event.paths.is_empty() {
            pending.rescan = true;
            return Ok(());
        }
        for path in event.paths {
            let path = normalize_path(&path);
            if self.event_path_is_relevant(&event.kind, &path) {
                pending.paths.insert(path);
            }
        }
        Ok(())
    }

    /// Registers roots not already covered by this platform watcher.
    fn register_roots(&mut self, set: &WatchSet) -> Result<(), WatchError> {
        // A root that does not exist yet is watched through the nearest
        // existing ancestor, and installing a recursive watch is not atomic
        // with the directories being created — `mkdir -p a/b && write a/b/f`
        // can finish before the watch on `a` exists, and nothing reports it.
        // While that is true, events are a hint and the snapshot is the truth.
        // Sticky. Re-registration happens again after the tree appears — a
        // transient notify error re-registers, and by then every root exists —
        // so assigning here cleared the flag before the arrival was ever
        // reported, and the reconcile that existed to catch it never ran.
        // Cleared in `reconcile_on_arrival`, once the arrival has been reported.
        if set.roots().iter().any(|root| !root.exists()) {
            self.watching_for_arrival = true;
        }
        let desired = set.event_roots();
        let mut index = 0;
        while index < self.registered.len() {
            let keep = desired
                .iter()
                .any(|(path, _)| same_path(path, &self.registered[index].0));
            if keep {
                index += 1;
            } else {
                self.remove_registered(index)?;
            }
        }

        for (path, mode) in desired {
            let Some(index) = self
                .registered
                .iter()
                .position(|(existing, _)| same_path(existing, &path))
            else {
                self.add_registered(path, mode)?;
                continue;
            };

            if mode == RecursiveMode::Recursive && self.registered[index].1 != mode {
                self.remove_registered(index)?;
                self.add_registered(path, mode)?;
            }
        }
        Ok(())
    }

    /// Registers one path, ignoring a path that disappeared during recovery.
    fn add_registered(&mut self, path: PathBuf, mode: RecursiveMode) -> Result<(), WatchError> {
        match self._watcher.watch(&path, mode) {
            Ok(()) => self.registered.push((path, mode)),
            Err(source) if is_transient_notify_error(&source) => {}
            Err(source) => {
                return Err(WatchError::Register { path, source });
            }
        }
        Ok(())
    }

    /// Removes one path, ignoring a platform watch already removed by the OS.
    fn remove_registered(&mut self, index: usize) -> Result<(), WatchError> {
        let (path, _) = self.registered[index].clone();
        match self._watcher.unwatch(&path) {
            Ok(()) => {
                self.registered.remove(index);
            }
            Err(source) if is_transient_notify_error(&source) => {
                self.registered.remove(index);
            }
            Err(source) => {
                return Err(WatchError::Unregister { path, source });
            }
        }
        Ok(())
    }

    /// Whether a platform event error concerns a root that may be recreated.
    fn event_error_is_transient(&self, error: &notify::Error) -> bool {
        if !is_transient_notify_error(error) {
            return false;
        }
        error.paths.is_empty()
            || error
                .paths
                .iter()
                .all(|path| self.path_is_in_watch_scope(path))
    }

    /// Recreates the platform watcher after a short-lived backend disconnect.
    fn recover_with_retry(&mut self) -> Result<(), WatchError> {
        for attempt in 0..MAX_RECONNECT_ATTEMPTS {
            match self.recover() {
                Ok(()) => return Ok(()),
                Err(error) if error.is_transient() && attempt + 1 < MAX_RECONNECT_ATTEMPTS => {
                    std::thread::sleep(RECONNECT_BACKOFF);
                }
                Err(error) => return Err(error),
            }
        }
        Err(WatchError::Disconnected)
    }

    /// Whether a path belongs to a watched tree, including ignored descendants.
    fn path_is_in_watch_scope(&self, path: &Path) -> bool {
        let path = normalize_path(path);
        self.set.roots.iter().any(|root| {
            same_path(root, &path)
                || relative_path(&path, root).is_some()
                || root.parent().is_some_and(|parent| same_path(parent, &path))
        })
    }

    /// Converts one event batch into stable source changes.
    fn finish_pending(&mut self, pending: PendingEvents) -> Result<Vec<Change>, WatchError> {
        if pending.paths.is_empty() && !pending.rescan {
            return Ok(Vec::new());
        }

        let now = snapshot(&self.set);
        let mut previous: BTreeMap<String, (PathBuf, Stamp)> = self
            .seen
            .iter()
            .map(|(path, stamp)| (path_key(path), (path.clone(), *stamp)))
            .collect();
        let mut changes = Vec::new();
        for (path, stamp) in &now {
            match previous.remove(&path_key(path)) {
                None => changes.push(Change {
                    path: path.clone(),
                    kind: ChangeKind::Added,
                }),
                Some((_, before))
                    if before != *stamp
                        || pending.paths.iter().any(|event| same_path(event, path)) =>
                {
                    changes.push(Change {
                        path: path.clone(),
                        kind: ChangeKind::Modified,
                    });
                }
                Some(_) => {}
            }
        }
        for (_, (path, _)) in previous {
            changes.push(Change {
                path,
                kind: ChangeKind::Removed,
            });
        }

        self.seen = now;
        changes.sort_by_key(|left| path_key(&left.path));
        Ok(changes)
    }

    /// Whether a platform event path belongs to a non-ignored logical root.
    fn event_path_is_relevant(&self, kind: &EventKind, path: &Path) -> bool {
        let path = normalize_path(path);
        let directory_event = path.is_dir()
            || matches!(kind, EventKind::Create(CreateKind::Folder))
            || matches!(kind, EventKind::Remove(RemoveKind::Folder));
        self.set.roots.iter().any(|root| {
            if same_path(&path, root) {
                return true;
            }
            if root.parent().is_some_and(|parent| same_path(&path, parent)) {
                return true;
            }
            let Some(relative) = relative_path(&path, root) else {
                return false;
            };
            let parts: Vec<&str> = relative
                .split('/')
                .filter(|part| !part.is_empty())
                .collect();
            if parts.iter().any(|part| is_ignored_directory_name(part)) {
                return false;
            }
            parts
                .last()
                .is_none_or(|part| directory_event || is_watchable_file(Path::new(part)))
        })
    }

    /// Drains registration notifications before establishing a new baseline.
    fn discard_startup_events(&mut self) -> Result<(), WatchError> {
        for _ in 0..MAX_EVENTS_PER_BATCH {
            match self.events.try_recv() {
                Ok(Ok(_)) => {}
                Ok(Err(error)) if self.event_error_is_transient(&error) => {
                    let set = self.set.clone();
                    self.register_roots(&set)?;
                }
                Ok(Err(error)) => {
                    let paths = error
                        .paths
                        .iter()
                        .map(|path| normalize_path(path))
                        .collect();
                    return Err(WatchError::Event {
                        paths,
                        source: error,
                    });
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(WatchError::Disconnected);
                }
            }
        }
        Ok(())
    }
}

/// Creates a notify watcher and its owned event channel.
fn create_watcher() -> Result<(RecommendedWatcher, Receiver<notify::Result<Event>>), WatchError> {
    let (sender, events) = mpsc::sync_channel(EVENT_CHANNEL_CAPACITY);
    let watcher = RecommendedWatcher::new(
        move |event| {
            let _ = sender.send(event);
        },
        Config::default().with_follow_symlinks(false),
    )
    .map_err(WatchError::Initialize)?;
    Ok((watcher, events))
}
