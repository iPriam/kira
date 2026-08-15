//! Owned `ffi_type` graphs and prepared CIFs.

use kira_runtime_abi::{
    ForeignAggregates, ForeignArrayElement, ForeignMember, ForeignSignature, ForeignType,
    ForeignTypeSpec,
};

use crate::LibffiError;
use crate::raw::{FFI_TYPE_STRUCT, RawFfiCif, RawFfiType, RawLibffi};

/// The libffi type graph for one program's aggregate table and signature.
pub(crate) struct PreparedCif {
    pub(crate) cif: RawFfiCif,
    pub(crate) graph: FfiTypeGraph,
    pub(crate) _argument_types: Box<[*mut RawFfiType]>,
    pub(crate) _result_type: *mut RawFfiType,
}

impl PreparedCif {
    pub(crate) fn new(
        api: &RawLibffi,
        signature: &ForeignSignature,
        aggregates: &ForeignAggregates,
    ) -> Result<Self, LibffiError> {
        let graph = FfiTypeGraph::new(api, aggregates)?;
        let argument_types: Box<[*mut RawFfiType]> = signature
            .parameters()
            .iter()
            .map(|spec| graph.type_for(*spec))
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let result_type = graph.type_for(signature.result())?;
        let mut cif = RawFfiCif {
            abi: 0,
            nargs: signature.parameters().len() as u32,
            arg_types: if argument_types.is_empty() {
                std::ptr::null_mut()
            } else {
                argument_types.as_ptr() as *mut *mut RawFfiType
            },
            result_type,
            bytes: 0,
            flags: 0,
        };
        let status = unsafe {
            // SAFETY: `cif`, all type pointers, and the terminated graph remain
            // owned by the returned `PreparedCif` for every call using it.
            (api.prep_cif)(
                &mut cif,
                (api.default_abi)(),
                argument_types.len() as u32,
                result_type,
                cif.arg_types,
            )
        };
        if status != 0 {
            return Err(LibffiError::Prepare { status });
        }
        Ok(Self {
            cif,
            graph,
            _argument_types: argument_types,
            _result_type: result_type,
        })
    }

    pub(crate) fn layout(&self, spec: ForeignTypeSpec) -> Result<(usize, usize), LibffiError> {
        self.graph.layout(spec)
    }
}

/// A graph whose nodes and element arrays stay at stable addresses until its
/// prepared CIF is dropped.
pub(crate) struct FfiTypeGraph {
    aggregates: Vec<*mut RawFfiType>,
    /// One node per aggregate, allocated once so that a node's address is
    /// already final when a later node points at it.
    nodes: Box<[RawFfiType]>,
    element_lists: Vec<Box<[*mut RawFfiType]>>,
    layouts: Vec<(usize, usize)>,
    api_types: ApiTypes,
}

#[derive(Clone, Copy)]
struct ApiTypes {
    void: *mut RawFfiType,
    uint8: *mut RawFfiType,
    sint8: *mut RawFfiType,
    uint16: *mut RawFfiType,
    sint16: *mut RawFfiType,
    uint32: *mut RawFfiType,
    sint32: *mut RawFfiType,
    uint64: *mut RawFfiType,
    sint64: *mut RawFfiType,
    float: *mut RawFfiType,
    double: *mut RawFfiType,
    pointer: *mut RawFfiType,
}

