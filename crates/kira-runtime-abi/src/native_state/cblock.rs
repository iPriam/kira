//! Portable uniquely owned C-block trees.

use thiserror::Error;

use crate::ForeignPointerWidth;

/// A byte offset inside a portable C-block payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CBlockOffset(u64);

impl CBlockOffset {
    /// Creates an offset from a target-layout byte count.
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Returns the byte count this offset carries.
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

/// A portable uniquely owned C-block tree crossing between engines.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeCBlock {
    bytes: Box<[u8]>,
    children: Vec<NativeCBlockChild>,
}

impl NativeCBlock {
    /// Creates a leaf block owning `bytes`.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: bytes.into_boxed_slice(),
            children: Vec::new(),
        }
    }

    /// Moves `child` under this block at one embedded pointer word.
    pub fn attach(
        &mut self,
        offset: CBlockOffset,
        width: ForeignPointerWidth,
        child: NativeCBlock,
    ) -> Result<(), NativeCBlockError> {
        let width_bytes = u64::from(width.bytes());
        let Some(end) = offset.bytes().checked_add(width_bytes) else {
            return Err(NativeCBlockError::ChildOutsidePayload {
                offset,
                width,
                payload_len: self.bytes.len() as u64,
            });
        };
        if end > self.bytes.len() as u64 {
            return Err(NativeCBlockError::ChildOutsidePayload {
                offset,
                width,
                payload_len: self.bytes.len() as u64,
            });
        }
        let start = offset.bytes() as usize;
        self.bytes[start..end as usize].fill(0);
        self.children.push(NativeCBlockChild {
            offset,
            width,
            block: Box::new(child),
        });
        Ok(())
    }

    /// Returns the payload bytes, with embedded child words cleared.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns owned child blocks in embedded-word order.
    pub fn children(&self) -> &[NativeCBlockChild] {
        &self.children
    }

    /// Splits this tree into its payload and children.
    pub fn into_parts(self) -> (Box<[u8]>, Vec<NativeCBlockChild>) {
        (self.bytes, self.children)
    }
}

/// One child in a [`NativeCBlock`] ownership tree.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeCBlockChild {
    offset: CBlockOffset,
    width: ForeignPointerWidth,
    block: Box<NativeCBlock>,
}

impl NativeCBlockChild {
    /// Returns the embedded pointer's byte offset.
    pub const fn offset(&self) -> CBlockOffset {
        self.offset
    }

    /// Returns the embedded pointer's target width.
    pub const fn width(&self) -> ForeignPointerWidth {
        self.width
    }

    /// Returns the owned child block.
    pub fn block(&self) -> &NativeCBlock {
        &self.block
    }

    /// Consumes this row and returns its owned child block.
    pub fn into_block(self) -> NativeCBlock {
        *self.block
    }
}

/// A malformed portable C-block ownership tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NativeCBlockError {
    /// A child pointer word did not fit in its parent's payload.
    #[error(
        "C-block child at byte {offset:?} with width {width:?} exceeds payload length {payload_len}"
    )]
    ChildOutsidePayload {
        /// The attempted child offset.
        offset: CBlockOffset,
        /// The attempted pointer width.
        width: ForeignPointerWidth,
        /// The parent payload length.
        payload_len: u64,
    },
}
