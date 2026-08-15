//! Waiting for the next sample without drifting.
//!
//! A sampler is only as good as its clock. The period is held against a fixed
//! origin rather than added to "now" after each wake, so a slow sample steals
//! from the next interval instead of stretching every interval after it, and a
//! profile's sample times stay evenly spaced whatever the machine is doing.
//!
//! Windows needs its own waiter. `Sleep` there rounds up to the system timer
//! resolution, which is 15.6 ms by default — a thousand times the period a
//! kilohertz sampler asks for — so this uses the high-resolution waitable timer
//! instead, which is exact to a hundred nanoseconds.

use std::time::{Duration, Instant};

/// A fixed-period wake-up.
#[derive(Debug)]
pub struct Ticker {
    period: Duration,
    origin: Instant,
    ticks: u64,
    waiter: Waiter,
}

impl Ticker {
    /// A ticker that wakes `frequency` times a second.
    ///
    /// A frequency of zero would never wake; it is read as one hertz, which is
    /// the slowest a sampler can be asked for rather than a refusal to run.
    #[must_use]
    pub fn new(frequency: u32) -> Self {
        let period = Duration::from_nanos(1_000_000_000 / u64::from(frequency.max(1)));
        Self {
            period,
            origin: Instant::now(),
            ticks: 0,
            waiter: Waiter::new(),
        }
    }

    /// The interval between wake-ups.
    #[must_use]
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Sleeps until the next tick.
    ///
    /// Ticks already missed are dropped rather than fired back to back: a
    /// sampler that fell behind should keep sampling the present, not race to
    /// catch up on a past it can no longer observe.
    pub fn wait(&mut self) {
        self.ticks += 1;
        let target = self.origin + self.period * u32::try_from(self.ticks).unwrap_or(u32::MAX);
        let now = Instant::now();
        if target <= now {
            let behind = now.duration_since(self.origin).as_nanos();
            let period = self.period.as_nanos().max(1);
            self.ticks = (behind / period) as u64 + 1;
            return;
        }
        self.waiter.sleep(target - now);
    }
}

#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Threading::{
        CreateWaitableTimerExW, INFINITE, SetWaitableTimer, WaitForSingleObject,
    };

    /// Ask for a timer whose resolution is the hardware's rather than the
    /// scheduler tick. Named here because the Win32 metadata does not carry it.
    const HIGH_RESOLUTION: u32 = 0x0000_0002;

    /// Every right a thread needs over a timer it created.
    const TIMER_ALL_ACCESS: u32 = 0x001F_0003;

    /// A high-resolution waitable timer, or nothing when the system has none.
    #[derive(Debug)]
    pub(super) struct Waiter {
        timer: Option<Timer>,
    }

    /// An owned timer handle.
    ///
    /// The handle is created by this thread and closed on drop; nothing else
    /// ever holds it, which is what makes moving it between threads sound.
    #[derive(Debug)]
    struct Timer(HANDLE);

    // SAFETY: a waitable timer handle is a kernel object with no thread
    // affinity, and this one is owned exclusively by the `Timer` that closes
    // it, so no other thread can observe it after the move.
    unsafe impl Send for Timer {}

    impl Drop for Timer {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateWaitableTimerExW` and is
            // closed exactly once, here.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    impl Waiter {
        pub(super) fn new() -> Self {
            // SAFETY: an unnamed timer with no security attributes; the call
            // reports failure as a null handle, which is checked below.
            let handle = unsafe {
                CreateWaitableTimerExW(
                    std::ptr::null(),
                    std::ptr::null(),
                    HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            let usable = !handle.is_null() && handle != INVALID_HANDLE_VALUE;
            Self {
                timer: usable.then_some(Timer(handle)),
            }
        }

        pub(super) fn sleep(&self, duration: Duration) {
            let Some(timer) = &self.timer else {
                std::thread::sleep(duration);
                return;
            };
            // A negative due time is relative, in hundred-nanosecond units.
            let units = i64::try_from(duration.as_nanos() / 100).unwrap_or(i64::MAX);
            let due = -units.max(1);
            // SAFETY: `timer` is a live timer handle this waiter owns; the
            // call sets a one-shot relative due time with no completion
            // routine, and the wait that follows is on the same handle.
            let armed =
                unsafe { SetWaitableTimer(timer.0, &due, 0, None, std::ptr::null(), 0) != 0 };
            if !armed {
                std::thread::sleep(duration);
                return;
            }
            // SAFETY: waiting on a handle this waiter owns and keeps alive
            // across the call.
            unsafe {
                WaitForSingleObject(timer.0, INFINITE);
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::time::Duration;

    /// Every other platform sleeps to the nanosecond already.
    #[derive(Debug)]
    pub(super) struct Waiter;

    impl Waiter {
        pub(super) fn new() -> Self {
            Waiter
        }

        pub(super) fn sleep(&self, duration: Duration) {
            std::thread::sleep(duration);
        }
    }
}

use platform::Waiter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticker_holds_its_period_against_a_fixed_origin() {
        let mut ticker = Ticker::new(1_000);
        assert_eq!(ticker.period(), Duration::from_millis(1));
        let start = Instant::now();
        for _ in 0..20 {
            ticker.wait();
        }
        let elapsed = start.elapsed();
        // Twenty ticks of a millisecond, with room for a busy machine. The
        // point of the assertion is the lower bound: a ticker that returned
        // immediately would be a sampler with no period at all.
        assert!(elapsed >= Duration::from_millis(15), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
    }

    #[test]
    fn ticks_already_missed_are_dropped_rather_than_fired_back_to_back() {
        let mut ticker = Ticker::new(10_000);
        std::thread::sleep(Duration::from_millis(20));
        let start = Instant::now();
        ticker.wait();
        assert!(start.elapsed() < Duration::from_millis(5));
    }
}
