//! Sampling a child process with the Windows debugging facility.
//!
//! Windows has no `perf`. What it has is the facility every Windows profiler
//! and debugger is built on: suspend a thread, take its register context, and
//! unwind it with DbgHelp against the target's own unwind data and symbols.
//! That is what this collector drives, and it needs neither an elevated session
//! nor the Windows Performance Toolkit to be installed — a developer profiling
//! their own program has every right the walk requires.
//!
//! Each sample is weighted by the **cycles** the thread turned since the
//! previous tick, which is the `cycles` event and is what makes the numbers
//! mean something on a machine with idle threads on it. Weighting by the tick
//! instead would give a thread-pool worker that woke for a microsecond the same
//! credit as the thread that spent the whole millisecond interpreting, and a
//! process with a dozen sleeping workers would report itself as mostly asleep.
//!
//! The cycle counter is also what liveness is read from. `GetThreadTimes` only
//! advances on the scheduler tick — about every 15 ms — so at a kilohertz it
//! would throw away nineteen samples out of twenty and report a program as
//! sleeping through its own hot loop.

mod dbghelp;

use std::collections::HashMap;
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    ADDRESS64, AddrModeFlat, CONTEXT, GetThreadContext, STACKFRAME64, StackWalk64,
    SymFunctionTableAccess64, SymGetModuleBase64,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{OpenProcess, OpenThread, ResumeThread, SuspendThread};
use windows_sys::Win32::System::WindowsProgramming::QueryThreadCycleTime;

use self::dbghelp::{Resolved, Symbols, last_error};
use crate::clock::Ticker;
use crate::collect::{CollectError, CollectOptions, Launch};
use crate::model::{Frame, Nanos, Profile, Sample, ThreadId, ThreadRecord, View};
use crate::symbols::KiraSymbols;

/// Query the target's modules and read its memory: what an unwinder needs.
const PROCESS_ACCESS: u32 = 0x0400 | 0x0010;

/// Suspend, read the register context of, and query one thread.
const THREAD_ACCESS: u32 = 0x0002 | 0x0008 | 0x0040;

/// Start the child held at its first instruction.
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// How often the thread and module lists are rebuilt once the program is up.
const THREAD_REFRESH: std::time::Duration = std::time::Duration::from_millis(250);

/// How many ticks the lists are rebuilt on every one of, from the start.
const SETTLING_TICKS: u32 = 200;

#[cfg(target_arch = "x86_64")]
use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT_FULL_AMD64 as CONTEXT_FULL;
#[cfg(target_arch = "x86_64")]
const STACK_MACHINE: u32 = 0x8664;

#[cfg(target_arch = "aarch64")]
use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT_FULL_ARM64 as CONTEXT_FULL;
#[cfg(target_arch = "aarch64")]
const STACK_MACHINE: u32 = 0xAA64;

/// A child running under the sampler.
#[derive(Debug)]
pub(super) struct Recorder {
    child: Child,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<Collected, CollectError>>>,
}

impl Recorder {
    /// The name a report gives this collector.
    pub(super) const TOOL: &'static str = "windows-dbghelp";