impl FfiTypeGraph {
    fn new(api: &RawLibffi, aggregates: &ForeignAggregates) -> Result<Self, LibffiError> {
        let mut graph = Self {
            aggregates: Vec::with_capacity(aggregates.len()),
            nodes: (0..aggregates.len())
                .map(|_| RawFfiType {
                    size: 0,
                    alignment: 0,
                    kind: FFI_TYPE_STRUCT,
                    elements: std::ptr::null_mut(),
                })
                .collect(),
            element_lists: Vec::with_capacity(aggregates.len()),
            layouts: aggregates
                .layouts(kira_runtime_abi::ForeignPointerWidth::HOST)?
                .into_iter()
                .map(|layout| (layout.size as usize, layout.align as usize))
                .collect(),
            api_types: ApiTypes {
                void: api.type_void,
                uint8: api.type_uint8,
                sint8: api.type_sint8,
                uint16: api.type_uint16,
                sint16: api.type_sint16,
                uint32: api.type_uint32,
                sint32: api.type_sint32,
                uint64: api.type_uint64,
                sint64: api.type_sint64,
                float: api.type_float,
                double: api.type_double,
                pointer: api.type_pointer,
            },
        };
        for (index, aggregate) in aggregates.iter().enumerate() {
            let mut elements = Vec::new();
            for member in aggregate.members() {
                match member {
                    ForeignMember::Scalar(ty) => elements.push(graph.scalar(*ty)),
                    ForeignMember::Aggregate(id) => {
                        elements.push(
                            *graph
                                .aggregates
                                .get(id.0 as usize)
                                .ok_or(LibffiError::UnknownAggregate(id.0))?,
                        );
                    }
                    ForeignMember::Array { element, count } => {
                        for _ in 0..*count {
                            elements.push(match element {
                                ForeignArrayElement::Scalar(ty) => graph.scalar(*ty),
                                ForeignArrayElement::Aggregate(id) => *graph
                                    .aggregates
                                    .get(id.0 as usize)
                                    .ok_or(LibffiError::UnknownAggregate(id.0))?,
                            });
                        }
                    }
                }
            }
            if elements.is_empty() {
                elements.push(graph.api_types.uint8);
            }
            elements.push(std::ptr::null_mut());
            let mut elements = elements.into_boxed_slice();
            let element_pointer = elements.as_mut_ptr();
            graph.element_lists.push(elements);
            let node = graph
                .nodes
                .get_mut(index)
                .ok_or(LibffiError::UnknownAggregate(index as u32))?;
            node.elements = element_pointer;
            let pointer = node as *mut RawFfiType;
            graph.aggregates.push(pointer);
        }
        Ok(graph)
    }

    fn scalar(&self, ty: ForeignType) -> *mut RawFfiType {
        match ty {
            ForeignType::Void => self.api_types.void,
            ForeignType::I8 => self.api_types.sint8,
            ForeignType::I16 => self.api_types.sint16,
            ForeignType::I32 => self.api_types.sint32,
            ForeignType::I64 => self.api_types.sint64,
            ForeignType::U8 | ForeignType::Bool => self.api_types.uint8,
            ForeignType::U16 => self.api_types.uint16,
            ForeignType::U32 => self.api_types.uint32,
            ForeignType::U64 => self.api_types.uint64,
            ForeignType::F32 => self.api_types.float,
            ForeignType::F64 => self.api_types.double,
            ForeignType::RawPtr | ForeignType::CString => self.api_types.pointer,
        }
    }

    fn type_for(&self, spec: ForeignTypeSpec) -> Result<*mut RawFfiType, LibffiError> {
        Ok(match spec {
            ForeignTypeSpec::Scalar(ty) => self.scalar(ty),
            ForeignTypeSpec::Aggregate(id) => *self
                .aggregates
                .get(id.0 as usize)
                .ok_or(LibffiError::UnknownAggregate(id.0))?,
        })
    }

    fn layout(&self, spec: ForeignTypeSpec) -> Result<(usize, usize), LibffiError> {
        Ok(match spec {
            ForeignTypeSpec::Scalar(ty) => {
                let layout = kira_runtime_abi::scalar_layout(
                    ty,
                    kira_runtime_abi::ForeignPointerWidth::HOST,
                );
                (layout.size as usize, layout.align as usize)
            }
            ForeignTypeSpec::Aggregate(id) => *self
                .layouts
                .get(id.0 as usize)
                .ok_or(LibffiError::UnknownAggregate(id.0))?,
        })
    }
}
