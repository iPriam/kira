//! Raw Linux system calls with nothing beneath them: no libc, no crt, no
//! allocator, no std. This is the floor of the freestanding runtime — the
//! layer a hosted build reaches through libc instead.
//!
//! Every wrapper is `pub` so callers above this crate (an allocator over
//! `mmap`, a console over `write`, an exit path over `exit_group`) never grow
//! their own inline assembly; one crate owns the `svc`/`syscall` instruction
//! for all of Kira, which is also what makes an audit of "what does the
//! userland ask the kernel for directly" a read of one file.
//!
//! Errors are kernel errno values, not OS types: on a freestanding target
//! there is no `strerror`, and the meaning of a number is a property of the
//! kernel ABI, not of a host library. Because these are raw calls, every
//! answer follows the kernel's own convention — including `mmap`, whose
//! negated-errno failure the libc wrapper turns into `MAP_FAILED`, a value
//! that does not exist at this layer.
//!
//! On any target other than Linux aarch64 or Linux x86_64 the crate compiles
//! empty: it makes no promise about kernels it cannot speak to.

#![no_std]

#[cfg(test)]
extern crate std;

use core::fmt;

/// A kernel errno, as returned negated by the raw syscall convention.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Errno(pub u32);

impl Errno {
    /// The name the kernel's own headers give this errno.
    ///
    /// A freestanding program has no `strerror`; this fixed table is what
    /// error reporting above this crate prints instead.
    pub fn name(self) -> &'static str {
        match self.0 {
            1 => "EPERM",
            2 => "ENOENT",
            4 => "EINTR",
            5 => "EIO",
            9 => "EBADF",
            11 => "EAGAIN",
            12 => "ENOMEM",
            13 => "EACCES",
            14 => "EFAULT",
            16 => "EBUSY",
            17 => "EEXIST",
            20 => "ENOTDIR",
            21 => "EISDIR",
            22 => "EINVAL",
            24 => "EMFILE",
            27 => "EFBIG",
            28 => "ENOSPC",
            30 => "EROFS",
            32 => "EPIPE",
            _ => "UNKNOWN",
        }
    }
}

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.0)
    }
}

