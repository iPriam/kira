use super::*;

impl TypeTable {
    /// The collision-free runtime identity of a callback-state value type.
    pub fn native_state_type_id(&self, ty: Type) -> Option<NativeStateTypeId> {
        let (tag, payload) = match ty {
            Type::Int(spelling) => (1_u64, int_code(spelling)),
            Type::Float(spelling) => (2, float_code(spelling)),
            Type::Bool => (3, 0),
            Type::String => (4, 0),
            Type::Number => (11, 0),
            // Table indices are compilation-local. A live VM keeps callback
            // state across a rebuild, so using a struct/array/enum index here
            // would make an unrelated declaration invalidate every state
            // token after it. Fingerprinting the declaration shape keeps the
            // type id stable while still refusing a changed state schema.
            Type::Struct(_) => (5, self.native_state_fingerprint(ty)),
            Type::Array(_) => (6, self.native_state_fingerprint(ty)),
            Type::Enum(_) => (7, self.native_state_fingerprint(ty)),
            // The same runtime word a `RawPtr` is, so the same identity: what
            // it points at is a compile-time fact, not a runtime one.
            Type::RawPtr | Type::ForeignPtr(_) => (8, 0),
            // A capture cell is one box, and what recovery has to agree about
            // is what the box holds: two cells of different inner types are
            // different state schemas exactly as two structs are.
            Type::Cell(_) => (9, self.native_state_fingerprint(ty)),
            // A distinct type is its own schema, and the name is what a
            // rebuild can still agree about: the row index is
            // compilation-local, exactly as a struct index is, so the
            // fingerprint carries the name and the representation instead.
            Type::Distinct(_) => (10, self.native_state_fingerprint(ty)),
            // `Any` has no identity to give: the whole point of the type is
            // that the value inside it kept its own and this one has none, so
            // there is nothing for a recovery to check against.
            // A C block is seam-local storage; state that kept one across a
            // rebuild would keep a pointer into a program that no longer
            // exists, so it has no recovery identity either.
            // A runtime type descriptor names a row of *this* build's
            // descriptor table, so state that kept one across a rebuild would
            // name a type the new program never described.
            Type::Void
            | Type::Error
            | Type::CString
            | Type::CBlock
            | Type::NativeState(_)
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::RuntimeType
            | Type::Any => {
                return None;
            }
        };
        const PAYLOAD_MASK: u64 = 0x00ff_ffff_ffff_ffff;
        Some(NativeStateTypeId::new(
            (tag << 56) | (payload & PAYLOAD_MASK),
        ))
    }