    pub(super) fn start(launch: &Launch, options: &CollectOptions) -> Result<Self, CollectError> {
        let mut command = Command::new(&launch.program);
        command.args(&launch.arguments);
        for (key, value) in &launch.environment {
            command.env(key, value);
        }
        // Started suspended, and released only once the sampler has opened its
        // symbol session. Loading a large program's debug records takes tens of
        // milliseconds, which is longer than a fast program runs for — a
        // recording that let the child start first would report nothing at all
        // and look like a program that did nothing.
        command.creation_flags(CREATE_SUSPENDED);
        let child = command.spawn().map_err(|source| CollectError::Spawn {
            program: launch.program.clone(),
            source,
        })?;
        let process = OwnedProcess::open(child.id())?;
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let options = *options;
        let pid = child.id();
        let origin = Instant::now();
        // The program's own debug records sit beside it, which is nowhere the
        // default symbol search would look.
        let search = launch.program.parent().map(std::path::Path::to_path_buf);
        let (ready, started) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("kira-profile-machine".to_owned())
            .spawn(move || {
                sample(
                    process,
                    pid,
                    &signal,
                    options,
                    origin,
                    search.as_deref(),
                    &ready,
                )
            })
            .map_err(|source| CollectError::Io {
                action: "starting the sampling thread".to_owned(),
                source,
            })?;
        let opened = started.recv().unwrap_or(Ok(()));
        let recorder = Self {
            child,
            stop,
            worker: Some(worker),
        };
        // Released whatever the sampler reported, because a child left
        // suspended is a program that never runs — a recording with no machine
        // view is a worse outcome than no recording at all.
        resume_process(pid);
        opened?;
        Ok(recorder)
    }

    pub(super) fn wait(&mut self) -> Result<i32, CollectError> {
        let status = self.child.wait().map_err(|source| CollectError::Io {
            action: "waiting for the program".to_owned(),
            source,
        })?;
        self.stop.store(true, Ordering::Relaxed);
        Ok(status.code().unwrap_or(1))
    }

    pub(super) fn finish(mut self, symbols: &KiraSymbols) -> Result<Profile, CollectError> {
        self.stop.store(true, Ordering::Relaxed);
        let collected = match self.worker.take() {
            Some(worker) => worker.join().map_err(|_| CollectError::Tool {
                tool: Self::TOOL,
                problem: "the sampling thread ended abnormally".to_owned(),
            })??,
            None => Collected::default(),
        };
        Ok(collected.into_profile(symbols))
    }
}

/// A process handle owned by the sampler thread.
///
/// Holding it also holds the process id, so a walk can never be redirected at
/// another program that reused the id after the child exited.
#[derive(Debug)]
struct OwnedProcess(HANDLE);

// SAFETY: a process handle is a kernel object with no thread affinity, owned
// exclusively by this value, which closes it exactly once on drop.
unsafe impl Send for OwnedProcess {}

