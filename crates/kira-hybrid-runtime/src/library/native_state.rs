//! Callback-state transport through one loaded native library.

use super::*;

impl NativeLibrary {
    /// Boxes callback state in the loaded native half's process-lifetime store.
    pub fn native_state_create(
        &self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        // SAFETY: every node is allocated and consumed by this same loaded library.
        let node = unsafe { self.encode_state_value(&value)? };
        let mut token = 0;
        // SAFETY: `node` is live and `token` is one writable word.
        let status = unsafe { (self.state_new)(ty.as_word(), node, &mut token) };
        self.check_state_status(status, token)?;
        Ok(NativeStateToken::from_word(token))
    }

    /// Recovers an owned callback-state copy from the loaded native half.
    pub fn native_state_recover(
        &self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        let mut node = std::ptr::null_mut();
        // SAFETY: `node` is one writable pointer slot.
        let status = unsafe { (self.state_recover)(token.as_word(), ty.as_word(), &mut node) };
        self.check_state_status(status, token.as_word())?;
        // SAFETY: success initializes one live node from this library.
        unsafe { self.decode_state_value(node) }
    }

    /// Replaces callback state in the loaded native half.
    pub fn native_state_replace(
        &self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        // SAFETY: every node is allocated and consumed by this same loaded library.
        let node = unsafe { self.encode_state_value(&value)? };
        // SAFETY: `node` is live and consumed by the runtime call.
        let status = unsafe { (self.state_replace)(token.as_word(), ty.as_word(), node) };
        self.check_state_status(status, token.as_word())
    }

    /// Releases callback state in the loaded native half exactly once.
    pub fn native_state_free(&self, token: NativeStateToken) -> Result<(), NativeStateError> {
        // SAFETY: this function pointer accepts any token word and validates it.
        let status = unsafe { (self.state_free)(token.as_word()) };
        self.check_state_status(status, token.as_word())
    }

    /// Builds the library's node tree from a stored value, reading it.
    ///
    /// Read rather than consumed: an aggregate's children are shared, so taking
    /// ownership of one would mean copying the whole subtree just to walk it.
    pub(crate) unsafe fn encode_state_value(
        &self,
        value: &NativeStateValue,
    ) -> Result<StateNode, NativeStateError> {
        Ok(match value {
            // SAFETY: the constructor accepts its scalar by value.
            NativeStateValue::Int(value) => unsafe { (self.state_value_int)(*value) },
            NativeStateValue::Any { type_id, payload } => {
                // SAFETY: the child is allocated by this library and ownership
                // moves into the parent node.
                let child = unsafe { self.encode_state_value(payload)? };
                // SAFETY: the constructor consumes the live child exactly once.
                unsafe { (self.state_value_any)(*type_id, child) }
            }
            // SAFETY: the constructor accepts its opaque word by value.
            NativeStateValue::RawPtr(value) => unsafe { (self.state_value_raw_ptr)(*value) },
            // SAFETY: the constructor accepts its scalar by value.
            NativeStateValue::Float(value) => unsafe { (self.state_value_float)(*value) },
            // SAFETY: the constructor accepts its scalar by value.
            NativeStateValue::Bool(value) => unsafe { (self.state_value_bool)(u8::from(*value)) },
            NativeStateValue::String(value) => {
                let string = self.new_string(value) as *mut c_void;
                // SAFETY: the constructor consumes this live string handle.
                unsafe { (self.state_value_string)(string) }
            }
            NativeStateValue::CBlock(bytes) => {
                // SAFETY: the helper copies this ownership tree into nodes from
                // the loaded library.
                unsafe { self.encode_cblock(bytes)? }
            }
            // SAFETY: the aggregate owns every child this builds.
            NativeStateValue::Struct(values) => unsafe {
                self.encode_aggregate(NativeStateValueTag::STRUCT, 0, values)?
            },
            // SAFETY: the aggregate owns every child this builds.
            NativeStateValue::Array(values) => unsafe {
                self.encode_aggregate(NativeStateValueTag::ARRAY, 0, values)?
            },
            NativeStateValue::Enum { tag, payload } => {
                let values = payload.as_deref().map_or(&[][..], std::slice::from_ref);
                // SAFETY: the aggregate owns every child this builds.
                unsafe { self.encode_aggregate(NativeStateValueTag::ENUM, *tag, values)? }
            }
            // A cell crosses as an opaque handle; the declaring half owns its
            // storage.
            NativeStateValue::Cell(cell) => {
                if cell.is_vm_owned() {
                    // SAFETY: the proxy is a fresh native cell box. The state
                    // node takes one share, and this releases the constructor
                    // share immediately below.
                    let proxy = unsafe { (self.cell_proxy_new)(cell.handle()) };
                    // SAFETY: `proxy` is that fresh box, so the node takes its
                    // own share of a live cell.
                    let node = unsafe { (self.state_value_cell)(proxy) };
                    // SAFETY: this releases the constructor share only; the
                    // node above holds the one that keeps the box alive.
                    unsafe { (self.cell_release.free)(proxy) };
                    node
                } else {
                    // SAFETY: the constructor takes the box by handle and
                    // takes its own share; this node keeps the one it had.
                    unsafe { (self.state_value_cell)(cell.handle()) }
                }
            }
        })
    }

