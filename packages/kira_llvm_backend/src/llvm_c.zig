const std = @import("std");
const builtin = @import("builtin");
const toolchain = @import("toolchain.zig");
const NativeLibrary = if (builtin.os.tag == .windows) WindowsNativeLibrary else std.DynLib;

pub const c = @cImport({
    @cInclude("llvm-c/Analysis.h");
    @cInclude("llvm-c/Core.h");
    @cInclude("llvm-c/Target.h");
    @cInclude("llvm-c/TargetMachine.h");
});

pub const Api = struct {
    lib: NativeLibrary,

    LLVMContextCreate: *const @TypeOf(c.LLVMContextCreate),
    LLVMContextDispose: *const @TypeOf(c.LLVMContextDispose),
    LLVMModuleCreateWithNameInContext: *const @TypeOf(c.LLVMModuleCreateWithNameInContext),
    LLVMDisposeModule: *const @TypeOf(c.LLVMDisposeModule),
    LLVMCreateBuilderInContext: *const @TypeOf(c.LLVMCreateBuilderInContext),
    LLVMDisposeBuilder: *const @TypeOf(c.LLVMDisposeBuilder),
    LLVMInt1TypeInContext: *const @TypeOf(c.LLVMInt1TypeInContext),
    LLVMInt8TypeInContext: *const @TypeOf(c.LLVMInt8TypeInContext),
    LLVMInt32TypeInContext: *const @TypeOf(c.LLVMInt32TypeInContext),
    LLVMInt64TypeInContext: *const @TypeOf(c.LLVMInt64TypeInContext),
    LLVMFloatTypeInContext: *const @TypeOf(c.LLVMFloatTypeInContext),
    LLVMDoubleTypeInContext: *const @TypeOf(c.LLVMDoubleTypeInContext),
    LLVMIntPtrTypeInContext: *const @TypeOf(c.LLVMIntPtrTypeInContext),
    LLVMVoidTypeInContext: *const @TypeOf(c.LLVMVoidTypeInContext),
    LLVMPointerTypeInContext: *const @TypeOf(c.LLVMPointerTypeInContext),
    LLVMStructTypeInContext: *const @TypeOf(c.LLVMStructTypeInContext),
    LLVMStructCreateNamed: *const @TypeOf(c.LLVMStructCreateNamed),
    LLVMStructSetBody: *const @TypeOf(c.LLVMStructSetBody),
    LLVMInt16TypeInContext: *const @TypeOf(c.LLVMInt16TypeInContext),
    LLVMIntTypeInContext: *const @TypeOf(c.LLVMIntTypeInContext),
    LLVMArrayType2: *const @TypeOf(c.LLVMArrayType2),
    LLVMFunctionType: *const @TypeOf(c.LLVMFunctionType),
    LLVMAddFunction: *const @TypeOf(c.LLVMAddFunction),
    LLVMAppendBasicBlockInContext: *const @TypeOf(c.LLVMAppendBasicBlockInContext),
    LLVMPositionBuilderAtEnd: *const @TypeOf(c.LLVMPositionBuilderAtEnd),
    LLVMBuildRetVoid: *const @TypeOf(c.LLVMBuildRetVoid),
    LLVMBuildRet: *const @TypeOf(c.LLVMBuildRet),
    LLVMBuildAlloca: *const @TypeOf(c.LLVMBuildAlloca),
    LLVMBuildStore: *const @TypeOf(c.LLVMBuildStore),
    LLVMBuildLoad2: *const @TypeOf(c.LLVMBuildLoad2),
    LLVMBuildAdd: *const @TypeOf(c.LLVMBuildAdd),
    LLVMBuildSub: *const @TypeOf(c.LLVMBuildSub),
    LLVMBuildMul: *const @TypeOf(c.LLVMBuildMul),
    LLVMBuildSDiv: *const @TypeOf(c.LLVMBuildSDiv),
    LLVMBuildSRem: *const @TypeOf(c.LLVMBuildSRem),
    LLVMBuildFAdd: *const @TypeOf(c.LLVMBuildFAdd),
    LLVMBuildFSub: *const @TypeOf(c.LLVMBuildFSub),
    LLVMBuildFMul: *const @TypeOf(c.LLVMBuildFMul),
    LLVMBuildFDiv: *const @TypeOf(c.LLVMBuildFDiv),
    LLVMBuildFRem: *const @TypeOf(c.LLVMBuildFRem),
    LLVMBuildNeg: *const @TypeOf(c.LLVMBuildNeg),
    LLVMBuildFNeg: *const @TypeOf(c.LLVMBuildFNeg),
    LLVMBuildXor: *const @TypeOf(c.LLVMBuildXor),
    LLVMBuildICmp: *const @TypeOf(c.LLVMBuildICmp),
    LLVMBuildFCmp: *const @TypeOf(c.LLVMBuildFCmp),
    LLVMBuildBr: *const @TypeOf(c.LLVMBuildBr),
    LLVMBuildCondBr: *const @TypeOf(c.LLVMBuildCondBr),
    LLVMBuildSwitch: *const @TypeOf(c.LLVMBuildSwitch),
    LLVMAddCase: *const @TypeOf(c.LLVMAddCase),
    LLVMBuildAnd: *const @TypeOf(c.LLVMBuildAnd),
    LLVMBuildOr: *const @TypeOf(c.LLVMBuildOr),
    LLVMBuildUnreachable: *const @TypeOf(c.LLVMBuildUnreachable),
    LLVMBuildCall2: *const @TypeOf(c.LLVMBuildCall2),
    LLVMBuildExtractValue: *const @TypeOf(c.LLVMBuildExtractValue),
    LLVMBuildInsertValue: *const @TypeOf(c.LLVMBuildInsertValue),
    LLVMBuildSelect: *const @TypeOf(c.LLVMBuildSelect),
    LLVMBuildNot: *const @TypeOf(c.LLVMBuildNot),
    LLVMBuildIntToPtr: *const @TypeOf(c.LLVMBuildIntToPtr),
    LLVMBuildPtrToInt: *const @TypeOf(c.LLVMBuildPtrToInt),
    LLVMBuildZExt: *const @TypeOf(c.LLVMBuildZExt),
    LLVMBuildSExt: *const @TypeOf(c.LLVMBuildSExt),
    LLVMBuildTrunc: *const @TypeOf(c.LLVMBuildTrunc),
    LLVMBuildFPExt: *const @TypeOf(c.LLVMBuildFPExt),
    LLVMBuildFPTrunc: *const @TypeOf(c.LLVMBuildFPTrunc),
    LLVMBuildSIToFP: *const @TypeOf(c.LLVMBuildSIToFP),
    LLVMBuildFPToSI: *const @TypeOf(c.LLVMBuildFPToSI),
    LLVMBuildBitCast: *const @TypeOf(c.LLVMBuildBitCast),
    LLVMBuildInBoundsGEP2: *const @TypeOf(c.LLVMBuildInBoundsGEP2),
    LLVMGetParam: *const @TypeOf(c.LLVMGetParam),
    LLVMGetUndef: *const @TypeOf(c.LLVMGetUndef),
    LLVMConstNull: *const @TypeOf(c.LLVMConstNull),
    LLVMSizeOf: *const @TypeOf(c.LLVMSizeOf),
    LLVMConstReal: *const @TypeOf(c.LLVMConstReal),
    LLVMTypeOf: *const @TypeOf(c.LLVMTypeOf),
    LLVMGetBasicBlockTerminator: *const @TypeOf(c.LLVMGetBasicBlockTerminator),
    LLVMGetInsertBlock: *const @TypeOf(c.LLVMGetInsertBlock),
    LLVMGetEntryBasicBlock: *const @TypeOf(c.LLVMGetEntryBasicBlock),
    LLVMPositionBuilderBefore: *const @TypeOf(c.LLVMPositionBuilderBefore),
    LLVMConstInt: *const @TypeOf(c.LLVMConstInt),
    LLVMConstStringInContext2: *const @TypeOf(c.LLVMConstStringInContext2),
    LLVMConstNamedStruct: *const @TypeOf(c.LLVMConstNamedStruct),
    LLVMConstInBoundsGEP2: *const @TypeOf(c.LLVMConstInBoundsGEP2),
    LLVMAddGlobal: *const @TypeOf(c.LLVMAddGlobal),
    LLVMSetInitializer: *const @TypeOf(c.LLVMSetInitializer),
    LLVMSetGlobalConstant: *const @TypeOf(c.LLVMSetGlobalConstant),
    LLVMSetLinkage: *const @TypeOf(c.LLVMSetLinkage),
    LLVMSetDLLStorageClass: *const @TypeOf(c.LLVMSetDLLStorageClass),
    LLVMSetTarget: *const @TypeOf(c.LLVMSetTarget),
    LLVMSetModuleDataLayout: *const @TypeOf(c.LLVMSetModuleDataLayout),
    LLVMVerifyModule: *const @TypeOf(c.LLVMVerifyModule),
    LLVMDisposeMessage: *const @TypeOf(c.LLVMDisposeMessage),
    LLVMPrintModuleToString: *const @TypeOf(c.LLVMPrintModuleToString),
    LLVMGetTargetFromTriple: *const @TypeOf(c.LLVMGetTargetFromTriple),
    LLVMCreateTargetMachine: *const @TypeOf(c.LLVMCreateTargetMachine),
    LLVMDisposeTargetMachine: *const @TypeOf(c.LLVMDisposeTargetMachine),
    LLVMCreateTargetDataLayout: *const @TypeOf(c.LLVMCreateTargetDataLayout),
    LLVMDisposeTargetData: *const @TypeOf(c.LLVMDisposeTargetData),
    LLVMCopyStringRepOfTargetData: *const @TypeOf(c.LLVMCopyStringRepOfTargetData),
    LLVMTargetMachineEmitToFile: *const @TypeOf(c.LLVMTargetMachineEmitToFile),
    LLVMGetHostCPUName: *const @TypeOf(c.LLVMGetHostCPUName),
    LLVMGetHostCPUFeatures: *const @TypeOf(c.LLVMGetHostCPUFeatures),

    LLVMInitializeTargetInfo: *const fn () callconv(.c) void,
    LLVMInitializeTarget: *const fn () callconv(.c) void,
    LLVMInitializeTargetMC: *const fn () callconv(.c) void,
    LLVMInitializeAsmPrinter: *const fn () callconv(.c) void,
    LLVMInitializeAsmParser: ?*const fn () callconv(.c) void,

    pub fn open(tc: toolchain.Toolchain) !Api {
        var lib = try openNativeLibrary(tc.llvm_c_library_path);
        errdefer lib.close();

        return .{
            .lib = lib,
            .LLVMContextCreate = try load(&lib, *const @TypeOf(c.LLVMContextCreate), "LLVMContextCreate"),
            .LLVMContextDispose = try load(&lib, *const @TypeOf(c.LLVMContextDispose), "LLVMContextDispose"),
            .LLVMModuleCreateWithNameInContext = try load(&lib, *const @TypeOf(c.LLVMModuleCreateWithNameInContext), "LLVMModuleCreateWithNameInContext"),
            .LLVMDisposeModule = try load(&lib, *const @TypeOf(c.LLVMDisposeModule), "LLVMDisposeModule"),
            .LLVMCreateBuilderInContext = try load(&lib, *const @TypeOf(c.LLVMCreateBuilderInContext), "LLVMCreateBuilderInContext"),
            .LLVMDisposeBuilder = try load(&lib, *const @TypeOf(c.LLVMDisposeBuilder), "LLVMDisposeBuilder"),
            .LLVMInt1TypeInContext = try load(&lib, *const @TypeOf(c.LLVMInt1TypeInContext), "LLVMInt1TypeInContext"),
            .LLVMInt8TypeInContext = try load(&lib, *const @TypeOf(c.LLVMInt8TypeInContext), "LLVMInt8TypeInContext"),
            .LLVMInt32TypeInContext = try load(&lib, *const @TypeOf(c.LLVMInt32TypeInContext), "LLVMInt32TypeInContext"),
            .LLVMInt64TypeInContext = try load(&lib, *const @TypeOf(c.LLVMInt64TypeInContext), "LLVMInt64TypeInContext"),
            .LLVMFloatTypeInContext = try load(&lib, *const @TypeOf(c.LLVMFloatTypeInContext), "LLVMFloatTypeInContext"),
            .LLVMDoubleTypeInContext = try load(&lib, *const @TypeOf(c.LLVMDoubleTypeInContext), "LLVMDoubleTypeInContext"),
            .LLVMIntPtrTypeInContext = try load(&lib, *const @TypeOf(c.LLVMIntPtrTypeInContext), "LLVMIntPtrTypeInContext"),
            .LLVMVoidTypeInContext = try load(&lib, *const @TypeOf(c.LLVMVoidTypeInContext), "LLVMVoidTypeInContext"),
            .LLVMPointerTypeInContext = try load(&lib, *const @TypeOf(c.LLVMPointerTypeInContext), "LLVMPointerTypeInContext"),
            .LLVMStructTypeInContext = try load(&lib, *const @TypeOf(c.LLVMStructTypeInContext), "LLVMStructTypeInContext"),
            .LLVMStructCreateNamed = try load(&lib, *const @TypeOf(c.LLVMStructCreateNamed), "LLVMStructCreateNamed"),
            .LLVMStructSetBody = try load(&lib, *const @TypeOf(c.LLVMStructSetBody), "LLVMStructSetBody"),
            .LLVMInt16TypeInContext = try load(&lib, *const @TypeOf(c.LLVMInt16TypeInContext), "LLVMInt16TypeInContext"),
            .LLVMIntTypeInContext = try load(&lib, *const @TypeOf(c.LLVMIntTypeInContext), "LLVMIntTypeInContext"),
            .LLVMArrayType2 = try load(&lib, *const @TypeOf(c.LLVMArrayType2), "LLVMArrayType2"),
            .LLVMFunctionType = try load(&lib, *const @TypeOf(c.LLVMFunctionType), "LLVMFunctionType"),
            .LLVMAddFunction = try load(&lib, *const @TypeOf(c.LLVMAddFunction), "LLVMAddFunction"),
            .LLVMAppendBasicBlockInContext = try load(&lib, *const @TypeOf(c.LLVMAppendBasicBlockInContext), "LLVMAppendBasicBlockInContext"),
            .LLVMPositionBuilderAtEnd = try load(&lib, *const @TypeOf(c.LLVMPositionBuilderAtEnd), "LLVMPositionBuilderAtEnd"),
            .LLVMBuildRetVoid = try load(&lib, *const @TypeOf(c.LLVMBuildRetVoid), "LLVMBuildRetVoid"),
            .LLVMBuildRet = try load(&lib, *const @TypeOf(c.LLVMBuildRet), "LLVMBuildRet"),
            .LLVMBuildAlloca = try load(&lib, *const @TypeOf(c.LLVMBuildAlloca), "LLVMBuildAlloca"),
            .LLVMBuildStore = try load(&lib, *const @TypeOf(c.LLVMBuildStore), "LLVMBuildStore"),
            .LLVMBuildLoad2 = try load(&lib, *const @TypeOf(c.LLVMBuildLoad2), "LLVMBuildLoad2"),
            .LLVMBuildAdd = try load(&lib, *const @TypeOf(c.LLVMBuildAdd), "LLVMBuildAdd"),
            .LLVMBuildSub = try load(&lib, *const @TypeOf(c.LLVMBuildSub), "LLVMBuildSub"),
            .LLVMBuildMul = try load(&lib, *const @TypeOf(c.LLVMBuildMul), "LLVMBuildMul"),
            .LLVMBuildSDiv = try load(&lib, *const @TypeOf(c.LLVMBuildSDiv), "LLVMBuildSDiv"),
            .LLVMBuildSRem = try load(&lib, *const @TypeOf(c.LLVMBuildSRem), "LLVMBuildSRem"),
            .LLVMBuildFAdd = try load(&lib, *const @TypeOf(c.LLVMBuildFAdd), "LLVMBuildFAdd"),
            .LLVMBuildFSub = try load(&lib, *const @TypeOf(c.LLVMBuildFSub), "LLVMBuildFSub"),
            .LLVMBuildFMul = try load(&lib, *const @TypeOf(c.LLVMBuildFMul), "LLVMBuildFMul"),
            .LLVMBuildFDiv = try load(&lib, *const @TypeOf(c.LLVMBuildFDiv), "LLVMBuildFDiv"),
            .LLVMBuildFRem = try load(&lib, *const @TypeOf(c.LLVMBuildFRem), "LLVMBuildFRem"),
            .LLVMBuildNeg = try load(&lib, *const @TypeOf(c.LLVMBuildNeg), "LLVMBuildNeg"),
            .LLVMBuildFNeg = try load(&lib, *const @TypeOf(c.LLVMBuildFNeg), "LLVMBuildFNeg"),
            .LLVMBuildXor = try load(&lib, *const @TypeOf(c.LLVMBuildXor), "LLVMBuildXor"),
            .LLVMBuildICmp = try load(&lib, *const @TypeOf(c.LLVMBuildICmp), "LLVMBuildICmp"),
            .LLVMBuildFCmp = try load(&lib, *const @TypeOf(c.LLVMBuildFCmp), "LLVMBuildFCmp"),
            .LLVMBuildBr = try load(&lib, *const @TypeOf(c.LLVMBuildBr), "LLVMBuildBr"),
            .LLVMBuildCondBr = try load(&lib, *const @TypeOf(c.LLVMBuildCondBr), "LLVMBuildCondBr"),
            .LLVMBuildSwitch = try load(&lib, *const @TypeOf(c.LLVMBuildSwitch), "LLVMBuildSwitch"),
            .LLVMAddCase = try load(&lib, *const @TypeOf(c.LLVMAddCase), "LLVMAddCase"),
            .LLVMBuildAnd = try load(&lib, *const @TypeOf(c.LLVMBuildAnd), "LLVMBuildAnd"),
            .LLVMBuildOr = try load(&lib, *const @TypeOf(c.LLVMBuildOr), "LLVMBuildOr"),
            .LLVMBuildUnreachable = try load(&lib, *const @TypeOf(c.LLVMBuildUnreachable), "LLVMBuildUnreachable"),
            .LLVMBuildCall2 = try load(&lib, *const @TypeOf(c.LLVMBuildCall2), "LLVMBuildCall2"),
            .LLVMBuildExtractValue = try load(&lib, *const @TypeOf(c.LLVMBuildExtractValue), "LLVMBuildExtractValue"),
            .LLVMBuildInsertValue = try load(&lib, *const @TypeOf(c.LLVMBuildInsertValue), "LLVMBuildInsertValue"),
            .LLVMBuildSelect = try load(&lib, *const @TypeOf(c.LLVMBuildSelect), "LLVMBuildSelect"),
            .LLVMBuildNot = try load(&lib, *const @TypeOf(c.LLVMBuildNot), "LLVMBuildNot"),
            .LLVMBuildIntToPtr = try load(&lib, *const @TypeOf(c.LLVMBuildIntToPtr), "LLVMBuildIntToPtr"),
            .LLVMBuildPtrToInt = try load(&lib, *const @TypeOf(c.LLVMBuildPtrToInt), "LLVMBuildPtrToInt"),
            .LLVMBuildZExt = try load(&lib, *const @TypeOf(c.LLVMBuildZExt), "LLVMBuildZExt"),
            .LLVMBuildSExt = try load(&lib, *const @TypeOf(c.LLVMBuildSExt), "LLVMBuildSExt"),
            .LLVMBuildTrunc = try load(&lib, *const @TypeOf(c.LLVMBuildTrunc), "LLVMBuildTrunc"),
            .LLVMBuildFPExt = try load(&lib, *const @TypeOf(c.LLVMBuildFPExt), "LLVMBuildFPExt"),
            .LLVMBuildFPTrunc = try load(&lib, *const @TypeOf(c.LLVMBuildFPTrunc), "LLVMBuildFPTrunc"),
            .LLVMBuildSIToFP = try load(&lib, *const @TypeOf(c.LLVMBuildSIToFP), "LLVMBuildSIToFP"),
            .LLVMBuildFPToSI = try load(&lib, *const @TypeOf(c.LLVMBuildFPToSI), "LLVMBuildFPToSI"),
            .LLVMBuildBitCast = try load(&lib, *const @TypeOf(c.LLVMBuildBitCast), "LLVMBuildBitCast"),
            .LLVMBuildInBoundsGEP2 = try load(&lib, *const @TypeOf(c.LLVMBuildInBoundsGEP2), "LLVMBuildInBoundsGEP2"),
            .LLVMGetParam = try load(&lib, *const @TypeOf(c.LLVMGetParam), "LLVMGetParam"),
            .LLVMGetUndef = try load(&lib, *const @TypeOf(c.LLVMGetUndef), "LLVMGetUndef"),
            .LLVMConstNull = try load(&lib, *const @TypeOf(c.LLVMConstNull), "LLVMConstNull"),
            .LLVMSizeOf = try load(&lib, *const @TypeOf(c.LLVMSizeOf), "LLVMSizeOf"),
            .LLVMConstReal = try load(&lib, *const @TypeOf(c.LLVMConstReal), "LLVMConstReal"),
            .LLVMTypeOf = try load(&lib, *const @TypeOf(c.LLVMTypeOf), "LLVMTypeOf"),
            .LLVMGetBasicBlockTerminator = try load(&lib, *const @TypeOf(c.LLVMGetBasicBlockTerminator), "LLVMGetBasicBlockTerminator"),
            .LLVMGetInsertBlock = try load(&lib, *const @TypeOf(c.LLVMGetInsertBlock), "LLVMGetInsertBlock"),
            .LLVMGetEntryBasicBlock = try load(&lib, *const @TypeOf(c.LLVMGetEntryBasicBlock), "LLVMGetEntryBasicBlock"),
            .LLVMPositionBuilderBefore = try load(&lib, *const @TypeOf(c.LLVMPositionBuilderBefore), "LLVMPositionBuilderBefore"),
            .LLVMConstInt = try load(&lib, *const @TypeOf(c.LLVMConstInt), "LLVMConstInt"),
            .LLVMConstStringInContext2 = try load(&lib, *const @TypeOf(c.LLVMConstStringInContext2), "LLVMConstStringInContext2"),
            .LLVMConstNamedStruct = try load(&lib, *const @TypeOf(c.LLVMConstNamedStruct), "LLVMConstNamedStruct"),
            .LLVMConstInBoundsGEP2 = try load(&lib, *const @TypeOf(c.LLVMConstInBoundsGEP2), "LLVMConstInBoundsGEP2"),
            .LLVMAddGlobal = try load(&lib, *const @TypeOf(c.LLVMAddGlobal), "LLVMAddGlobal"),
            .LLVMSetInitializer = try load(&lib, *const @TypeOf(c.LLVMSetInitializer), "LLVMSetInitializer"),
            .LLVMSetGlobalConstant = try load(&lib, *const @TypeOf(c.LLVMSetGlobalConstant), "LLVMSetGlobalConstant"),
            .LLVMSetLinkage = try load(&lib, *const @TypeOf(c.LLVMSetLinkage), "LLVMSetLinkage"),
            .LLVMSetDLLStorageClass = try load(&lib, *const @TypeOf(c.LLVMSetDLLStorageClass), "LLVMSetDLLStorageClass"),
            .LLVMSetTarget = try load(&lib, *const @TypeOf(c.LLVMSetTarget), "LLVMSetTarget"),
            .LLVMSetModuleDataLayout = try load(&lib, *const @TypeOf(c.LLVMSetModuleDataLayout), "LLVMSetModuleDataLayout"),
            .LLVMVerifyModule = try load(&lib, *const @TypeOf(c.LLVMVerifyModule), "LLVMVerifyModule"),
            .LLVMDisposeMessage = try load(&lib, *const @TypeOf(c.LLVMDisposeMessage), "LLVMDisposeMessage"),
            .LLVMPrintModuleToString = try load(&lib, *const @TypeOf(c.LLVMPrintModuleToString), "LLVMPrintModuleToString"),
            .LLVMGetTargetFromTriple = try load(&lib, *const @TypeOf(c.LLVMGetTargetFromTriple), "LLVMGetTargetFromTriple"),
            .LLVMCreateTargetMachine = try load(&lib, *const @TypeOf(c.LLVMCreateTargetMachine), "LLVMCreateTargetMachine"),
            .LLVMDisposeTargetMachine = try load(&lib, *const @TypeOf(c.LLVMDisposeTargetMachine), "LLVMDisposeTargetMachine"),
            .LLVMCreateTargetDataLayout = try load(&lib, *const @TypeOf(c.LLVMCreateTargetDataLayout), "LLVMCreateTargetDataLayout"),
            .LLVMDisposeTargetData = try load(&lib, *const @TypeOf(c.LLVMDisposeTargetData), "LLVMDisposeTargetData"),
            .LLVMCopyStringRepOfTargetData = try load(&lib, *const @TypeOf(c.LLVMCopyStringRepOfTargetData), "LLVMCopyStringRepOfTargetData"),
            .LLVMTargetMachineEmitToFile = try load(&lib, *const @TypeOf(c.LLVMTargetMachineEmitToFile), "LLVMTargetMachineEmitToFile"),
            .LLVMGetHostCPUName = try load(&lib, *const @TypeOf(c.LLVMGetHostCPUName), "LLVMGetHostCPUName"),
            .LLVMGetHostCPUFeatures = try load(&lib, *const @TypeOf(c.LLVMGetHostCPUFeatures), "LLVMGetHostCPUFeatures"),
            .LLVMInitializeTargetInfo = try load(&lib, *const fn () callconv(.c) void, tc.init_symbols.target_info),
            .LLVMInitializeTarget = try load(&lib, *const fn () callconv(.c) void, tc.init_symbols.target),
            .LLVMInitializeTargetMC = try load(&lib, *const fn () callconv(.c) void, tc.init_symbols.target_mc),
            .LLVMInitializeAsmPrinter = try load(&lib, *const fn () callconv(.c) void, tc.init_symbols.asm_printer),
            .LLVMInitializeAsmParser = if (tc.init_symbols.asm_parser) |name|
                try load(&lib, *const fn () callconv(.c) void, name)
            else
                null,
        };
    }

    pub fn close(self: *Api) void {
        self.lib.close();
    }
};

