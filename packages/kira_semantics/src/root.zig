pub const analyze = @import("analyzer.zig").analyze;
pub const analyzeWithImports = @import("analyzer.zig").analyzeWithImports;
pub const analyzeWithImportsOptions = @import("analyzer.zig").analyzeWithImportsOptions;
pub const AnalysisOptions = @import("analyzer.zig").AnalysisOptions;
pub const analyzeLibrary = @import("analyzer.zig").analyzeLibrary;
pub const ImportedGlobals = @import("analyzer.zig").ImportedGlobals;
pub const ImportedFunction = @import("imported_globals.zig").ImportedFunction;
pub const ImportedType = @import("imported_globals.zig").ImportedType;
pub const ImportedField = @import("imported_globals.zig").ImportedField;
pub const ImportedAnnotation = @import("imported_globals.zig").ImportedAnnotation;
pub const ResolvedType = @import("kira_semantics_model").ResolvedType;
pub const LoweringContext = @import("lower_shared.zig").Context;
pub const typeFromSyntax = @import("lower_shared.zig").typeFromSyntax;
pub const lowerAnnotationDecl = @import("lower_shared.zig").lowerAnnotationDecl;
pub const resolveForeignFunction = @import("lower_shared.zig").resolveForeignFunction;
pub const resolveNamedTypeInfo = @import("lower_shared.zig").resolveNamedTypeInfo;
pub const lowerFieldDefaultExpr = @import("lower_program.zig").lowerFieldDefaultExpr;

test {
    _ = @import("analyzer.zig");
}
