//! The Windows symbol handler, which is what turns an address into a name.
//!
//! DbgHelp is single-threaded per process: every `Sym*` call for one target has
//! to come from the thread that initialised it. [`Symbols`] therefore owns the
//! session and never leaves the sampler thread, and the sampler resolves every
//! address it collected before it ends rather than handing raw addresses back.

use std::collections::HashSet;
use std::ffi::CStr;
use std::path::Path;

use windows_sys::Win32::Foundation::{HANDLE, HMODULE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    IMAGEHLP_LINE64, IMAGEHLP_MODULE64, SYMBOL_INFO, SymCleanup, SymFromAddr, SymGetLineFromAddr64,
    SymGetModuleInfo64, SymInitialize, SymLoadModuleEx, SymSetOptions,
};

use windows_sys::Win32::System::ProcessStatus::{
    EnumProcessModules, GetModuleFileNameExA, GetModuleInformation, MODULEINFO,
};

use crate::collect::CollectError;

/// Undecorate names, never put a dialog in front of a profiler, and read a
/// module's debug records only when an address needs them.
///
/// Deferring matters more here than it does in a debugger: the sampler is
/// unwinding a program that is running, and the unwinder needs a module's
/// records the first time it walks through it. Line tables are deliberately
/// *not* asked for: they are the bulk of a large program's debug records, they
/// would be read on the sampling thread, and the machine view does not show
/// source lines — `perf report` does not either. Kira frames carry their source
/// location in the Kira view, which is where a reader looks for it.
const SYMBOL_OPTIONS: u32 =
    SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_FAIL_CRITICAL_ERRORS | SYMOPT_NO_PROMPTS;

const SYMOPT_UNDNAME: u32 = 0x0000_0002;
const SYMOPT_DEFERRED_LOADS: u32 = 0x0000_0004;
const SYMOPT_FAIL_CRITICAL_ERRORS: u32 = 0x0000_0200;
const SYMOPT_NO_PROMPTS: u32 = 0x0008_0000;

/// The most modules one target is expected to have loaded.
const MAX_MODULES: usize = 512;

/// The longest module path this reads back, including its terminator.
const MAX_PATH_BYTES: usize = 1024;

/// The longest symbol name DbgHelp will write.
const MAX_SYM_NAME: usize = 2000;

/// What one address turned out to be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Resolved {
    /// The symbol name, empty when no symbol covered the address.
    pub(super) symbol: String,
    /// The image the address fell in.
    pub(super) object: String,
    /// Bytes from the start of the symbol.
    pub(super) offset: u32,
    /// The source file, when a line table covered the address.
    pub(super) file: Option<String>,
    /// The source line, when a line table covered the address.
    pub(super) line: Option<u32>,
}

/// A symbol session for one process.
///
/// Not `Send`: DbgHelp binds the session to the thread that opened it, and the
/// sampler is the only thread that ever touches one.
#[derive(Debug)]
pub(super) struct Symbols {
    process: HANDLE,
    open: bool,
    /// The base address of every module already registered with the session.
    loaded: HashSet<u64>,
}

impl Symbols {
    /// Opens a symbol session over `process`, loading the modules it has now.
    ///
    /// `search` is the directory the target's own debug records sit in. It has
    /// to be named: the default search asks the current process's directory and
    /// the working directory, and a profiler that launched a program from
    /// somewhere else is in neither.
    ///
    /// The target's modules are *not* enumerated here. A recording opens its
    /// symbol session against a process held at its first instruction, whose
    /// loader has not run and which therefore has no modules to enumerate —
    /// asking for them fails the whole call. [`Symbols::refresh`] picks them up
    /// as the program loads them.
    pub(super) fn open(process: HANDLE, search: Option<&Path>) -> Result<Self, CollectError> {
        let path = search.map(|path| {
            let mut bytes = path.to_string_lossy().into_owned().into_bytes();
            bytes.push(0);
            bytes
        });
        let pointer = path
            .as_ref()
            .map_or(std::ptr::null(), |bytes| bytes.as_ptr());
        // SAFETY: the options call takes no pointers; the initialise call is
        // given a live process handle this recorder owns and a null-terminated
        // search path that outlives the call.
        let opened = unsafe {
            SymSetOptions(SYMBOL_OPTIONS);
            SymInitialize(process, pointer, 0) != 0
        };
        if !opened {
            return Err(CollectError::Platform {
                call: "SymInitialize",
                code: last_error(),
            });
        }
        Ok(Self {
            process,
            open: true,
            loaded: HashSet::new(),
        })
    }

