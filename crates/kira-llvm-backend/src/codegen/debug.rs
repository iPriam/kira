//! Debug metadata for LLVM-generated Kira functions.
//!
//! The builder deliberately records function identities and one source line
//! per body first. That gives LLDB stable names and useful stop locations now,
//! while leaving room for expression-level locations once the IR carries them.

use std::path::{Path, PathBuf};

use kira_debug::DebugInfo;
use kira_semantics_model::Type;
use llvm_sys::core::{
    LLVMAddModuleFlag, LLVMConstInt, LLVMInt32TypeInContext, LLVMSetCurrentDebugLocation2,
    LLVMValueAsMetadata,
};
use llvm_sys::debuginfo::{
    LLVMCreateDIBuilder, LLVMDIBuilderCreateAutoVariable, LLVMDIBuilderCreateBasicType,
    LLVMDIBuilderCreateCompileUnit, LLVMDIBuilderCreateDebugLocation,
    LLVMDIBuilderCreateExpression, LLVMDIBuilderCreateFile, LLVMDIBuilderCreateFunction,
    LLVMDIBuilderCreateSubroutineType, LLVMDIBuilderCreateUnspecifiedType, LLVMDIBuilderFinalize,
    LLVMDIBuilderInsertDeclareRecordAtEnd, LLVMDIFlagZero, LLVMDWARFEmissionKind,
    LLVMDWARFSourceLanguage, LLVMDisposeDIBuilder, LLVMSetSubprogram,
};
use llvm_sys::prelude::*;

use super::ffi::c_string;

/// The DIBuilder and metadata nodes owned by one LLVM module.
pub(super) struct DebugBuilder {
    builder: LLVMDIBuilderRef,
    llvm_builder: LLVMBuilderRef,
    context: LLVMContextRef,
    file: LLVMMetadataRef,
    scopes: Vec<Option<LLVMMetadataRef>>,
    lines: Vec<u32>,
    finalized: bool,
}

