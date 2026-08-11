//! Compiler-owned autobind profiles.
//!
//! A profile is still autobinding: it emits the same ordinary Kira
//! `@FFI.Extern` source as a C-header profile, but its declaration surface is
//! owned by the compiler rather than copied into a package. This is the seam
//! for native implementations whose implementation language is not C and
//! therefore has no vendor header to parse — currently the ReDB runtime.

use kira_native_lib_definition::NativeLibrarySpec;

use super::model::{BindingModule, FunctionDecl, KiraType, OpaqueDecl, ParamDecl, PointerDecl};

/// Returns the compiler-owned profile name, if `spec` asks for one.
pub(super) fn profile(spec: &NativeLibrarySpec) -> Option<&str> {
    spec.autobind()?
        .profile
        .as_ref()
        .map(|profile| profile.as_str())
        .filter(|profile| profile.eq_ignore_ascii_case("redb"))
}

/// Builds the typed Kira binding surface for the compiler-owned ReDB runtime.
pub(super) fn module(profile: &str) -> BindingModule {
    debug_assert!(profile.eq_ignore_ascii_case("redb"));

    let handle = "kira_redb_handle";
    let handle_ptr = "kira_redb_handle_ptr";
    let write = "kira_redb_write_txn";
    let write_ptr = "kira_redb_write_txn_ptr";

    let mut module = BindingModule {
        library: "redb".to_owned(),
        opaques: vec![
            OpaqueDecl {
                name: handle.to_owned(),
            },
            OpaqueDecl {
                name: write.to_owned(),
            },
        ],
        pointers: vec![
            PointerDecl {
                name: handle_ptr.to_owned(),
                target: handle.to_owned(),
            },
            PointerDecl {
                name: write_ptr.to_owned(),
                target: write.to_owned(),
            },
        ],
        ..BindingModule::default()
    };

    let cstring = KiraType::CString;
    let handle_type = KiraType::Named(handle_ptr.to_owned());
    let write_type = KiraType::Named(write_ptr.to_owned());
    let void = KiraType::Void;
    let i32 = KiraType::Int("I32");
    let bool_type = KiraType::Bool;

    module.functions = vec![
        function(
            "kira_redb_open",
            vec![param("path", cstring.clone())],
            handle_type.clone(),
        ),
        function(
            "kira_redb_close",
            vec![param("database", handle_type.clone())],
            void.clone(),
        ),
        function(
            "kira_redb_handle_is_valid",
            vec![param("database", handle_type.clone())],
            bool_type.clone(),
        ),
        function(
            "kira_redb_put",
            vec![
                param("database", handle_type.clone()),
                param("table", cstring.clone()),
                param("key", cstring.clone()),
                param("value", cstring.clone()),
            ],
            i32.clone(),
        ),
        function(
            "kira_redb_get",
            vec![
                param("database", handle_type.clone()),
                param("table", cstring.clone()),
                param("key", cstring.clone()),
            ],
            cstring.clone(),
        ),
        function(
            "kira_redb_contains",
            vec![
                param("database", handle_type.clone()),
                param("table", cstring.clone()),
                param("key", cstring.clone()),
            ],
            bool_type.clone(),
        ),
        function(
            "kira_redb_delete",
            vec![
                param("database", handle_type.clone()),
                param("table", cstring.clone()),
                param("key", cstring.clone()),
            ],
            i32.clone(),
        ),
        function(
            "kira_redb_last_error",
            vec![param("database", handle_type.clone())],
            cstring.clone(),
        ),
        function(
            "kira_redb_write_begin",
            vec![param("database", handle_type.clone())],
            write_type.clone(),
        ),
        function(
            "kira_redb_write_txn_is_valid",
            vec![param("transaction", write_type.clone())],
            bool_type,
        ),
        function(
            "kira_redb_write_put",
            vec![
                param("transaction", write_type.clone()),
                param("table", cstring.clone()),
                param("key", cstring.clone()),
                param("value", cstring.clone()),
            ],
            i32.clone(),
        ),
        function(
            "kira_redb_write_delete",
            vec![
                param("transaction", write_type.clone()),
                param("table", cstring.clone()),
                param("key", cstring),
            ],
            i32.clone(),
        ),
        function(
            "kira_redb_write_commit",
            vec![param("transaction", write_type)],
            i32,
        ),
        function(
            "kira_redb_write_abort",
            vec![param("transaction", KiraType::Named(write_ptr.to_owned()))],
            void,
        ),
    ];

    module
}

fn param(name: &str, param_type: KiraType) -> ParamDecl {
    ParamDecl {
        name: name.to_owned(),
        param_type,
    }
}

fn function(symbol: &str, params: Vec<ParamDecl>, result: KiraType) -> FunctionDecl {
    FunctionDecl {
        symbol: symbol.to_owned(),
        params,
        result,
    }
}

#[cfg(test)]
mod tests {
    use super::module;

    #[test]
    fn redb_profile_exposes_the_complete_transaction_surface() {
        let binding = module("redb");
        let symbols: Vec<&str> = binding
            .functions
            .iter()
            .map(|function| function.symbol.as_str())
            .collect();

        assert_eq!(binding.library, "redb");
        assert_eq!(binding.opaques.len(), 2);
        assert_eq!(binding.pointers.len(), 2);
        assert_eq!(
            symbols,
            vec![
                "kira_redb_open",
                "kira_redb_close",
                "kira_redb_handle_is_valid",
                "kira_redb_put",
                "kira_redb_get",
                "kira_redb_contains",
                "kira_redb_delete",
                "kira_redb_last_error",
                "kira_redb_write_begin",
                "kira_redb_write_txn_is_valid",
                "kira_redb_write_put",
                "kira_redb_write_delete",
                "kira_redb_write_commit",
                "kira_redb_write_abort",
            ]
        );
    }
}