impl OwnedProcess {
    fn open(pid: u32) -> Result<Self, CollectError> {
        // SAFETY: opening a handle to a child this process just created, with
        // the two rights an unwinder needs; failure is a null handle.
        let handle = unsafe { OpenProcess(PROCESS_ACCESS, 0, pid) };
        if handle.is_null() {
            return Err(CollectError::Platform {
                call: "OpenProcess",
                code: last_error(),
            });
        }
        Ok(Self(handle))
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenProcess` and is closed once, here.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// A thread handle owned by the sampler thread.
#[derive(Debug)]
struct OwnedThread(HANDLE);

impl Drop for OwnedThread {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenThread` and is closed once, here.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// A toolhelp snapshot, closed when the walk over it ends.
#[derive(Debug)]
struct OwnedSnapshot(HANDLE);

impl Drop for OwnedSnapshot {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateToolhelp32Snapshot` and is
        // closed once, here.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// What the sampler knows about one thread between ticks.
#[derive(Debug)]
struct ThreadState {
    handle: OwnedThread,
    ordinal: u32,
    cycles: u64,
}

/// One sample, before its addresses have names.
#[derive(Debug, Default)]
struct RawSample {
    thread: u32,
    time: Nanos,
    weight: u64,
    addresses: Vec<u64>,
}

/// Everything one sampling run produced.
#[derive(Debug, Default)]
struct Collected {
    samples: Vec<RawSample>,
    resolved: Vec<Resolved>,
    addresses: Vec<Vec<u32>>,
    threads: Vec<u32>,
    lost: u64,
    frequency: u32,
}

fn sample(
    process: OwnedProcess,
    pid: u32,
    stop: &AtomicBool,
    options: CollectOptions,
    origin: Instant,
    search: Option<&std::path::Path>,
    ready: &std::sync::mpsc::Sender<Result<(), CollectError>>,
) -> Result<Collected, CollectError> {
    let mut symbols = match Symbols::open(process.0, search) {
        Ok(symbols) => symbols,
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(Collected::default());
        }
    };
    let mut ticker = Ticker::new(options.frequency);
    let mut collected = Collected {
        frequency: options.frequency,
        ..Collected::default()
    };
    let mut states: HashMap<u32, ThreadState> = HashMap::new();
    let mut refreshed = Instant::now();
    let mut addresses = Vec::with_capacity(options.max_depth as usize);
    refresh_threads(pid, &mut states, &mut collected);
    let _ = ready.send(Ok(()));

    let mut ticks = 0u32;
    while !stop.load(Ordering::Relaxed) {
        ticker.wait();
        ticks = ticks.saturating_add(1);
        // The program was released from its first instruction a moment ago and
        // loads its modules over the next few milliseconds, so the module list
        // is caught up on every tick until it has settled. Listing modules
        // costs microseconds.
        if ticks <= SETTLING_TICKS {
            symbols.refresh();
        }
        // The thread list is not: a system thread snapshot is the one call here
        // that costs tens of milliseconds, and doing it per tick would drop the
        // sampling rate to a few dozen hertz. The threads the program starts
        // with are known before it is released, and any it starts later are
        // picked up on this cadence.
        if refreshed.elapsed() >= THREAD_REFRESH {
            refresh_threads(pid, &mut states, &mut collected);
            symbols.refresh();
            refreshed = Instant::now();
        }
        let time = Nanos::new(origin.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        for state in states.values_mut() {
            let Some(cycles) = thread_cycles(&state.handle) else {
                continue;
            };
            let burned = cycles.saturating_sub(state.cycles);
            state.cycles = cycles;
            // A thread that turned no cycles since the previous tick was not
            // on a processor, and a profile of what ran should not say it was.
            if burned == 0 {
                continue;
            }
            addresses.clear();
            if !walk(process.0, &state.handle, options.max_depth, &mut addresses) {
                collected.lost = collected.lost.saturating_add(1);
                continue;
            }
            addresses.reverse();
            collected.samples.push(RawSample {
                thread: state.ordinal,
                time,
                weight: burned,
                addresses: addresses.clone(),
            });
        }
    }

    symbols.refresh();
    resolve(&symbols, &mut collected);
    Ok(collected)
}

/// Rebuilds the thread list, keeping the handles already open.
fn refresh_threads(pid: u32, states: &mut HashMap<u32, ThreadState>, collected: &mut Collected) {
    let Some(live) = thread_ids(pid) else {
        return;
    };
    states.retain(|tid, _| live.contains(tid));
    for tid in live {
        if states.contains_key(&tid) {
            continue;
        }
        // SAFETY: opening a thread of the child by id, with the rights a walk
        // needs; failure is a null handle, checked before it is stored.
        let handle = unsafe { OpenThread(THREAD_ACCESS, 0, tid) };
        if handle.is_null() {
            continue;
        }
        let ordinal = match collected.threads.iter().position(|known| *known == tid) {
            Some(index) => index as u32,
            None => {
                collected.threads.push(tid);
                (collected.threads.len() - 1) as u32
            }
        };
        states.insert(
            tid,
            ThreadState {
                handle: OwnedThread(handle),
                ordinal,
                cycles: 0,
            },
        );
    }
}

/// Lets every thread of `pid` run.
///
/// A process created suspended has exactly one thread, held at its first
/// instruction; resuming it is what starts the program.
fn resume_process(pid: u32) {
    let Some(threads) = thread_ids(pid) else {
        return;
    };
    for tid in threads {
        // SAFETY: opening a thread of the child by id for the one right this
        // needs; failure is a null handle, and the handle is closed on drop.
        let handle = unsafe { OpenThread(THREAD_ACCESS, 0, tid) };
        if handle.is_null() {
            continue;
        }
        let thread = OwnedThread(handle);
        // SAFETY: resuming a thread of the child this recorder created.
        unsafe {
            ResumeThread(thread.0);
        }
    }
}

/// Every thread id belonging to `pid`.
fn thread_ids(pid: u32) -> Option<Vec<u32>> {
    // SAFETY: a snapshot of the system's threads; failure is reported as a
    // null handle, and the snapshot is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot.is_null() {
        return None;
    }
    let snapshot = OwnedSnapshot(snapshot);
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut ids = Vec::new();
    // SAFETY: `entry` declares its own size, which is what the walk reads
    // before it writes, and the snapshot handle is live for both calls.
    let mut more = unsafe { Thread32First(snapshot.0, &mut entry) != 0 };
    while more {
        if entry.th32OwnerProcessID == pid {
            ids.push(entry.th32ThreadID);
        }
        // SAFETY: same snapshot and entry, walked to exhaustion.
        more = unsafe { Thread32Next(snapshot.0, &mut entry) != 0 };
    }
    Some(ids)
}

/// The processor cycles a thread has turned since it started.
fn thread_cycles(thread: &OwnedThread) -> Option<u64> {
    let mut cycles = 0u64;
    // SAFETY: a caller-owned `u64` and a live thread handle.
    let read = unsafe { QueryThreadCycleTime(thread.0, &mut cycles) != 0 };
    read.then_some(cycles)
}

/// Suspends `thread` and unwinds it, returning whether a stack was read.
fn walk(process: HANDLE, thread: &OwnedThread, max_depth: u32, out: &mut Vec<u64>) -> bool {
    // SAFETY: suspending a thread of the child; `u32::MAX` reports failure,
    // and every path below resumes exactly the threads that were suspended.
    let suspended = unsafe { SuspendThread(thread.0) };
    if suspended == u32::MAX {
        return false;
    }
    let walked = walk_suspended(process, thread, max_depth, out);
    // SAFETY: resuming the thread this call suspended.
    unsafe {
        ResumeThread(thread.0);
    }
    walked
}

fn walk_suspended(
    process: HANDLE,
    thread: &OwnedThread,
    max_depth: u32,
    out: &mut Vec<u64>,
) -> bool {
    // SAFETY: `CONTEXT` is a plain register record; every field the call reads
    // is `ContextFlags`, set immediately below.
    let mut context: CONTEXT = unsafe { std::mem::zeroed() };
    context.ContextFlags = CONTEXT_FULL;
    // SAFETY: the thread is suspended and the context is caller-owned.
    let read = unsafe { GetThreadContext(thread.0, &mut context) != 0 };
    if !read {
        return false;
    }
    let mut frame = STACKFRAME64 {
        AddrPC: flat(program_counter(&context)),
        AddrFrame: flat(frame_pointer(&context)),
        AddrStack: flat(stack_pointer(&context)),
        ..STACKFRAME64::default()
    };
    for _ in 0..max_depth {
        // SAFETY: the thread is suspended, the frame and context are
        // caller-owned, and the two callbacks are DbgHelp's own, which is what
        // its documentation requires for a target opened with `SymInitialize`.
        let stepped = unsafe {
            StackWalk64(
                STACK_MACHINE,
                process,
                thread.0,
                &mut frame,
                std::ptr::from_mut(&mut context).cast(),
                None,
                Some(SymFunctionTableAccess64),
                Some(SymGetModuleBase64),
                None,
            ) != 0
        };
        if !stepped || frame.AddrPC.Offset == 0 {
            break;
        }
        out.push(frame.AddrPC.Offset);
    }
    !out.is_empty()
}

fn flat(offset: u64) -> ADDRESS64 {
    ADDRESS64 {
        Offset: offset,
        Segment: 0,
        Mode: AddrModeFlat,
    }
}

#[cfg(target_arch = "x86_64")]
fn program_counter(context: &CONTEXT) -> u64 {
    context.Rip
}

#[cfg(target_arch = "x86_64")]
fn frame_pointer(context: &CONTEXT) -> u64 {
    context.Rbp
}

#[cfg(target_arch = "x86_64")]
fn stack_pointer(context: &CONTEXT) -> u64 {
    context.Rsp
}

#[cfg(target_arch = "aarch64")]
fn program_counter(context: &CONTEXT) -> u64 {
    context.Pc
}

#[cfg(target_arch = "aarch64")]
fn frame_pointer(context: &CONTEXT) -> u64 {
    // SAFETY: the register union's two arms are the same 31 words; reading the
    // named arm is how the frame pointer is spelled on this architecture.
    unsafe { context.Anonymous.Anonymous.Fp }
}

#[cfg(target_arch = "aarch64")]
fn stack_pointer(context: &CONTEXT) -> u64 {
    context.Sp
}

/// Gives every address a name, once per distinct address.
fn resolve(symbols: &Symbols, collected: &mut Collected) {
    let mut seen: HashMap<u64, u32> = HashMap::new();
    collected.addresses = Vec::with_capacity(collected.samples.len());
    for sample in &collected.samples {
        let mut indices = Vec::with_capacity(sample.addresses.len());
        for address in &sample.addresses {
            let index = match seen.get(address) {
                Some(index) => *index,
                None => {
                    let index = collected.resolved.len() as u32;
                    collected.resolved.push(symbols.resolve(*address));
                    seen.insert(*address, index);
                    index
                }
            };
            indices.push(index);
        }
        collected.addresses.push(indices);
    }
}

impl Collected {
    fn into_profile(self, symbols: &KiraSymbols) -> Profile {
        let mut profile = Profile::new(View::Machine, "cycles", Recorder::TOOL);
        profile.unit = crate::model::Unit::Cycles;
        profile.frequency = self.frequency;
        profile.lost = self.lost;
        for (index, tid) in self.threads.iter().enumerate() {
            profile.threads.push(ThreadRecord {
                id: ThreadId::new(index as u32),
                name: format!("tid-{tid}"),
            });
        }
        let frames = self
            .resolved
            .iter()
            .map(|resolved| {
                let identity = symbols.classify(&resolved.symbol, &resolved.object);
                let name = profile.frames.name(&identity.name);
                let object = profile.frames.name(&resolved.object);
                let file = resolved.file.as_ref().map(|file| profile.frames.name(file));
                profile.frames.insert(Frame {
                    symbol: name,
                    kind: identity.kind,
                    object,
                    function: identity.function,
                    offset: Some(resolved.offset),
                    file,
                    line: resolved.line,
                })
            })
            .collect::<Vec<_>>();
        for (sample, indices) in self.samples.iter().zip(&self.addresses) {
            profile.samples.push(Sample {
                thread: ThreadId::new(sample.thread),
                time: sample.time,
                weight: sample.weight,
                stack: indices
                    .iter()
                    .filter_map(|index| frames.get(*index as usize).copied())
                    .collect(),
            });
        }
        profile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_of_this_process_reports_cycles_it_has_turned() {
        // SAFETY: the pseudo-handle for the calling thread needs no release.
        let handle = unsafe { windows_sys::Win32::System::Threading::GetCurrentThread() };
        let thread = std::mem::ManuallyDrop::new(OwnedThread(handle));
        let first = thread_cycles(&thread).expect("this thread reports cycles");
        std::hint::black_box((0..100_000u64).sum::<u64>());
        let second = thread_cycles(&thread).expect("this thread reports cycles");
        assert!(second > first, "{first} -> {second}");
    }

    #[test]
    fn this_process_has_threads_the_sampler_can_find() {
        let ids = thread_ids(std::process::id()).expect("a thread snapshot");
        assert!(!ids.is_empty());
    }
}
