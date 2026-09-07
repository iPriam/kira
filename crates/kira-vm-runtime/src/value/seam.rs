//! Value conversion at native and foreign call seams.

use kira_runtime_abi::{
    ForeignArg, ForeignResult, ForeignType, ForeignTypeSpec, NativeArg, NativeResult,
    NativeStateValue,
};

use super::{EnumId, Heap, Value};

impl Heap {
    /// A payload-less enum's variant tag, or `None` when it carries something.
    ///
    /// The one place the seam's enum rule is written down, so the two callers
    /// that need it — lifting a result and lowering an argument — cannot come
    /// to different answers. Asked of the *value* rather than the declaration:
    /// this side holds a heap object, not a type, and a variant carrying
    /// something owned has no one-word form to cross as.
    pub fn enum_seam_tag(&self, id: EnumId) -> Option<i64> {
        match (self.enum_tag(id), self.enum_payload_ref(id)) {
            (Some(tag), None) => i64::try_from(tag).ok(),
            _ => None,
        }
    }
}

impl Heap {
    /// Renders a value as the text `print` emits, consuming what it owns, or
    /// `None` when the value has no pinned rendering.
    ///
    /// Float formatting matches the reference: whole floats print without a
    /// decimal point (`2.0` -> `2`), matching Rust's default `f64` display.
    ///
    /// A struct is the `None` case, and deliberately so: what `print` renders
    /// for a struct is not pinned anywhere in the language corpus, so any text
    /// invented here would be inventing language surface. Analysis rejects
    /// `print(someStruct)` before a program runs; this is the runtime saying
    /// the same thing rather than printing something made up.
    pub fn format_and_consume(&mut self, value: Value) -> Option<String> {
        let rendered = match value {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Str(id) => self.get(id).to_owned(),
            Value::Void => String::new(),
            // A struct and an array are both the `None` case, and for the same
            // reason: neither has a rendering the language corpus pins, so any
            // text invented here would be inventing language surface. Analysis
            // rejects both before a program runs; this is the runtime saying
            // the same thing rather than printing something made up. A `RawPtr`
            // is the `None` case too: an opaque foreign word has no pinned
            // rendering, and `print(RawPtr)` is refused in the frontend.
            // An erased value joins them: `print` of an `Any` is refused in the
            // frontend, and what it would render is not pinned anywhere.
            Value::Struct(_)
            | Value::Array(_)
            | Value::Enum(_)
            | Value::RawPtr(_)
            | Value::Erased(_)
            | Value::NativeState(_)
            | Value::Cell(_)
            | Value::NativeView { .. }
            | Value::NativeSnapshot(_)
            | Value::MainThreadTask(_)
            | Value::Type(_)
            | Value::CBlock(_) => {
                self.drop_value(value);
                return None;
            }
        };
        self.drop_value(value);
        Some(rendered)
    }

    /// Brings a seam argument into this heap as a runtime value.
    ///
    /// The seam's rule is that arguments borrow, so a string is copied in here
    /// rather than aliased: the caller's storage stays the caller's, and the
    /// value this returns is this heap's to drop like any other.
    ///
    /// A handle is the `None` case. Its word names an object in a heap that
    /// outlives one call, and this heap does not have one: each call runs on a
    /// fresh [`Heap`] that is dropped when the call ends, so there is nothing a
    /// handle could denote. Saying so beats minting a value: a wrong answer
    /// about a handle is a wrong answer about *which object*, which is a
    /// use-after-free, not a bad print. The persistent instance is what gives
    /// handles a home; until then the caller reports the refusal by name.
    pub fn lower(&mut self, argument: NativeArg<'_>) -> Option<Value> {
        Some(match argument {
            NativeArg::Void => Value::Void,
            NativeArg::Int(value) => Value::Int(value),
            NativeArg::Float(value) => Value::Float(value),
            NativeArg::Bool(value) => Value::Bool(value),
            NativeArg::Str(text) => Value::Str(self.alloc(text.to_owned())),
            // A struct, an array, or a payload-carrying enum arrives as a value
            // tree and is rebuilt in this heap. The tree belongs to the caller;
            // what lands here is this heap's own copy, dropped like any other.
            NativeArg::Aggregate(tree) => self.from_native_state(tree),
            // A raw pointer is an opaque word at both seams. Callback userdata
            // needs exactly this crossing: the runtime half mints or receives the
            // token and a native callback hands it back unchanged.
            NativeArg::RawPtr(value) => Value::RawPtr(value),
            // A payload-less enum arrives as its variant tag and is rebuilt
            // here: nothing was borrowed and nothing was moved, because the
            // whole value is the number. A tag too large for the VM's own
            // representation is refused rather than truncated — a wrong tag is
            // a wrong *variant*, which is a silently different program.
            NativeArg::Enum(tag) => match u64::try_from(tag) {
                Ok(tag) => Value::Enum(self.alloc_enum(tag, None)),
                Err(_) => return None,
            },
            NativeArg::Handle(_) => return None,
        })
    }