impl DebugBuilder {
    /// Creates a DWARF/CodeView compile unit and one subprogram scope per Kira
    /// function.
    pub(super) fn new(
        module: LLVMModuleRef,
        context: LLVMContextRef,
        llvm_builder: LLVMBuilderRef,
        info: &DebugInfo,
    ) -> Self {
        let source_path = info
            .source
            .as_ref()
            .map(|source| source.path.clone())
            .unwrap_or_else(|| PathBuf::from(&info.module_name));
        let filename = source_path
            .file_name()
            .unwrap_or_else(|| source_path.as_os_str())
            .to_string_lossy();
        let directory = source_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy();
        let filename = c_string(&filename);
        let directory = c_string(&directory);
        let producer = c_string("kira");
        let empty = c_string("");

        // Keep DWARF explicit in every object. On MSVC, also request the
        // platform's CodeView companion: LLDB can use the resulting PDB when
        // the linker consumes the COFF object. The two flags are compatible,
        // so object inspection still has a portable DWARF record while the
        // native Windows debugger gets its normal symbol format.
        // SAFETY: each flag value is a constant in this live context and the
        // module remains owned by this builder for the rest of the call.
        unsafe {
            let dwarf_version =
                LLVMValueAsMetadata(LLVMConstInt(LLVMInt32TypeInContext(context), 4, 0));
            LLVMAddModuleFlag(
                module,
                llvm_sys::LLVMModuleFlagBehavior::LLVMModuleFlagBehaviorOverride,
                c"Dwarf Version".as_ptr(),
                "Dwarf Version".len(),
                dwarf_version,
            );
            #[cfg(target_env = "msvc")]
            {
                let codeview =
                    LLVMValueAsMetadata(LLVMConstInt(LLVMInt32TypeInContext(context), 1, 0));
                LLVMAddModuleFlag(
                    module,
                    llvm_sys::LLVMModuleFlagBehavior::LLVMModuleFlagBehaviorOverride,
                    c"CodeView".as_ptr(),
                    "CodeView".len(),
                    codeview,
                );
            }
        }

        // SAFETY: all metadata is created by the builder for this live module;
        // the borrowed C strings remain alive for every call that consumes them.
        let (builder, file, compile_unit, subroutine_type) = unsafe {
            let builder = LLVMCreateDIBuilder(module);
            let file = LLVMDIBuilderCreateFile(
                builder,
                filename.as_ptr(),
                filename.as_bytes().len(),
                directory.as_ptr(),
                directory.as_bytes().len(),
            );
            let compile_unit = LLVMDIBuilderCreateCompileUnit(
                builder,
                LLVMDWARFSourceLanguage::LLVMDWARFSourceLanguageC,
                file,
                producer.as_ptr(),
                producer.as_bytes().len(),
                i32::from(info.optimized),
                empty.as_ptr(),
                0,
                0,
                empty.as_ptr(),
                0,
                LLVMDWARFEmissionKind::LLVMDWARFEmissionKindFull,
                0,
                0,
                0,
                empty.as_ptr(),
                0,
                empty.as_ptr(),
                0,
            );
            let subroutine_type = LLVMDIBuilderCreateSubroutineType(
                builder,
                file,
                std::ptr::null_mut(),
                0,
                LLVMDIFlagZero,
            );
            (builder, file, compile_unit, subroutine_type)
        };

        let scopes = info
            .functions
            .iter()
            .map(|function| {
                let name = c_string(&function.name);
                let linkage =
                    c_string(function.symbol.as_deref().unwrap_or(function.name.as_str()));
                // SAFETY: the scope and file belong to this builder's module;
                // the strings are valid for the duration of this C call.
                let scope = unsafe {
                    LLVMDIBuilderCreateFunction(
                        builder,
                        compile_unit,
                        name.as_ptr(),
                        name.as_bytes().len(),
                        linkage.as_ptr(),
                        linkage.as_bytes().len(),
                        file,
                        function.line.max(1),
                        subroutine_type,
                        0,
                        1,
                        function.line.max(1),
                        LLVMDIFlagZero,
                        i32::from(info.optimized),
                    )
                };
                Some(scope)
            })
            .collect();
        let lines = info
            .functions
            .iter()
            .map(|function| function.line.max(1))
            .collect();

        Self {
            builder,
            llvm_builder,
            context,
            file,
            scopes,
            lines,
            finalized: false,
        }
    }

    /// Attaches the Kira subprogram scope to a native LLVM function.
    pub(super) fn attach(&self, index: usize, function: LLVMValueRef) {
        let Some(Some(scope)) = self.scopes.get(index) else {
            return;
        };
        // SAFETY: `function` belongs to the module used to create these
        // metadata nodes, and `scope` is one of this builder's subprograms.
        unsafe { LLVMSetSubprogram(function, *scope) };
    }

    /// Sets the current builder location to a function's declaration line.
    pub(super) fn set_location(&self, index: usize) {
        let Some(Some(scope)) = self.scopes.get(index) else {
            return;
        };
        let line = self.lines.get(index).copied().unwrap_or(1);
        // SAFETY: the scope belongs to this context and remains live until the
        // builder is finalized; the LLVM builder is also owned by this module.
        let location = unsafe {
            LLVMDIBuilderCreateDebugLocation(self.context, line, 1, *scope, std::ptr::null_mut())
        };
        // SAFETY: `location` is a metadata node from the same live context.
        unsafe { LLVMSetCurrentDebugLocation2(self.llvm_builder, location) };
    }

