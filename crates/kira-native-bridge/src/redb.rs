//! The compiler-owned ReDB backend exposed to generated Kira bindings.
//!
//! The project-facing package contains no Rust implementation and no C shim.
//! Its `AutobindProfile.redb` declaration asks Kira to generate the typed
//! `@FFI.Extern` surface below, while this module is linked into the same
//! runtime archive as every native Kira program. ReDB remains the storage
//! engine; these functions only translate Kira's UTF-8 key/value vocabulary to
//! ReDB's byte-slice table API.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::path::Path;

use redb::{Database, ReadableDatabase, TableDefinition, TableError, WriteTransaction};

/// The one physical ReDB table. Logical table names are namespaced into keys,
/// which keeps the generated ABI fixed while still allowing a typed Kira API
/// to address as many logical tables as it needs.
const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kira");

/// A Kira-owned database handle.
pub struct KiraRedbHandle {
    database: Database,
}

/// A live ReDB write transaction.
pub struct KiraRedbWriteTransaction {
    transaction: Option<WriteTransaction>,
}

thread_local! {
    /// The last error on the calling thread. The generated `CString` result
    /// copies this text before the next foreign call can replace it.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Opens or creates a ReDB database at `path`.
///
/// A null result means the operation failed; call `kira_redb_last_error` with
/// the returned handle when one exists, or call `kira_redb_last_error` with a
/// null handle for an open failure.
///
/// # Safety
/// `path` must be null or a valid NUL-terminated UTF-8 C string for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_open(path: *const c_char) -> *mut KiraRedbHandle {
    clear_error();
    // SAFETY: the exported function's contract requires `path` to be null or
    // a valid NUL-terminated UTF-8 string for the duration of this call.
    let path = match unsafe { read_text(path, "database path") } {
        Ok(path) => path,
        Err(error) => {
            set_error(error);
            return std::ptr::null_mut();
        }
    };
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        set_error(format!("cannot create database directory: {error}"));
        return std::ptr::null_mut();
    }

    match Database::create(path) {
        Ok(database) => Box::into_raw(Box::new(KiraRedbHandle { database })),
        Err(error) => {
            set_error(format!("cannot open ReDB database: {error}"));
            std::ptr::null_mut()
        }
    }
}

/// Closes a database handle. Null is a harmless no-op.
///
/// # Safety
/// `database` must be null or a live handle returned by [`kira_redb_open`],
/// closed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_close(database: *mut KiraRedbHandle) {
    clear_error();
    if !database.is_null() {
        // SAFETY: the caller owns this live handle and closes it at most once.
        unsafe { drop(Box::from_raw(database)) };
    }
}

/// Reports whether a database handle is non-null.
///
/// # Safety
/// `database` may be null; a non-null value must be a handle returned by
/// [`kira_redb_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_handle_is_valid(database: *mut KiraRedbHandle) -> bool {
    !database.is_null()
}