    /// Registers modules the program has loaded since the last call.
    ///
    /// Not `SymRefreshModuleList`: that re-reads every module the target has,
    /// costs tens of milliseconds each time, and the sampler needs to notice
    /// new modules on the tick after they appear. Listing the target's modules
    /// and registering only the ones that are new costs microseconds, which is
    /// what lets a program that runs for a tenth of a second be sampled at all.
    pub(super) fn refresh(&mut self) {
        let mut modules = [std::ptr::null_mut(); MAX_MODULES];
        let mut needed = 0u32;
        let capacity = (size_of::<HMODULE>() * MAX_MODULES) as u32;
        // SAFETY: a caller-owned array whose byte capacity is passed with it,
        // and a live process handle opened for querying and reading.
        let listed = unsafe {
            EnumProcessModules(self.process, modules.as_mut_ptr(), capacity, &mut needed) != 0
        };
        if !listed {
            return;
        }
        let count = (needed as usize / size_of::<HMODULE>()).min(MAX_MODULES);
        for module in &modules[..count] {
            let mut information = MODULEINFO::default();
            // SAFETY: a caller-owned `MODULEINFO` with its size, a module
            // handle this process just listed, and the same live handle.
            let described = unsafe {
                GetModuleInformation(
                    self.process,
                    *module,
                    &mut information,
                    size_of::<MODULEINFO>() as u32,
                ) != 0
            };
            if !described {
                continue;
            }
            let base = information.lpBaseOfDll as u64;
            if !self.loaded.insert(base) {
                continue;
            }
            let mut name = [0u8; MAX_PATH_BYTES];
            // SAFETY: a caller-owned buffer with its length, filled with the
            // module's path as ANSI bytes.
            let length = unsafe {
                GetModuleFileNameExA(
                    self.process,
                    *module,
                    name.as_mut_ptr(),
                    MAX_PATH_BYTES as u32,
                )
            } as usize;
            if length == 0 || length >= MAX_PATH_BYTES {
                continue;
            }
            name[length] = 0;
            // SAFETY: the image name is the null-terminated path just read, the
            // base and size are the ones the loader reported, and the module is
            // registered exactly once because `loaded` says so.
            unsafe {
                SymLoadModuleEx(
                    self.process,
                    std::ptr::null_mut(),
                    name.as_ptr(),
                    std::ptr::null(),
                    base,
                    information.SizeOfImage,
                    std::ptr::null(),
                    0,
                );
            }
        }
    }

    /// What `address` is, as far as the loaded symbols say.
    pub(super) fn resolve(&self, address: u64) -> Resolved {
        let mut resolved = Resolved {
            object: self.module_of(address),
            ..Resolved::default()
        };
        let mut buffer = SymbolBuffer::new();
        let mut displacement = 0u64;
        // SAFETY: `buffer` is a `SYMBOL_INFO` followed by the name storage its
        // `MaxNameLen` declares, which is the layout DbgHelp writes into.
        let found = unsafe {
            SymFromAddr(
                self.process,
                address,
                &mut displacement,
                std::ptr::from_mut(&mut buffer.info),
            ) != 0
        };
        if found {
            resolved.symbol = buffer.name();
            resolved.offset = u32::try_from(displacement).unwrap_or(u32::MAX);
        }

        let mut line = IMAGEHLP_LINE64 {
            SizeOfStruct: size_of::<IMAGEHLP_LINE64>() as u32,
            Key: std::ptr::null_mut(),
            LineNumber: 0,
            FileName: std::ptr::null_mut(),
            Address: 0,
        };
        let mut line_displacement = 0u32;
        // SAFETY: `line` is a correctly sized `IMAGEHLP_LINE64`, and the
        // `FileName` it writes points into DbgHelp's own storage, which stays
        // valid until the next call on this session — it is copied below.
        let has_line = unsafe {
            SymGetLineFromAddr64(self.process, address, &mut line_displacement, &mut line) != 0
        };
        if has_line && !line.FileName.is_null() {
            // SAFETY: DbgHelp writes a null-terminated ANSI path here.
            let file = unsafe { CStr::from_ptr(line.FileName.cast()) };
            resolved.file = Some(file.to_string_lossy().into_owned());
            resolved.line = Some(line.LineNumber);
        }
        resolved
    }

