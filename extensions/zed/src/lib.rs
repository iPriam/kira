//! Zed extension for the Kira programming language.
//!
//! Two surfaces: a Tree-sitter grammar for highlighting (declared in
//! `extension.toml`, no Rust involved), and the language server wiring below.
//!
//! The server is `kira-language-server` from the `kira-lsp` crate. It speaks
//! LSP over stdio, takes no CLI arguments, and publishes diagnostics —
//! everything else answers MethodNotFound. There is deliberately no
//! `language_server_initialization_options`: the server has no options to
//! initialize.

use zed_extension_api::{
    self as zed, Command, LanguageServerId, Result, Worktree,
    settings::{CommandSettings, LspSettings},
};

/// The binary the `kira-lsp` crate installs, and the name we look for on PATH.
const SERVER_BINARY: &str = "kira-language-server";

/// The id in `extension.toml` under `[language_servers.*]`. Zed passes this
/// back to us as the `LanguageServerId`, and it is the key users write under
/// `"lsp"` in their Zed settings, so it must match both places exactly.
const SERVER_ID: &str = "kira-lsp";

#[derive(Default)]
struct KiraExtension;

impl KiraExtension {
    /// Tier 1: an explicit override in the user's Zed settings.
    ///
    /// ```jsonc
    /// { "lsp": { "kira-lsp": { "binary": { "path": "…", "arguments": [] } } } }
    /// ```
    ///
    /// This is the escape hatch for anyone running a server build that is not
    /// on PATH — a `target/release` build straight out of a kira-rusty
    /// checkout, say, or a second toolchain kept deliberately out of the way.
    ///
    /// Checked before discovery, so that an explicit path always beats
    /// something we found ourselves — a setting that lost to a lucky PATH hit
    /// would look broken. Zed may well honour `binary.path` itself and never
    /// call this at all; that costs nothing and keeps the extension correct on
    /// its own terms rather than relying on a host behaviour we do not
    /// control.
    ///
    /// # Why an error here is carried rather than raised or dropped
    ///
    /// `LspSettings::for_worktree` fails in two situations this code cannot
    /// tell apart: the host call failed, or the JSON did not deserialize. An
    /// unset `lsp.kira-lsp` key lands in one of them depending on what Zed
    /// sends back for a key nobody wrote — `{}` deserializes to a default,
    /// while `null` is a deserialize error — and which of those Zed does is
    /// not knowable from this side.
    ///
    /// So the error is neither raised nor dropped. Raising it would break
    /// every user with no settings at all if Zed sends `null`; dropping it
    /// would silently ignore a malformed override, run a different binary than
    /// the one asked for, and look like the setting did nothing. It is carried
    /// instead, and surfaces in the failure message — the one place it is
    /// certainly worth reading, and never a false alarm.
    fn configured(worktree: &Worktree) -> (Option<CommandSettings>, Option<String>) {
        match LspSettings::for_worktree(SERVER_ID, worktree) {
            Ok(settings) => (settings.binary, None),
            Err(error) => (None, Some(error)),
        }
    }

    /// Tier 2: find the binary on the worktree's PATH.
    ///
    /// `worktree.which` — never a global/process-level lookup. Zed resolves
    /// this against the shell environment it derives for the worktree, which
    /// is the only lookup that matches what the user sees in their own
    /// terminal. Zed launched from Finder or the Dock inherits launchd's
    /// minimal environment, not the one from `.zshrc`/`.zprofile`, so
    /// `~/.cargo/bin` — where `cargo install` puts the server — is absent from
    /// the process PATH. A global lookup would therefore resolve for users who
    /// start Zed from a terminal and fail for everyone else, with the same
    /// install and no way to tell the two apart. `worktree.which` sees the
    /// worktree's shell env and finds the binary in both cases.
    fn discover_on_path(worktree: &Worktree) -> Option<String> {
        worktree.which(SERVER_BINARY)
    }

    /// The message a user sees when the server could not be found at all.
    ///
    /// This is the extension's only voice: everything else it does is silent.
    /// So it names the install command rather than the problem, and appends any
    /// settings error, which at this point is the likeliest cause — someone
    /// wrote an override, mistyped it, and is now being told their binary is
    /// missing without being told why the override did not take.
    fn not_found_message(settings_error: Option<&str>) -> String {
        let mut message = format!(
            "{SERVER_BINARY} not found in PATH. Install it from a kira-rusty \
             checkout with `cargo install --path crates/kira-lsp`, then restart \
             Zed. If it is installed somewhere Zed cannot see, set an explicit \
             path in your Zed settings: {{\"lsp\": {{\"{SERVER_ID}\": \
             {{\"binary\": {{\"path\": \"/absolute/path/to/{SERVER_BINARY}\"}}}}}}}}"
        );
        if let Some(error) = settings_error {
            message.push_str(&format!(
                "\n\nYour `lsp.{SERVER_ID}` settings could not be read ({error}). \
                 If you set a binary path there, this is why it was not used."
            ));
        }
        message
    }
}

impl zed::Extension for KiraExtension {
    fn new() -> Self {
        Self
    }

    /// Resolve the server: explicit override, then the worktree's PATH, then
    /// an error that tells the user exactly how to fix it.
    ///
    /// There is no download tier, and that is a decision rather than an
    /// omission. Other Zed extensions fetch a release asset from GitHub when
    /// discovery fails; `kira-lsp` is unpublished and has no release pipeline,
    /// so there is no artifact to fetch and no URL that would resolve. Code
    /// written against a release that does not exist cannot be run or tested,
    /// and would fail at the network with something far less legible than the
    /// message below. When releases exist, a download tier belongs between
    /// PATH discovery and the error — until then, `cargo install` is the
    /// install path and the error names it.
    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let (binary, settings_error) = Self::configured(worktree);

        // Each field of `binary` stands on its own. `arguments` without a
        // `path` means "run whatever you find, with these arguments" — dropping
        // them because a sibling field was absent would be a setting that
        // silently did nothing.
        let configured_path = binary.as_ref().and_then(|binary| binary.path.clone());
        let args = binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            // The server takes no arguments of its own: no --stdio flag, no
            // config path. stdio is its only transport.
            .unwrap_or_default();
        let env = match binary.as_ref().and_then(|binary| binary.env.clone()) {
            Some(configured) => configured.into_iter().collect(),
            // Otherwise the worktree's shell env, so the server runs with the
            // same environment the user's terminal would give it.
            None => worktree.shell_env(),
        };

        let command = match configured_path {
            Some(path) => path,
            None => Self::discover_on_path(worktree)
                .ok_or_else(|| Self::not_found_message(settings_error.as_deref()))?,
        };

        Ok(Command { command, args, env })
    }
}

zed::register_extension!(KiraExtension);
