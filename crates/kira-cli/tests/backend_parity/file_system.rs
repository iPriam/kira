//! Foundation's filesystem on every backend.
//!
//! One filesystem, reached two ways: the VM describes each operation and hands
//! it to its host, while native code calls `kira_rt_fs_*`. Both go through one
//! implementation in `kira-runtime-abi`, and these cases are what turns that
//! into a result rather than a claim — including the hybrid case, where the two
//! engines run in one process and have to see each other's writes.
//!
//! The answers themselves — a short read at end of file, a `text` that stops at
//! a NUL while `size` does not, a recursive `removePath`, a `makeDirectory` that
//! refuses to create parents — are the reference implementation's, measured by
//! differential run and recorded in `.codex/work/foundation-filesystem.md`.

use crate::assert_parity_on_disk;

/// Writing, reading back, and measuring one file.
#[test]
fn a_file_round_trips_on_every_backend() {
    let out = assert_parity_on_disk(
        r#"
import Foundation

@Main
function main() {
    removePath("round")
    print(makeDirectory("round"))
    print(writeFile("round/a.txt", "hello"))
    let contents = readFile("round/a.txt")
    print(contents.ok)
    print(contents.text)
    print(contents.size)
    print(fileSize("round/a.txt"))
    print(fileExists("round/a.txt"))
    print(pathExists("round/a.txt"))
    print(isDirectory("round/a.txt"))
    print(removePath("round"))
    return
}
"#,
    );
    assert_eq!(
        out,
        "true\ntrue\ntrue\nhello\n5\n5\ntrue\ntrue\nfalse\ntrue\n"
    );
}

/// A missing path answers rather than trapping, on every backend.
#[test]
fn a_missing_path_answers_the_same_on_every_backend() {
    let out = assert_parity_on_disk(
        r#"
import Foundation

@Main
function main() {
    let contents = readFile("nothing-here.txt")
    print(contents.ok)
    print(contents.text)
    print(contents.size)
    print(fileSize("nothing-here.txt"))
    print(fileExists("nothing-here.txt"))
    print(pathExists("nothing-here.txt"))
    print(isDirectory("nothing-here.txt"))
    print(removePath("nothing-here.txt"))
    print(readFileRange("nothing-here.txt", 0, 4).count)
    print(listDirectory("nothing-here.txt").count())
    return
}
"#,
    );
    assert_eq!(out, "false\n\n0\n0\nfalse\nfalse\nfalse\nfalse\n0\n0\n");
}

/// The byte path is binary-safe and short at end of file, and `text` is not:
/// it stops at the first NUL while `size` still counts the whole file.
#[test]
fn bytes_cross_intact_and_text_stops_at_a_nul_on_every_backend() {
    let out = assert_parity_on_disk(
        r#"
import Foundation

@Main
function main() {
    var bytes: [U8] = [104, 105, 0, 122]
    print(writeBytesFile("bytes.bin", bytes))
    let whole = readFileRange("bytes.bin", 0, 10)
    print(whole.count)
    var index = 0
    while index < whole.count {
        print(Int(whole[index]))
        index = index + 1
    }
    print(readFileRange("bytes.bin", 2, 2).count)
    print(readFileRange("bytes.bin", 99, 4).count)
    print(readFileRange("bytes.bin", 0, 0).count)
    print(readFileRange("bytes.bin", 0 - 1, 4).count)
    let text = readFile("bytes.bin")
    print(text.size)
    print(text.text.count)
    var empty: [U8] = []
    print(writeBytesFile("empty.bin", empty))
    print(fileSize("empty.bin"))
    print(removePath("bytes.bin"))
    print(removePath("empty.bin"))
    return
}
"#,
    );
    assert_eq!(
        out,
        "true\n4\n104\n105\n0\n122\n2\n0\n0\n0\n4\n2\ntrue\n0\ntrue\ntrue\n"
    );
}

/// Directory operations: idempotent creation, no parents, recursive removal,
/// and a listing that holds exactly the entries and answers out of range.
#[test]
fn directory_operations_agree_on_every_backend() {
    let out = assert_parity_on_disk(
        r#"
import Foundation

@Main
function main() {
    removePath("tree")
    print(makeDirectory("tree"))
    print(makeDirectory("tree"))
    print(isDirectory("tree"))
    print(fileExists("tree"))
    print(makeDirectory("tree/a/b"))
    print(writeFile("tree/one.txt", "1"))
    print(makeDirectory("tree/inner"))
    print(writeFile("tree/inner/two.txt", "2"))
    let listing = listDirectory("tree")
    print(listing.count())
    print(listing.entry(99))
    print(listing.entry(0 - 1))
    listing.close()
    print(renamePath("tree/one.txt", "tree/renamed.txt"))
    print(pathExists("tree/one.txt"))
    print(pathExists("tree/renamed.txt"))
    print(renamePath("tree/missing.txt", "tree/other.txt"))
    print(removePath("tree"))
    print(pathExists("tree"))
    return
}
"#,
    );
    assert_eq!(
        out,
        "true\ntrue\ntrue\nfalse\nfalse\ntrue\ntrue\ntrue\n2\n\n\ntrue\nfalse\ntrue\nfalse\ntrue\nfalse\n"
    );
}

/// The hybrid case the other two backends cannot reach: a `@Native` function
/// writes through `kira_rt_fs_*` while a `@Runtime` one reads through the VM
/// host, in one process, and each sees what the other did.
///
/// On `vm` and `llvm` the annotations change nothing — both put every function
/// on one engine — so agreeing with them is exactly the statement that the
/// boundary moved where the code ran and nothing else.
#[test]
fn the_two_engines_share_one_filesystem_on_every_backend() {
    let out = assert_parity_on_disk(
        r#"
import Foundation

@Native
function nativeWrite(path: borrow String, text: borrow String) -> Bool {
    return fsWriteText(path, text)
}

@Native
function nativeRead(path: borrow String) -> String {
    return fsReadText(path)
}

@Native
function nativeSize(path: borrow String) -> U64 {
    return fsFileSize(path)
}

@Native
function nativeRemove(path: borrow String) -> Bool {
    return fsRemovePath(path)
}

@Main
function main() {
    nativeRemove("seam")
    print(makeDirectory("seam"))
    print(nativeWrite("seam/from-native.txt", "written natively"))
    let read = readFile("seam/from-native.txt")
    print(read.ok)
    print(read.text)
    print(writeFile("seam/from-runtime.txt", "written by the vm"))
    print(nativeRead("seam/from-runtime.txt"))
    print(nativeSize("seam/from-runtime.txt"))
    print(listDirectory("seam").count())
    print(nativeRemove("seam"))
    print(pathExists("seam"))
    return
}
"#,
    );
    assert_eq!(
        out,
        "true\ntrue\ntrue\nwritten natively\ntrue\nwritten by the vm\n17\n2\ntrue\nfalse\n"
    );
}