    /// Builds one loaded-library node from a portable C-block tree.
    unsafe fn encode_cblock(&self, block: &NativeCBlock) -> Result<StateNode, NativeStateError> {
        // SAFETY: the constructor copies this live payload and reserves exactly
        // one slot per child below.
        let node = unsafe {
            (self.state_value_cblock)(
                block.bytes().as_ptr(),
                block.bytes().len(),
                block.children().len(),
            )
        };
        if node.is_null() {
            return Err(NativeStateError::MalformedValue);
        }
        for (index, child) in block.children().iter().enumerate() {
            // SAFETY: recursion creates one live child node in this library.
            let child_node = unsafe { self.encode_cblock(child.block())? };
            // SAFETY: parent and child are live, the slot is in range and set
            // once, and the metadata was validated by `NativeCBlock::attach`.
            let status = unsafe {
                (self.state_value_set_cblock_child)(
                    node,
                    index,
                    child.offset().bytes(),
                    child.width().bytes(),
                    child_node,
                )
            };
            if status != NativeStateStatus::OK.0 {
                // SAFETY: both nodes remain owned here after a refused store.
                unsafe {
                    (self.state_value_free)(child_node);
                    (self.state_value_free)(node);
                }
                return Err(NativeStateError::MalformedValue);
            }
        }
        Ok(node)
    }

    unsafe fn encode_aggregate(
        &self,
        tag: NativeStateValueTag,
        enum_tag: u32,
        values: &[NativeStateValue],
    ) -> Result<StateNode, NativeStateError> {
        // SAFETY: constructor takes plain scalar metadata.
        let node = unsafe { (self.state_value_aggregate)(tag.0, enum_tag, values.len()) };
        for (index, value) in values.iter().enumerate() {
            // SAFETY: recursion allocates a child in this same library.
            let child = unsafe { self.encode_state_value(value)? };
            // SAFETY: node and child are live; each in-range slot is set once.
            let status = unsafe { (self.state_value_set_child)(node, index, child) };
            if status != NativeStateStatus::OK.0 {
                // SAFETY: the parent remains live after a refused child store.
                unsafe { (self.state_value_free)(node) };
                return Err(NativeStateError::MalformedValue);
            }
        }
        Ok(node)
    }

