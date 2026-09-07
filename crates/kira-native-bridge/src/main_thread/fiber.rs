//! Platform cooperative fibers for main-thread lifecycles.

#[cfg(all(unix, any(target_arch = "aarch64", target_arch = "x86_64")))]
mod unix;
#[cfg(all(
    not(all(unix, any(target_arch = "aarch64", target_arch = "x86_64"))),
    not(target_os = "windows")
))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(all(unix, any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) use unix::{LifecycleEntry, active, pump, reset, start};
#[cfg(all(
    not(all(unix, any(target_arch = "aarch64", target_arch = "x86_64"))),
    not(target_os = "windows")
))]
pub(super) use unsupported::{LifecycleEntry, active, pump, reset, start};
#[cfg(target_os = "windows")]
pub(super) use windows::{LifecycleEntry, active, pump, reset, start};