    /// Takes an owned seam result into this heap as a runtime value.
    ///
    /// The seam's rule is that results own, so a returned string is *moved* in
    /// rather than copied: nothing else holds it.
    ///
    /// A handle is the `None` case, for the reason [`Heap::lower`] gives: this
    /// heap has no representation for an object it did not allocate.
    pub fn absorb(&mut self, result: NativeResult) -> Option<Value> {
        Some(match result {
            NativeResult::Void => Value::Void,
            NativeResult::Int(value) => Value::Int(value),
            NativeResult::Float(value) => Value::Float(value),
            NativeResult::Bool(value) => Value::Bool(value),
            NativeResult::Str(text) => Value::Str(self.alloc(text)),
            // The tree was decoded out of what native code handed over, so it
            // is already this side's; rebuilding it moves nothing further.
            NativeResult::Aggregate(tree) => self.from_native_state(&tree),
            // Callback userdata remains one opaque word across the native seam.
            NativeResult::RawPtr(value) => Value::RawPtr(value),
            // Rebuilt from the tag, exactly as in `lower`: there is no owned
            // storage to move in, which is the whole difference from `Str`.
            NativeResult::Enum(tag) => match u64::try_from(tag) {
                Ok(tag) => Value::Enum(self.alloc_enum(tag, None)),
                Err(_) => return None,
            },
            NativeResult::Handle(_) => return None,
        })
    }

    /// Renders a runtime value as a seam result, leaving `value` untouched, or
    /// `None` when the value has no representation at the seam.
    ///
    /// The seam's rule is that results own, so a string is copied out: the
    /// result outlives this heap, and the caller drops `value` itself.
    ///
    /// A struct is the `None` case: [`NativeResult`] has no struct shape, and
    /// the hybrid ABI has no layout for one yet. This says so rather than
    /// substituting some other value — a wrong answer here is a wrong answer
    /// about *ownership*, which is a double free or a leak at the boundary, not
    /// a bad print. The signature split is checked before a hybrid program is
    /// ever built, so a rejected value should never reach here.
    pub fn lift(&mut self, value: Value) -> Option<NativeResult> {
        Some(match value {
            Value::Void => NativeResult::Void,
            Value::Int(value) => NativeResult::Int(value),
            Value::Float(value) => NativeResult::Float(value),
            Value::Bool(value) => NativeResult::Bool(value),
            Value::Str(id) => NativeResult::Str(self.get(id).to_owned()),
            // Callback userdata leaves through the native seam as the same
            // opaque word; neither side dereferences or frees it.
            Value::RawPtr(value) => NativeResult::RawPtr(value),
            // A payload-less enum leaves as its tag alone. Checked on the
            // *value* rather than trusted from the type: this routine sees a
            // heap object, not a declaration, and a variant holding something
            // owned has no one-word form to leave as. The backend already
            // refuses to build such a crossing, so this is the backstop that
            // makes a mistake a refusal instead of a lost payload.
            Value::Enum(id) => match self.enum_seam_tag(id) {
                Some(tag) => NativeResult::Enum(tag),
                // One carrying a payload leaves as a tree instead, like a
                // struct: the tag alone would drop what the variant holds.
                None => NativeResult::Aggregate(self.seam_tree(value)?),
            },
            // A struct and an array leave as a tree of their contents. This is
            // a copy, not a move: the caller still owns `value` and drops it
            // itself, exactly as it does for the string case above.
            Value::Struct(_) | Value::Array(_) => NativeResult::Aggregate(self.seam_tree(value)?),
            Value::Erased(_) => NativeResult::Aggregate(self.seam_tree(value)?),
            // A C block crosses as a tree node carrying its bytes, exactly as
            // a struct's fields do: the receiving engine materializes a block
            // it owns, so the pointer C reads is always inside the engine
            // holding the value.
            Value::CBlock(_) => NativeResult::Aggregate(self.seam_tree(value)?),
            Value::NativeState(_)
            | Value::MainThreadTask(_)
            | Value::Type(_)
            | Value::Cell(_)
            | Value::NativeView { .. }
            | Value::NativeSnapshot(_) => return None,
        })
    }

    /// Copies `value` into a seam tree, leaving the original owned by its heap.
    ///
    /// Copied first because [`Heap::into_native_state`] *consumes* what it
    /// converts — it was built for a value being moved into callback state —
    /// and the seam's rule for a result is that the caller keeps its own. A
    /// shape with no tree form answers `None` rather than a partial tree.
    pub(crate) fn seam_tree(&mut self, value: Value) -> Option<NativeStateValue> {
        let copy = self.copy_value(value);
        self.into_native_state(copy).ok()
    }