    /// Stable shape identity for an aggregate callback-state type.
    ///
    /// The recursive walk deliberately uses declaration names and field
    /// shapes, never the table's local ids. An unrelated struct added before a
    /// state-bearing struct therefore leaves the token recoverable, while a
    /// field insertion, removal, or type change produces a different id and
    /// is rejected at the state boundary instead of trapping later on a bad
    /// path.
    fn native_state_fingerprint(&self, ty: Type) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut visiting = HashSet::new();
        self.mix_native_state_type(&mut hash, ty, &mut visiting);
        if hash == 0 { 1 } else { hash }
    }

    fn mix_native_state_type(&self, hash: &mut u64, ty: Type, visiting: &mut HashSet<(u8, u32)>) {
        match ty {
            Type::Int(spelling) => {
                mix_native_state_bytes(hash, b"int");
                mix_native_state_u64(hash, int_code(spelling));
            }
            Type::Float(spelling) => {
                mix_native_state_bytes(hash, b"float");
                mix_native_state_u64(hash, float_code(spelling));
            }
            Type::Bool => mix_native_state_bytes(hash, b"bool"),
            Type::String => mix_native_state_bytes(hash, b"string"),
            Type::Number => mix_native_state_bytes(hash, b"number"),
            Type::RawPtr | Type::ForeignPtr(_) => mix_native_state_bytes(hash, b"raw-ptr"),
            // Never reached: `native_state_type_id` refuses a descriptor before
            // the walk starts, because the row it names belongs to one build.
            Type::RuntimeType => mix_native_state_bytes(hash, b"type"),
            // Name and representation, and no id: a distinct type is a schema
            // an author wrote down, and the row it sits in is not.
            Type::Distinct(id) => {
                mix_native_state_bytes(hash, b"distinct");
                match self.distincts.get(id) {
                    Some(def) => {
                        // The package-qualified identity, so two packages'
                        // same-named distincts never fingerprint alike.
                        mix_native_state_bytes(hash, self.identity_key(ty).as_bytes());
                        self.mix_native_state_type(hash, def.representation, visiting);
                    }
                    None => mix_native_state_bytes(hash, b"missing-distinct"),
                }
            }
            Type::Struct(id) => {
                let Some(def) = self.structs.get(id) else {
                    mix_native_state_bytes(hash, b"missing-struct");
                    return;
                };
                mix_native_state_bytes(hash, b"struct");
                // The package-qualified identity, generic arguments included,
                // so two packages' same-named structs never fingerprint alike.
                mix_native_state_bytes(hash, self.identity_key(ty).as_bytes());
                // A function type's representation is named for its signature,
                // and the signature is the whole of its identity: the fields
                // are the captures this compilation happened to find, so
                // walking them would give a library and the application that
                // links it two different ids for one type — and the recovery
                // would be refused for a program that is correct.
                if self.structs.origin(id) == StructOrigin::FunctionType {
                    return;
                }
                if !visiting.insert((5, id.index())) {
                    mix_native_state_bytes(hash, b"recursive");
                    return;
                }
                for field in &def.fields {
                    mix_native_state_bytes(hash, field.name.as_bytes());
                    mix_native_state_u64(hash, u64::from(field.mutable as u8));
                    self.mix_native_state_type(hash, field.ty, visiting);
                }
                visiting.remove(&(5, id.index()));
            }
            Type::Array(id) => {
                mix_native_state_bytes(hash, b"array");
                if let Some(element) = self.arrays.element(id) {
                    self.mix_native_state_type(hash, element, visiting);
                } else {
                    mix_native_state_bytes(hash, b"missing-array");
                }
            }
            Type::Enum(id) => {
                let Some(def) = self.enums.get(id) else {
                    mix_native_state_bytes(hash, b"missing-enum");
                    return;
                };
                mix_native_state_bytes(hash, b"enum");
                mix_native_state_bytes(hash, self.identity_key(ty).as_bytes());
                if !visiting.insert((7, id.index())) {
                    mix_native_state_bytes(hash, b"recursive");
                    return;
                }
                for variant in &def.variants {
                    mix_native_state_bytes(hash, variant.name.as_bytes());
                    match variant.payload {
                        Some(payload) => {
                            mix_native_state_bytes(hash, b"payload");
                            self.mix_native_state_type(hash, payload, visiting);
                        }
                        None => mix_native_state_bytes(hash, b"no-payload"),
                    }
                }
                visiting.remove(&(7, id.index()));
            }
            Type::Cell(id) => {
                mix_native_state_bytes(hash, b"cell");
                if !visiting.insert((9, id.index())) {
                    mix_native_state_bytes(hash, b"recursive");
                    return;
                }
                match self.cells.inner(id) {
                    Some(inner) => self.mix_native_state_type(hash, inner, visiting),
                    None => mix_native_state_bytes(hash, b"missing-cell"),
                }
                visiting.remove(&(9, id.index()));
            }
            // A nested state is part of the outer schema by the schema of what
            // it owns, not by its compilation-local NativeState table row.
            Type::NativeState(id) => {
                mix_native_state_bytes(hash, b"native-state");
                match self.native_states.target(id) {
                    Some(target) => self.mix_native_state_type(hash, target, visiting),
                    None => mix_native_state_bytes(hash, b"missing-native-state"),
                }
            }
            // These shapes are refused before this method is called. Keeping a
            // marker here makes the fingerprint total if an error node leaks
            // through a diagnostic-preserving analysis.
            Type::Void
            | Type::Error
            | Type::CString
            | Type::CBlock
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::Any => mix_native_state_bytes(hash, b"unsupported"),
        }
    }
}
