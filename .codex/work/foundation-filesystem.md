# Foundation's filesystem — the behaviour the oracle pins

Every line below was **measured** by running `kira-zig/zig-out/bin/kira` (rebuilt
2026-07-26, resolving its own Foundation at `~/.kira/toolchains/dev/1.7.3`) over
probe programs in `.codex/tmp/fsprobe`, on macOS/APFS. Nothing here was read out
of the reference implementation's sources; it is what the programs printed.

kira-rusty must reproduce it byte for byte on VM, LLVM/native, and hybrid.

## The surface

| Function | Signature |
|---|---|
| `readFile` | `(path: borrow String) -> FileContents` |
| `writeFile` | `(path: borrow String, text: borrow String) -> Bool` |
| `readFileRange` | `(path: borrow String, offset: Int, count: Int) -> [U8]` |
| `writeBytesFile` | `(path: borrow String, bytes: borrow [U8]) -> Bool` |
| `fileExists` / `pathExists` / `isDirectory` | `(path: borrow String) -> Bool` |
| `makeDirectory` / `removePath` | `(path: borrow String) -> Bool` |
| `renamePath` | `(oldPath: borrow String, newPath: borrow String) -> Bool` |
| `fileSize` | `(path: borrow String) -> U64` |
| `listDirectory` | `(path: borrow String) -> DirectoryListing` |

`FileContents` is `{ ok: Bool, text: String, size: U64 }`; `DirectoryListing`
answers `count() -> Int`, `entry(index: Int) -> String`, `close(): Void`.

## Measured behaviour

**Failure is a value.** No operation traps. A missing file, a bad parent, a
directory where a file was wanted — each answers `false`, `0`, or an empty
array.

- `readFile` of a missing path or of a **directory**: `ok=false`, `text=""`,
  `size=0`. Of a real file: `ok=true`, `size` = the file's byte length.
- `readFile`'s **`text` stops at the first NUL byte** while `size` counts the
  whole file: a 3-byte file `h\0i` reads back `ok=true`, `size=3`, `text="h"`,
  `text.count == 1`. The reference reads text as a C string; the byte-level
  `readFileRange` is the binary-safe path.
- `readFileRange(path, offset, count)`: short at end of file (4-byte file,
  `count=10` → 4 elements); `0` elements for an offset past the end, a
  `count <= 0`, a negative offset, a missing file, or a directory. Embedded NULs
  survive.
- `writeFile` / `writeBytesFile`: `true` on success, and both **truncate** an
  existing file (`"abcdef"` then `"xy"` leaves size 2). Writing an empty byte
  array succeeds and leaves size 0. A missing parent directory is `false`.
- `fileExists` is regular-files-only — `false` for a directory. `pathExists` is
  true for any entry. `isDirectory` is directories-only.
- `fileSize` of a missing path is `0`; of a **directory** it is the platform's
  `st_size` (64 on APFS for a fresh empty directory), not 0.
- `makeDirectory` **does not create parents** (`"pdir/a/b"` → `false`) and is
  **idempotent** on a directory that already exists (`true` twice). Over an
  existing *file* it is `false`.
- `renamePath` returns `true` for a file or a directory, **overwrites** an
  existing target, and is `false` when the source does not exist.
- `removePath` is **recursive**: a directory holding files and subdirectories is
  removed whole and answers `true`. Removing a path that is already gone is
  `false`.
- `listDirectory` of a missing path or of a file has `count() == 0`. `entry()`
  out of range — past the end or negative — is the empty string, not a trap.
  `.` and `..` are excluded.
- **Listing order is the platform's directory order, not sorted and not creation
  order.** Creating `ccc`, `aaa`, `bbb`, `mmm` in that order listed `mmm`,
  `bbb`, `ccc`, `aaa`. Both implementations walk `readdir` without reordering,
  so they agree on one host; neither promises an order across hosts.

## Where kira-rusty differs by design

The reference reaches the filesystem through a bundled C library (`NativeLibs/FS`
plus a dynamic-buffer helper) autobound into Foundation. kira-rusty ships no C
helper: the same Foundation surface sits on compiler intrinsics that route
through `HostCapabilities` on the VM and `kira_rt_fs_*` natively, so the portable
VM core still builds for `wasm32-unknown-unknown` and nothing needs a C
toolchain to read a file.