fn openNativeLibrary(path: []const u8) !NativeLibrary {
    if (builtin.os.tag == .windows) return WindowsNativeLibrary.open(path);
    return std.DynLib.open(path);
}

fn load(lib: anytype, comptime T: type, name: [:0]const u8) !T {
    return lib.lookup(T, name) orelse {
        std.debug.print("missing LLVM C symbol '{s}'\n", .{name});
        return error.MissingLlvmSymbol;
    };
}

const WindowsNativeLibrary = struct {
    handle: std.os.windows.HMODULE,

    const LOAD_WITH_ALTERED_SEARCH_PATH = 0x00000008;
    extern "kernel32" fn LoadLibraryExW([*:0]const u16, ?std.os.windows.HANDLE, u32) callconv(.winapi) ?std.os.windows.HMODULE;
    extern "kernel32" fn FreeLibrary(std.os.windows.HMODULE) callconv(.winapi) std.os.windows.BOOL;
    extern "kernel32" fn GetProcAddress(std.os.windows.HMODULE, [*:0]const u8) callconv(.winapi) ?*anyopaque;

    fn open(path: []const u8) !WindowsNativeLibrary {
        const path_w = try std.unicode.wtf8ToWtf16LeAllocZ(std.heap.page_allocator, path);
        defer std.heap.page_allocator.free(path_w);
        const handle = LoadLibraryExW(path_w.ptr, null, LOAD_WITH_ALTERED_SEARCH_PATH) orelse return error.NativeLibraryLoadFailed;
        return .{ .handle = handle };
    }

    fn close(self: *WindowsNativeLibrary) void {
        _ = FreeLibrary(self.handle);
    }

    fn lookup(self: *WindowsNativeLibrary, comptime T: type, name: [:0]const u8) ?T {
        const address = GetProcAddress(self.handle, name.ptr) orelse return null;
        return @ptrCast(address);
    }
};