    /// Describes the non-parameter local slots in a native function.
    ///
    /// The backend intentionally keeps the first version slot-named: lowering
    /// has stable local indices but the IR does not retain source binding
    /// names yet. A declaration still gives DWARF/LLDB the address, lifetime,
    /// and scalar type needed to inspect a live slot while preserving that
    /// index identity across VM and native backends.
    pub(super) fn declare_locals(
        &self,
        index: usize,
        parameter_count: usize,
        types: &[Type],
        storage: &[LLVMValueRef],
        entry: LLVMBasicBlockRef,
    ) {
        let Some(Some(scope)) = self.scopes.get(index) else {
            return;
        };
        let line = self.lines.get(index).copied().unwrap_or(1);
        let location = unsafe {
            // SAFETY: the scope belongs to this builder's live context.
            LLVMDIBuilderCreateDebugLocation(self.context, line, 1, *scope, std::ptr::null_mut())
        };
        let expression = unsafe {
            // SAFETY: an empty expression is the identity address expression;
            // it is owned by this live DIBuilder.
            LLVMDIBuilderCreateExpression(self.builder, std::ptr::null_mut(), 0)
        };
        for (slot, (&ty, &pointer)) in types.iter().zip(storage).enumerate() {
            if slot < parameter_count || pointer.is_null() {
                continue;
            }
            let name = c_string(&format!("local.{slot}"));
            let debug_type = self.local_type(ty);
            // SAFETY: `scope`, `file`, `debug_type`, and `expression` all
            // belong to this builder's module; `pointer` is the entry-block
            // alloca allocated for this local, and `entry` is that same block.
            unsafe {
                let variable = LLVMDIBuilderCreateAutoVariable(
                    self.builder,
                    *scope,
                    name.as_ptr(),
                    name.as_bytes().len(),
                    self.file,
                    line,
                    debug_type,
                    1,
                    LLVMDIFlagZero,
                    0,
                );
                LLVMDIBuilderInsertDeclareRecordAtEnd(
                    self.builder,
                    pointer,
                    variable,
                    expression,
                    location,
                    entry,
                );
            }
        }
    }

    /// Creates the small type vocabulary needed for native slot inspection.
    fn local_type(&self, ty: Type) -> LLVMMetadataRef {
        let (name, size, encoding) = match ty {
            Type::Int(_) => ("Int", 64, 0x05),
            Type::Float(_) => ("Float", 64, 0x04),
            Type::Bool => ("Bool", 1, 0x02),
            Type::RawPtr | Type::ForeignPtr(_) | Type::NativeState(_) | Type::Task(_) => {
                ("Pointer", 64, 0x01)
            }
            Type::String
            | Type::Array(_)
            | Type::Enum(_)
            | Type::Cell(_)
            | Type::Any
            | Type::Struct(_)
            | Type::Void
            | Type::Error
            | Type::CString => return self.unspecified_type("KiraValue"),
        };
        let name = c_string(name);
        // SAFETY: the name is live for this C call, and all metadata belongs to
        // this builder's module context.
        unsafe {
            LLVMDIBuilderCreateBasicType(
                self.builder,
                name.as_ptr(),
                name.as_bytes().len(),
                size,
                encoding,
                LLVMDIFlagZero,
            )
        }
    }

    fn unspecified_type(&self, name: &str) -> LLVMMetadataRef {
        let name = c_string(name);
        // SAFETY: the name is live for this C call and the result belongs to
        // this builder's module context.
        unsafe {
            LLVMDIBuilderCreateUnspecifiedType(self.builder, name.as_ptr(), name.as_bytes().len())
        }
    }

    /// Clears a Kira source location before generated adapter/entry helpers.
    pub(super) fn clear_location(&self) {
        // SAFETY: a null location is LLVM's documented way to clear the
        // builder's current debug location.
        unsafe { LLVMSetCurrentDebugLocation2(self.llvm_builder, std::ptr::null_mut()) };
    }

    /// Finalizes deferred metadata before the module is verified or disposed.
    pub(super) fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        // SAFETY: this builder owns all unresolved metadata created above and
        // has not been finalized before.
        unsafe { LLVMDIBuilderFinalize(self.builder) };
        self.finalized = true;
    }
}

impl Drop for DebugBuilder {
    fn drop(&mut self) {
        self.finalize();
        // SAFETY: finalization has happened at most once and the builder is
        // disposed exactly once after all metadata users are gone.
        unsafe { LLVMDisposeDIBuilder(self.builder) };
    }
}