    pub(crate) unsafe fn decode_state_value(
        &self,
        node: StateNode,
    ) -> Result<NativeStateValue, NativeStateError> {
        if node.is_null() {
            return Err(NativeStateError::MalformedValue);
        }
        // SAFETY: `node` is live and belongs to this library.
        let tag = NativeStateValueTag(unsafe { (self.state_value_tag)(node) });
        let value = match tag {
            NativeStateValueTag::INT => {
                // SAFETY: tag validation established the node shape.
                NativeStateValue::Int(unsafe { (self.state_value_read_int)(node) })
            }
            NativeStateValueTag::ANY => {
                // SAFETY: tag validation established the node shape.
                let type_id = unsafe { (self.state_value_read_any_type)(node) };
                // SAFETY: the `Any` node has exactly one owned payload child.
                let child = unsafe { (self.state_value_child)(node, 0) };
                // SAFETY: recursion consumes the child. A payload that fails
                // to decode frees this node first: an early return here would
                // otherwise strand it, with every not-yet-decoded sibling
                // behind it.
                let payload = match unsafe { self.decode_state_value(child) } {
                    Ok(payload) => payload,
                    Err(error) => {
                        // SAFETY: `node` is still live and uniquely owned.
                        unsafe { (self.state_value_free)(node) };
                        return Err(error);
                    }
                };
                NativeStateValue::any_of(type_id, payload)
            }
            NativeStateValueTag::RAW_PTR => {
                // SAFETY: tag validation established the node shape.
                NativeStateValue::RawPtr(unsafe { (self.state_value_read_raw_ptr)(node) })
            }
            NativeStateValueTag::CELL => {
                // SAFETY: tag validation established the node shape.
                let handle = unsafe { (self.state_value_read_cell)(node) };
                // Keep the loaded library with the share's release function.
                let release = Arc::clone(&self.cell_release);
                NativeStateValue::Cell(kira_runtime_abi::NativeCell::new(handle, move |handle| {
                    release.release(handle);
                }))
            }
            NativeStateValueTag::FLOAT => {
                // SAFETY: tag validation established the node shape.
                NativeStateValue::Float(unsafe { (self.state_value_read_float)(node) })
            }
            NativeStateValueTag::BOOL => {
                // SAFETY: tag validation established the node shape.
                NativeStateValue::Bool(unsafe { (self.state_value_read_bool)(node) } != 0)
            }
            NativeStateValueTag::STRING => {
                // SAFETY: tag validation established the node shape.
                let handle = unsafe { (self.state_value_read_string)(node) } as StrHandle;
                // SAFETY: the reader returned one owned handle from this library.
                let text = unsafe { self.take_string(handle) }
                    .map_err(|_| NativeStateError::MalformedValue)?;
                NativeStateValue::String(text)
            }
            NativeStateValueTag::C_BLOCK => {
                // SAFETY: tag validation established the node shape and both
                // accessors borrow from this node until it is freed below.
                let len = unsafe { (self.state_value_read_cblock_len)(node) };
                // SAFETY: same live C-block node.
                let data = unsafe { (self.state_value_read_cblock_data)(node) };
                if len != 0 && data.is_null() {
                    // SAFETY: `node` is still live and uniquely owned.
                    unsafe { (self.state_value_free)(node) };
                    return Err(NativeStateError::MalformedValue);
                }
                let bytes = if len == 0 {
                    Vec::new()
                } else {
                    // SAFETY: the C-block accessor promises `len` readable
                    // bytes until this live node is freed.
                    unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
                };
                let mut block = NativeCBlock::new(bytes);
                // SAFETY: C-block nodes report their child count through the
                // generic aggregate-length accessor.
                let child_count = unsafe { (self.state_value_len)(node) };
                for index in 0..child_count {
                    // SAFETY: the metadata accessors accept this live node and
                    // `index` is in the reported range.
                    let offset = unsafe { (self.state_value_cblock_child_offset)(node, index) };
                    // SAFETY: same live node and in-range index.
                    let width = unsafe { (self.state_value_cblock_child_width)(node, index) };
                    let width = match width {
                        4 => ForeignPointerWidth::Bits32,
                        8 => ForeignPointerWidth::Bits64,
                        _ => {
                            // SAFETY: `node` is still live and uniquely owned.
                            unsafe { (self.state_value_free)(node) };
                            return Err(NativeStateError::MalformedValue);
                        }
                    };
                    // SAFETY: child access consumes a fresh owned clone.
                    let child = unsafe { (self.state_value_child)(node, index) };
                    // SAFETY: `child` is that live owned clone and decoding
                    // consumes it exactly once.
                    let child = match unsafe { self.decode_state_value(child) } {
                        Ok(NativeStateValue::CBlock(child)) => child,
                        Ok(_) | Err(_) => {
                            // SAFETY: `node` is still live and uniquely owned.
                            unsafe { (self.state_value_free)(node) };
                            return Err(NativeStateError::MalformedValue);
                        }
                    };
                    if block
                        .attach(CBlockOffset::new(offset), width, child)
                        .is_err()
                    {
                        // SAFETY: `node` is still live and uniquely owned.
                        unsafe { (self.state_value_free)(node) };
                        return Err(NativeStateError::MalformedValue);
                    }
                }
                NativeStateValue::CBlock(block)
            }
            NativeStateValueTag::STRUCT | NativeStateValueTag::ARRAY => {
                // SAFETY: aggregate accessors accept this live node.
                let len = unsafe { (self.state_value_len)(node) };
                let mut values = Vec::with_capacity(len);
                for index in 0..len {
                    // SAFETY: `index < len`; the returned child is owned.
                    let child = unsafe { (self.state_value_child)(node, index) };
                    // SAFETY: recursion consumes that owned child. A child
                    // that fails frees this node before the error returns —
                    // the values decoded so far are dropped with `values`,
                    // and this node would otherwise be skipped by the free
                    // at the function's tail.
                    let value = match unsafe { self.decode_state_value(child) } {
                        Ok(value) => value,
                        Err(error) => {
                            // SAFETY: `node` is still live and uniquely owned.
                            unsafe { (self.state_value_free)(node) };
                            return Err(error);
                        }
                    };
                    values.push(value);
                }
                if tag == NativeStateValueTag::STRUCT {
                    NativeStateValue::struct_of(values)
                } else {
                    NativeStateValue::array_of(values)
                }
            }
            NativeStateValueTag::ENUM => {
                // SAFETY: enum accessors accept this live node.
                let enum_tag = unsafe { (self.state_value_enum_tag)(node) };
                // SAFETY: same live aggregate node.
                let len = unsafe { (self.state_value_len)(node) };
                let payload = if len == 0 {
                    None
                } else if len == 1 {
                    // SAFETY: child zero exists and is returned owned.
                    let child = unsafe { (self.state_value_child)(node, 0) };
                    // SAFETY: recursion consumes the child. A payload that
                    // fails frees this node first, as every other error path
                    // here does.
                    let decoded = unsafe { self.decode_state_value(child) };
                    match decoded {
                        Ok(payload) => Some(payload),
                        Err(error) => {
                            // SAFETY: `node` is still live and uniquely owned.
                            unsafe { (self.state_value_free)(node) };
                            return Err(error);
                        }
                    }
                } else {
                    // SAFETY: `node` is still live and uniquely owned.
                    unsafe { (self.state_value_free)(node) };
                    return Err(NativeStateError::MalformedValue);
                };
                NativeStateValue::enum_of(enum_tag, payload)
            }
            _ => {
                // SAFETY: `node` is still live and uniquely owned.
                unsafe { (self.state_value_free)(node) };
                return Err(NativeStateError::MalformedValue);
            }
        };
        // SAFETY: decoding copied or cloned every value out; release the node.
        unsafe { (self.state_value_free)(node) };
        Ok(value)
    }

    /// Returns the VM word carried by a native callback-state cell proxy.
    pub(crate) fn vm_cell_proxy_handle(&self, handle: u64) -> Option<u64> {
        // SAFETY: the loaded library validates null, inline, and ordinary
        // handles before reading the proxy tag.
        let value = unsafe { (self.cell_proxy_handle)(handle) };
        (value != u64::MAX).then_some(value)
    }

    fn check_state_status(&self, status: u32, token: u64) -> Result<(), NativeStateError> {
        match NativeStateStatus(status) {
            NativeStateStatus::OK => Ok(()),
            NativeStateStatus::NO_HOST => Err(NativeStateError::NoStateHost),
            NativeStateStatus::NULL_TOKEN => Err(NativeStateError::NullToken),
            NativeStateStatus::UNKNOWN_TOKEN => Err(NativeStateError::UnknownToken(token)),
            NativeStateStatus::WRONG_TYPE => Err(NativeStateError::WrongType {
                actual: 0,
                requested: 0,
            }),
            NativeStateStatus::TOKEN_EXHAUSTED => Err(NativeStateError::TokenExhausted),
            _ => Err(NativeStateError::MalformedValue),
        }
    }
}