    /// Borrows a runtime value as a foreign-call argument of the expected
    /// exact-width type, or `None` when the value cannot cross as that type.
    ///
    /// The borrow is the seam's contract: a `CString` argument borrows the
    /// heap's string bytes for the duration of the one call (the caller keeps
    /// its `String`, and the transient C copy the host makes is freed before the
    /// call returns). Every other supported argument is a `Copy` scalar. A
    /// mismatch returns `None` rather than guessing — analysis has already
    /// checked the signature, so this is a backstop, not the primary check.
    pub fn foreign_arg(&self, expected: ForeignTypeSpec, value: Value) -> Option<ForeignArg<'_>> {
        // An aggregate position has no scalar crossing and is the `None` case
        // here: the frontend refuses an aggregate at the seam, so no signature
        // this runs against holds one.
        let expected = expected.scalar()?;
        Some(match (expected, value) {
            (ForeignType::Void, Value::Void) => ForeignArg::Void,
            (ForeignType::I8, Value::Int(v)) => ForeignArg::I8(v as i8),
            (ForeignType::I16, Value::Int(v)) => ForeignArg::I16(v as i16),
            (ForeignType::I32, Value::Int(v)) => ForeignArg::I32(v as i32),
            (ForeignType::I64, Value::Int(v)) => ForeignArg::I64(v),
            (ForeignType::U8, Value::Int(v)) => ForeignArg::U8(v as u8),
            (ForeignType::U16, Value::Int(v)) => ForeignArg::U16(v as u16),
            (ForeignType::U32, Value::Int(v)) => ForeignArg::U32(v as u32),
            (ForeignType::U64, Value::Int(v)) => ForeignArg::U64(v as u64),
            (ForeignType::Bool, Value::Bool(v)) => ForeignArg::Bool(v),
            (ForeignType::F32, Value::Float(v)) => ForeignArg::F32(v as f32),
            (ForeignType::F64, Value::Float(v)) => ForeignArg::F64(v),
            (ForeignType::RawPtr, Value::RawPtr(w)) => ForeignArg::RawPtr(w),
            // A C block built for this argument — a struct's image or an
            // array's flattened elements — crosses as its payload address. The
            // block sits on the operand stack until the call returns, so the
            // callee reads live storage for exactly the call's duration.
            (ForeignType::RawPtr, Value::CBlock(id)) => ForeignArg::RawPtr(self.cblock_address(id)),
            (ForeignType::CString, Value::Str(id)) => ForeignArg::CString(self.get(id)),
            (ForeignType::CString, Value::CBlock(id)) => {
                ForeignArg::CStringPtr(self.cblock_address(id))
            }
            _ => return None,
        })
    }

    /// Takes an owned foreign-call result into this heap as a runtime value.
    ///
    /// Integer results are stored in the VM's 64-bit `Int`, sign- or
    /// zero-extended by their declared width, exactly as the generated adapter
    /// narrows them. `RawPtr` stays an opaque word. A `CString` result already
    /// carries the callee's bytes rather than its pointer — the seam copied them
    /// while the pointer was good — so it lands on this heap as an ordinary
    /// owned `String` and nothing here ever holds C storage.
    ///
    /// An aggregate result yields `None`: turning C-layout bytes back into a
    /// struct needs the aggregate's member tree, which lives in the module, not
    /// in the heap. The frontend refuses an aggregate at the seam, so no
    /// signature this runs against returns one.
    pub fn absorb_foreign(&mut self, result: ForeignResult) -> Option<Value> {
        Some(match result {
            ForeignResult::Void => Value::Void,
            ForeignResult::I8(v) => Value::Int(i64::from(v)),
            ForeignResult::I16(v) => Value::Int(i64::from(v)),
            ForeignResult::I32(v) => Value::Int(i64::from(v)),
            ForeignResult::I64(v) => Value::Int(v),
            ForeignResult::U8(v) => Value::Int(i64::from(v)),
            ForeignResult::U16(v) => Value::Int(i64::from(v)),
            ForeignResult::U32(v) => Value::Int(i64::from(v)),
            ForeignResult::U64(v) => Value::Int(v as i64),
            ForeignResult::Bool(v) => Value::Bool(v),
            ForeignResult::F32(v) => Value::Float(f64::from(v)),
            ForeignResult::F64(v) => Value::Float(v),
            ForeignResult::RawPtr(w) => Value::RawPtr(w),
            ForeignResult::CString(text) => Value::Str(self.alloc(text)),
            ForeignResult::Aggregate { .. } => return None,
        })
    }
}