    /// The image `address` belongs to, by file name.
    fn module_of(&self, address: u64) -> String {
        // SAFETY: `IMAGEHLP_MODULE64` is a plain-data Win32 struct with no
        // field that must be non-zero before the call sets it, and the one
        // field it reads first is written immediately below.
        let mut module: IMAGEHLP_MODULE64 = unsafe { std::mem::zeroed() };
        module.SizeOfStruct = size_of::<IMAGEHLP_MODULE64>() as u32;
        // SAFETY: `module` is zeroed with its declared size, which is the
        // contract `SymGetModuleInfo64` reads before it writes.
        let found = unsafe { SymGetModuleInfo64(self.process, address, &mut module) != 0 };
        if !found {
            return "[unknown]".to_owned();
        }
        let name = ansi_field(&module.ImageName);
        if name.is_empty() {
            ansi_field(&module.ModuleName)
        } else {
            name
        }
    }
}

impl Drop for Symbols {
    fn drop(&mut self) {
        if self.open {
            // SAFETY: closing the session this value opened, exactly once.
            unsafe {
                SymCleanup(self.process);
            }
            self.open = false;
        }
    }
}

/// A `SYMBOL_INFO` with the name storage it promises.
///
/// DbgHelp writes the name past the end of the struct, up to `MaxNameLen`
/// bytes. The two fields must stay adjacent for that to be in bounds, which is
/// what `repr(C)` and the single constructor here guarantee.
#[repr(C)]
struct SymbolBuffer {
    info: SYMBOL_INFO,
    name: [u8; MAX_SYM_NAME],
}

impl SymbolBuffer {
    fn new() -> Self {
        // SAFETY: `SYMBOL_INFO` is a plain-data Win32 struct with no pointer
        // fields that must be non-null, and every field DbgHelp reads is set
        // immediately below.
        let mut info: SYMBOL_INFO = unsafe { std::mem::zeroed() };
        info.SizeOfStruct = size_of::<SYMBOL_INFO>() as u32;
        info.MaxNameLen = MAX_SYM_NAME as u32;
        Self {
            info,
            name: [0; MAX_SYM_NAME],
        }
    }

    /// The name DbgHelp wrote, as text.
    fn name(&self) -> String {
        let length = self.info.NameLen as usize;
        // The first byte of the name lives in `SYMBOL_INFO::Name`, and the
        // rest in the storage that follows it, so the run is read from there.
        let start = std::ptr::from_ref(&self.info.Name).cast::<u8>();
        let available = MAX_SYM_NAME.saturating_sub(1);
        // SAFETY: `start` points at the first name byte inside this value, and
        // the read is clamped to the storage this struct owns.
        let bytes = unsafe { std::slice::from_raw_parts(start, length.min(available)) };
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// A fixed-size ANSI field as text, stopping at the first null.
fn ansi_field(field: &[i8]) -> String {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    let bytes = field[..end]
        .iter()
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Path::new(&text)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(text)
}

/// The last Win32 error on this thread.
pub(super) fn last_error() -> u32 {
    // SAFETY: reading the calling thread's own error code takes no arguments.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symbol_buffer_declares_the_name_storage_that_follows_it() {
        let buffer = SymbolBuffer::new();
        assert_eq!(buffer.info.MaxNameLen as usize, MAX_SYM_NAME);
        assert_eq!(buffer.info.SizeOfStruct as usize, size_of::<SYMBOL_INFO>());
        assert!(size_of::<SymbolBuffer>() >= size_of::<SYMBOL_INFO>() + MAX_SYM_NAME - 1);
    }

    /// The function the symbol handler is asked to name below.
    ///
    /// `inline(never)` because a symbolizer can only find a function that
    /// survived into the image with an address of its own.
    #[inline(never)]
    fn a_function_this_test_can_look_up() -> u64 {
        std::hint::black_box(7)
    }

    #[test]
    fn the_symbol_handler_names_a_function_in_this_very_process() {
        // SAFETY: the pseudo-handle for this process takes no arguments and
        // needs no release.
        let process = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() };
        let directory = std::env::current_exe().ok();
        let mut symbols = Symbols::open(process, directory.as_deref().and_then(Path::parent))
            .expect("a symbol session over this process");
        symbols.refresh();
        let address = a_function_this_test_can_look_up as *const () as usize as u64;
        let resolved = symbols.resolve(address);
        assert!(
            resolved.symbol.contains("a_function_this_test_can_look_up"),
            "{resolved:?}"
        );
        assert!(resolved.object.ends_with(".exe"), "{resolved:?}");
    }

    #[test]
    fn an_ansi_field_stops_at_its_null_and_keeps_only_the_file_name() {
        let mut field = [0i8; 32];
        for (slot, byte) in field.iter_mut().zip(b"C:\\Windows\\ntdll.dll") {
            *slot = *byte as i8;
        }
        assert_eq!(ansi_field(&field), "ntdll.dll");
    }
}