/// Inserts or replaces one namespaced key/value pair and commits it.
///
/// Returns zero on success and one on failure. The text of a failure is
/// available through [`kira_redb_last_error`].
///
/// # Safety
/// `database`, `table`, `key`, and `value` must follow the handle and C-string
/// contracts of the other functions in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_put(
    database: *mut KiraRedbHandle,
    table: *const c_char,
    key: *const c_char,
    value: *const c_char,
) -> i32 {
    clear_error();
    // SAFETY: the exported function's contract requires `database` to be a
    // live handle for the duration of this call.
    let database = match unsafe { handle(database) } {
        Ok(database) => database,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `table`.
    let table = match unsafe { read_text(table, "table") } {
        Ok(table) => table,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `key`.
    let key = match unsafe { read_text(key, "key") } {
        Ok(key) => key,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `value`.
    let value = match unsafe { read_text(value, "value") } {
        Ok(value) => value,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    let scoped = scoped_key(table, key);
    match write_put(&database.database, &scoped, value.as_bytes()) {
        Ok(()) => 0,
        Err(error) => {
            set_error(error);
            1
        }
    }
}

/// Reads one namespaced value, returning an empty string when the key is absent
/// or the operation fails. Call [`kira_redb_contains`] or inspect
/// [`kira_redb_last_error`] when the distinction matters.
///
/// # Safety
/// `database`, `table`, and `key` must follow the handle and C-string contracts
/// of the other functions in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_get(
    database: *mut KiraRedbHandle,
    table: *const c_char,
    key: *const c_char,
) -> *const c_char {
    clear_error();
    // SAFETY: the exported function's contract requires `database` to be a
    // live handle for the duration of this call.
    let database = match unsafe { handle(database) } {
        Ok(database) => database,
        Err(error) => {
            set_error(error);
            return std::ptr::null();
        }
    };
    // SAFETY: the exported function's C-string contract applies to `table`.
    let table = match unsafe { read_text(table, "table") } {
        Ok(table) => table,
        Err(error) => {
            set_error(error);
            return std::ptr::null();
        }
    };
    // SAFETY: the exported function's C-string contract applies to `key`.
    let key = match unsafe { read_text(key, "key") } {
        Ok(key) => key,
        Err(error) => {
            set_error(error);
            return std::ptr::null();
        }
    };
    let scoped = scoped_key(table, key);
    match read_value(&database.database, &scoped) {
        Ok(Some(value)) => match CString::new(value) {
            Ok(value) => last_value(value),
            Err(_) => {
                set_error("the stored value contains an interior NUL byte");
                std::ptr::null()
            }
        },
        Ok(None) => std::ptr::null(),
        Err(error) => {
            set_error(error);
            std::ptr::null()
        }
    }
}

/// Tests whether one namespaced key exists.
///
/// # Safety
/// `database`, `table`, and `key` must follow the handle and C-string contracts
/// of the other functions in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_contains(
    database: *mut KiraRedbHandle,
    table: *const c_char,
    key: *const c_char,
) -> bool {
    clear_error();
    // SAFETY: the exported function's contract requires `database` to be a
    // live handle for the duration of this call.
    let database = match unsafe { handle(database) } {
        Ok(database) => database,
        Err(error) => {
            set_error(error);
            return false;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `table`.
    let table = match unsafe { read_text(table, "table") } {
        Ok(table) => table,
        Err(error) => {
            set_error(error);
            return false;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `key`.
    let key = match unsafe { read_text(key, "key") } {
        Ok(key) => key,
        Err(error) => {
            set_error(error);
            return false;
        }
    };
    let scoped = scoped_key(table, key);
    match read_value(&database.database, &scoped) {
        Ok(value) => value.is_some(),
        Err(error) => {
            set_error(error);
            false
        }
    }
}

/// Deletes one namespaced key and commits the change.
///
/// Returns zero on success and one on failure.
///
/// # Safety
/// `database`, `table`, and `key` must follow the handle and C-string contracts
/// of the other functions in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_delete(
    database: *mut KiraRedbHandle,
    table: *const c_char,
    key: *const c_char,
) -> i32 {
    clear_error();
    // SAFETY: the exported function's contract requires `database` to be a
    // live handle for the duration of this call.
    let database = match unsafe { handle(database) } {
        Ok(database) => database,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `table`.
    let table = match unsafe { read_text(table, "table") } {
        Ok(table) => table,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `key`.
    let key = match unsafe { read_text(key, "key") } {
        Ok(key) => key,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    let scoped = scoped_key(table, key);
    match write_delete(&database.database, &scoped) {
        Ok(()) => 0,
        Err(error) => {
            set_error(error);
            1
        }
    }
}

/// Returns the last error on the calling thread, or an empty string.
///
/// The pointer remains valid until the next ReDB call on this thread. The
/// generated `CString` result copies it immediately, so Kira never stores this
/// native pointer.
///
/// # Safety
/// `database` is accepted for a uniform typed surface and may be null; it is
/// not dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_last_error(_database: *mut KiraRedbHandle) -> *const c_char {
    LAST_ERROR.with(|error| {
        error
            .borrow()
            .as_ref()
            .map_or_else(|| c"".as_ptr(), |error| error.as_ptr())
    })
}

/// Begins an explicit ReDB write transaction.
///
/// The transaction is committed with [`kira_redb_write_commit`]. If it is
/// abandoned, [`kira_redb_close`] is not involved; the transaction handle must
/// be committed or passed to the companion abort operation supplied by the
/// Kira wrapper.
///
/// # Safety
/// `database` must be null or a live handle returned by [`kira_redb_open`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_write_begin(
    database: *mut KiraRedbHandle,
) -> *mut KiraRedbWriteTransaction {
    clear_error();
    // SAFETY: the exported function's contract requires `database` to be a
    // live handle for the duration of this call.
    let database = match unsafe { handle(database) } {
        Ok(database) => database,
        Err(error) => {
            set_error(error);
            return std::ptr::null_mut();
        }
    };
    match database.database.begin_write() {
        Ok(transaction) => Box::into_raw(Box::new(KiraRedbWriteTransaction {
            transaction: Some(transaction),
        })),
        Err(error) => {
            set_error(format!("cannot begin ReDB write transaction: {error}"));
            std::ptr::null_mut()
        }
    }
}

/// Reports whether a write-transaction handle is non-null.
///
/// # Safety
/// `transaction` may be null; a non-null value must be a handle returned by
/// [`kira_redb_write_begin`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_write_txn_is_valid(
    transaction: *mut KiraRedbWriteTransaction,
) -> bool {
    !transaction.is_null()
}

/// Writes one namespaced pair inside an explicit transaction.
///
/// Returns zero on success and one on failure.
///
/// # Safety
/// `transaction` must be a live write transaction handle, and the strings must
/// be valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_write_put(
    transaction: *mut KiraRedbWriteTransaction,
    table: *const c_char,
    key: *const c_char,
    value: *const c_char,
) -> i32 {
    clear_error();
    // SAFETY: the exported function's contract requires `transaction` to be a
    // live handle for the duration of this call.
    let transaction = match unsafe { write_handle(transaction) } {
        Ok(transaction) => transaction,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `table`.
    let table = match unsafe { read_text(table, "table") } {
        Ok(table) => table,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `key`.
    let key = match unsafe { read_text(key, "key") } {
        Ok(key) => key,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `value`.
    let value = match unsafe { read_text(value, "value") } {
        Ok(value) => value,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    let scoped = scoped_key(table, key);
    let Some(transaction) = transaction.transaction.as_mut() else {
        set_error("the ReDB write transaction is already finished");
        return 1;
    };
    match transaction.open_table(TABLE) {
        Ok(mut table) => match table.insert(scoped.as_slice(), value.as_bytes()) {
            Ok(_) => 0,
            Err(error) => {
                set_error(format!("cannot write ReDB transaction: {error}"));
                1
            }
        },
        Err(error) => {
            set_error(format!("cannot open ReDB table: {error}"));
            1
        }
    }
}

/// Deletes one namespaced pair inside an explicit transaction.
///
/// Returns zero on success and one on failure.
///
/// # Safety
/// `transaction` must be a live write transaction handle, and the strings must
/// be valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_write_delete(
    transaction: *mut KiraRedbWriteTransaction,
    table: *const c_char,
    key: *const c_char,
) -> i32 {
    clear_error();
    // SAFETY: the exported function's contract requires `transaction` to be a
    // live handle for the duration of this call.
    let transaction = match unsafe { write_handle(transaction) } {
        Ok(transaction) => transaction,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `table`.
    let table = match unsafe { read_text(table, "table") } {
        Ok(table) => table,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    // SAFETY: the exported function's C-string contract applies to `key`.
    let key = match unsafe { read_text(key, "key") } {
        Ok(key) => key,
        Err(error) => {
            set_error(error);
            return 1;
        }
    };
    let scoped = scoped_key(table, key);
    let Some(transaction) = transaction.transaction.as_mut() else {
        set_error("the ReDB write transaction is already finished");
        return 1;
    };
    match transaction.open_table(TABLE) {
        Ok(mut table) => match table.remove(scoped.as_slice()) {
            Ok(_) => 0,
            Err(error) => {
                set_error(format!("cannot delete from ReDB transaction: {error}"));
                1
            }
        },
        Err(error) => {
            set_error(format!("cannot open ReDB table: {error}"));
            1
        }
    }
}

/// Commits an explicit ReDB write transaction and releases its handle.
///
/// Returns zero on success and one on failure.
///
/// # Safety
/// `transaction` must be null or a live handle returned by
/// [`kira_redb_write_begin`], committed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_write_commit(transaction: *mut KiraRedbWriteTransaction) -> i32 {
    clear_error();
    if transaction.is_null() {
        set_error("the ReDB write transaction handle is null");
        return 1;
    }
    // SAFETY: the caller owns this live transaction handle and commits it at
    // most once.
    let mut transaction = unsafe { Box::from_raw(transaction) };
    let Some(transaction) = transaction.transaction.take() else {
        set_error("the ReDB write transaction is already finished");
        return 1;
    };
    match transaction.commit() {
        Ok(()) => 0,
        Err(error) => {
            set_error(format!("cannot commit ReDB transaction: {error}"));
            1
        }
    }
}

/// Aborts and releases an explicit ReDB write transaction.
///
/// # Safety
/// `transaction` must be null or a live handle returned by
/// [`kira_redb_write_begin`], aborted at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_redb_write_abort(transaction: *mut KiraRedbWriteTransaction) {
    clear_error();
    if !transaction.is_null() {
        // SAFETY: the caller owns this live transaction handle and aborts it at
        // most once. Dropping a live WriteTransaction rolls it back.
        unsafe { drop(Box::from_raw(transaction)) };
    }
}

/// Validates and returns a database handle reference.
unsafe fn handle<'a>(database: *mut KiraRedbHandle) -> Result<&'a KiraRedbHandle, String> {
    if database.is_null() {
        return Err("the ReDB database handle is null".to_owned());
    }
    // SAFETY: the caller promises a live handle; all exported entry points keep
    // it borrowed for the duration of the operation.
    Ok(unsafe { &*database })
}

/// Validates and returns a write transaction reference.
unsafe fn write_handle<'a>(
    transaction: *mut KiraRedbWriteTransaction,
) -> Result<&'a mut KiraRedbWriteTransaction, String> {
    if transaction.is_null() {
        return Err("the ReDB write transaction handle is null".to_owned());
    }
    // SAFETY: the caller promises a live transaction handle and serializes
    // access to it through the Kira value holding the pointer.
    Ok(unsafe { &mut *transaction })
}

/// Reads one UTF-8 C string, accepting null as the empty string.
unsafe fn read_text<'a>(value: *const c_char, name: &str) -> Result<&'a str, String> {
    if value.is_null() {
        return Ok("");
    }
    // SAFETY: the generated adapter guarantees a valid NUL-terminated C string
    // for every `CString` argument.
    let bytes = unsafe { CStr::from_ptr(value) }.to_bytes();
    std::str::from_utf8(bytes).map_err(|_| format!("ReDB {name} is not valid UTF-8"))
}

/// Prefixes a logical table name into the one physical byte table.
fn scoped_key(table: &str, key: &str) -> Vec<u8> {
    let mut scoped = Vec::with_capacity(table.len() + key.len() + 1);
    scoped.extend_from_slice(table.as_bytes());
    scoped.push(0);
    scoped.extend_from_slice(key.as_bytes());
    scoped
}

/// Performs one committed write transaction.
fn write_put(database: &Database, key: &[u8], value: &[u8]) -> Result<(), String> {
    let transaction = database
        .begin_write()
        .map_err(|error| format!("cannot begin ReDB write transaction: {error}"))?;
    {
        let mut table = transaction
            .open_table(TABLE)
            .map_err(|error| format!("cannot open ReDB table: {error}"))?;
        table
            .insert(key, value)
            .map_err(|error| format!("cannot write ReDB value: {error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("cannot commit ReDB transaction: {error}"))
}

/// Performs one committed delete transaction.
fn write_delete(database: &Database, key: &[u8]) -> Result<(), String> {
    let transaction = database
        .begin_write()
        .map_err(|error| format!("cannot begin ReDB write transaction: {error}"))?;
    {
        let mut table = transaction
            .open_table(TABLE)
            .map_err(|error| format!("cannot open ReDB table: {error}"))?;
        table
            .remove(key)
            .map_err(|error| format!("cannot delete ReDB value: {error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("cannot commit ReDB transaction: {error}"))
}

/// Performs one read transaction and copies the value before it closes.
fn read_value(database: &Database, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let transaction = database
        .begin_read()
        .map_err(|error| format!("cannot begin ReDB read transaction: {error}"))?;
    let table = match transaction.open_table(TABLE) {
        Ok(table) => table,
        Err(TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(error) => return Err(format!("cannot open ReDB table: {error}")),
    };
    let value = table
        .get(key)
        .map_err(|error| format!("cannot read ReDB value: {error}"))?
        .map(|value| value.value().to_vec());
    Ok(value)
}

/// Clears the calling thread's error.
fn clear_error() {
    LAST_ERROR.with(|error| *error.borrow_mut() = None);
}

/// Stores an error for the next generated `CString` read.
fn set_error(error: impl AsRef<str>) {
    let text = error.as_ref().replace('\0', "�");
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = CString::new(text).ok();
    });
}

/// Stores a successful C string in the same thread-local slot used by errors.
fn last_value(value: CString) -> *const c_char {
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = Some(value);
        slot.borrow()
            .as_ref()
            .map_or_else(|| c"".as_ptr(), |value| value.as_ptr())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn string(value: &str) -> CString {
        CString::new(value).expect("test strings contain no NUL")
    }

    fn read(value: *const c_char) -> String {
        assert!(!value.is_null(), "ReDB returned a null string");
        // SAFETY: the backend returns a live thread-local NUL-terminated
        // string until the next ReDB call on this test thread.
        unsafe { CStr::from_ptr(value) }
            .to_str()
            .expect("ReDB test value is UTF-8")
            .to_owned()
    }

    #[test]
    fn crud_commit_and_abort_follow_the_typed_surface_contract() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("kira-redb-{nonce}"));
        fs::create_dir_all(&directory).expect("create ReDB test directory");
        let path = string(&directory.join("database.redb").to_string_lossy());
        let table = string("records");
        let key = string("ada");
        let value = string("Ada");

        // SAFETY: every pointer below either comes from a CString kept alive
        // for the call or from a live handle returned by this module. Each
        // owned handle is released exactly once.
        unsafe {
            let database = kira_redb_open(path.as_ptr());
            assert!(kira_redb_handle_is_valid(database));
            assert!(!kira_redb_contains(database, table.as_ptr(), key.as_ptr()));
            assert_eq!(read(kira_redb_last_error(database)), "");
            assert_eq!(
                kira_redb_put(database, table.as_ptr(), key.as_ptr(), value.as_ptr()),
                0
            );
            assert!(kira_redb_contains(database, table.as_ptr(), key.as_ptr()));
            assert_eq!(
                read(kira_redb_get(database, table.as_ptr(), key.as_ptr())),
                "Ada"
            );

            let transaction = kira_redb_write_begin(database);
            assert!(kira_redb_write_txn_is_valid(transaction));
            let grace = string("grace");
            let value = string("Grace");
            assert_eq!(
                kira_redb_write_put(transaction, table.as_ptr(), grace.as_ptr(), value.as_ptr()),
                0
            );
            assert_eq!(
                kira_redb_write_delete(transaction, table.as_ptr(), key.as_ptr()),
                0
            );
            kira_redb_write_abort(transaction);
            assert!(kira_redb_contains(database, table.as_ptr(), key.as_ptr()));
            assert!(!kira_redb_contains(
                database,
                table.as_ptr(),
                grace.as_ptr()
            ));

            let transaction = kira_redb_write_begin(database);
            assert!(kira_redb_write_txn_is_valid(transaction));
            assert_eq!(
                kira_redb_write_put(transaction, table.as_ptr(), grace.as_ptr(), value.as_ptr()),
                0
            );
            assert_eq!(kira_redb_write_commit(transaction), 0);
            assert!(kira_redb_contains(database, table.as_ptr(), grace.as_ptr()));

            assert_eq!(kira_redb_delete(database, table.as_ptr(), key.as_ptr()), 0);
            assert!(!kira_redb_contains(database, table.as_ptr(), key.as_ptr()));
            kira_redb_close(database);
        }

        fs::remove_dir_all(directory).expect("remove ReDB test directory");
    }
}