/// Decodes a raw syscall return value under the kernel convention:
/// `-4095 ..= -1` encodes `-errno`, anything else is a success count.
///
/// Public because it *is* the convention: a caller above this crate that must
/// issue a number this crate has no wrapper for yet decodes its answer here,
/// rather than growing a second copy of the rule.
pub fn decode(raw: isize) -> Result<usize, Errno> {
    // As unsigned, `-4095..=-1` is exactly `usize::MAX - 4094 ..= usize::MAX`;
    // the errno is the absolute value of the negation.
    let unsigned = raw as usize;
    if unsigned > usize::MAX - 4095 {
        Err(Errno((!unsigned).wrapping_add(1) as u32))
    } else {
        Ok(unsigned)
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
mod imp {
    // The numbers are per architecture and differ even where the names do not;
    // each pair is stated once, beside the assembly that issues them.
    #[cfg(target_arch = "aarch64")]
    pub(super) const WRITE: usize = 64;
    #[cfg(target_arch = "aarch64")]
    pub(super) const MMAP: usize = 222;
    #[cfg(target_arch = "aarch64")]
    pub(super) const MUNMAP: usize = 215;
    #[cfg(target_arch = "aarch64")]
    pub(super) const EXIT_GROUP: usize = 94;

    #[cfg(target_arch = "x86_64")]
    pub(super) const WRITE: usize = 1;
    #[cfg(target_arch = "x86_64")]
    pub(super) const MMAP: usize = 9;
    #[cfg(target_arch = "x86_64")]
    pub(super) const MUNMAP: usize = 11;
    #[cfg(target_arch = "x86_64")]
    pub(super) const EXIT_GROUP: usize = 231;

    /// Issues syscall number `NR` with six register arguments and returns the
    /// raw kernel answer.
    ///
    /// # Safety
    /// Each caller vouches for its own pointer arguments: the kernel
    /// dereferences whatever it is handed in this address space.
    #[cfg(target_arch = "aarch64")]
    pub(super) unsafe fn raw<const NR: usize>(args: [usize; 6]) -> isize {
        let ret;
        // SAFETY: the `svc` enters the kernel with arguments in x0-x5 and the
        // number in x8; the kernel answers in x0 and may clobber x1-x18, so
        // every one of those is listed and none may be relied on afterwards.
        unsafe {
            core::arch::asm!(
                "svc #0",
                inlateout("x0") args[0] => ret,
                in("x1") args[1],
                in("x2") args[2],
                in("x3") args[3],
                in("x4") args[4],
                in("x5") args[5],
                in("x8") NR,
                lateout("x9") _, lateout("x10") _, lateout("x11") _,
                lateout("x12") _, lateout("x13") _, lateout("x14") _,
                lateout("x15") _, lateout("x16") _, lateout("x17") _,
                options(nostack)
            );
        }
        ret
    }

    /// Issues syscall number `NR` with six register arguments and returns the
    /// raw kernel answer.
    ///
    /// # Safety
    /// Each caller vouches for its own pointer arguments: the kernel
    /// dereferences whatever it is handed in this address space.
    #[cfg(target_arch = "x86_64")]
    pub(super) unsafe fn raw<const NR: usize>(args: [usize; 6]) -> isize {
        let ret;
        // SAFETY: the `syscall` instruction takes the number in rax and the
        // arguments in rdi, rsi, rdx, r10, r8, r9; it answers in rax and
        // clobbers rcx and r11 by contract.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") NR => ret,
                in("rdi") args[0],
                in("rsi") args[1],
                in("rdx") args[2],
                in("r10") args[3],
                in("r8") args[4],
                in("r9") args[5],
                lateout("rcx") _, lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }

    /// Writes `bytes` to `fd`, once.
    ///
    /// A short write is reported as the count written, never retried here:
    /// only the caller knows whether the rest of the buffer still has meaning.
    pub fn write(fd: i32, bytes: &[u8]) -> Result<usize, crate::Errno> {
        // SAFETY: `bytes` is a live slice whose length bounds what the kernel
        // reads.
        crate::decode(unsafe {
            raw::<WRITE>([fd as usize, bytes.as_ptr() as usize, bytes.len(), 0, 0, 0])
        })
    }

    /// Ends every thread of the process with `code`.
    ///
    /// `exit_group` rather than `exit`: teardown must end the process even if
    /// a thread came into existence behind the runtime's back.
    pub fn exit_group(code: i32) -> ! {
        // SAFETY: the only argument is the status; the kernel does not return.
        unsafe { raw::<EXIT_GROUP>([code as usize, 0, 0, 0, 0, 0]) };
        unreachable!("the kernel does not return from exit_group")
    }

    /// Maps `length` bytes of anonymous private read-write memory,
    /// page-rounded up by the kernel.
    ///
    /// This is the freestanding allocator's only source of memory.
    ///
    /// # Safety
    /// The mapping is unreadable until initialized, like any fresh allocation.
    pub unsafe fn mmap_anonymous(length: usize) -> Result<*mut u8, crate::Errno> {
        const PROT_READ_WRITE: usize = 1 | 2;
        const MAP_PRIVATE_ANONYMOUS: usize = 0x02 | 0x20;
        // SAFETY: a fresh anonymous mapping; nothing points into it yet.
        crate::decode(unsafe {
            raw::<MMAP>([
                0,
                length,
                PROT_READ_WRITE,
                MAP_PRIVATE_ANONYMOUS,
                (-1isize) as usize,
                0,
            ])
        })
        .map(|address| address as *mut u8)
    }

    /// Releases a mapping this crate handed out.
    ///
    /// # Safety
    /// `[ptr, ptr + length)` must be a live mapping from [`mmap_anonymous`]
    /// that nothing will touch again.
    pub unsafe fn munmap(ptr: *mut u8, length: usize) -> Result<(), crate::Errno> {
        // SAFETY: the caller vouches for the mapping and its extent.
        crate::decode(unsafe { raw::<MUNMAP>([ptr as usize, length, 0, 0, 0, 0]) }).map(|_| ())
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
pub use imp::{exit_group, mmap_anonymous, munmap, write};

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;

    /// The kernel encodes failures as `-errno`, so the boundary between a
    /// large success count and an error must fall exactly at -4095.
    #[test]
    fn errno_encoding_matches_the_kernel_convention() {
        assert_eq!(decode(0), Ok(0));
        assert_eq!(decode(4096), Ok(4096));
        assert_eq!(decode(-1), Err(Errno(1)));
        assert_eq!(decode(-12), Err(Errno(12)));
        assert_eq!(decode(-4095), Err(Errno(4095)));
        // -4096 would be errno 4096, which no kernel errno is; the encoding
        // reserves it, and this contract reports it as a success count.
        assert_eq!(decode(-4096), Ok((-4096isize) as usize));
    }

    /// Error reporting prints the header name, not just the number.
    #[test]
    fn errnos_name_themselves() {
        assert_eq!(Errno(12).name(), "ENOMEM");
        assert_eq!(Errno(32).name(), "EPIPE");
        assert_eq!(Errno(999).name(), "UNKNOWN");
        assert_eq!(format!("{}", Errno(2)), "ENOENT (2)");
    }
}
