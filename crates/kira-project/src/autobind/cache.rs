//! The stamp that decides whether a binding has to be generated again.
//!
//! Parsing a header set is the expensive part of a build that has one, and it
//! answers the same question on every `kira check` until a header changes. So
//! each generated file gets a stamp beside it recording what it was generated
//! from: the inputs' sizes and modification times, the declaration that
//! selected them, and the target it was generated for.
//!
//! # Why a file that has no stamp is adopted rather than overwritten
//!
//! A package may ship its bindings in version control — `kira-graphics` does,
//! because regenerating a Vulkan or Direct3D binding needs an SDK that is not
//! on every machine that builds against it. Those files are the package's
//! source. Overwriting them on first build would rewrite tracked files nobody
//! asked to change, and could replace a binding generated with a complete SDK
//! by one generated without it.
//!
//! So a binding that exists with no stamp is *adopted*: the stamp is written
//! from today's inputs, the file is left exactly as it is, and the caller is
//! told so it can say once that this is what happened. Everything after that is
//! ordinary — edit a header and the binding regenerates, because now there is a
//! stamp to go stale.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// What one generated binding was generated from.
///
/// Written as a small text file rather than a serialized struct: it is read by
/// a person as often as by the tool, and a stamp whose format needs a decoder
/// is one more thing to keep in step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stamp {
    /// The declaration and target this binding was generated for, rendered.
    pub(super) key: String,
    /// One line per input file: size, modification time, path.
    pub(super) inputs: Vec<String>,
    /// True when this records a binding the package shipped and this generator
    /// adopted rather than wrote. An adopted binding is re-adopted on every
    /// build — its note repeats, so a reader told it exists can read it — and it
    /// is never regenerated over, because it is the package's own source.
    pub(super) adopted: bool,
}

impl Stamp {
    /// The same binding source: the declaration, target, and inputs, without
    /// regard to whether it was adopted. Provenance decides whether to note and
    /// re-adopt, not whether the recorded inputs still match today's.
    fn same_source(&self, other: &Stamp) -> bool {
        self.key == other.key && self.inputs == other.inputs
    }

    /// Renders the stamp as the text written beside the binding.
    ///
    /// The `adopted` marker sits on its own line after the key. It is a bare
    /// word, which `describe_input` never produces, so a stamp written before
    /// the marker existed reads back as a generated one with its inputs intact.
    fn render(&self) -> String {
        let mut text = String::from("kira-autobind 1\n");
        text.push_str(&self.key);
        text.push('\n');
        if self.adopted {
            text.push_str("adopted\n");
        }
        for input in &self.inputs {
            text.push_str(input);
            text.push('\n');
        }
        text
    }

    /// Reads a stamp back, or `None` when it is absent or not one.
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "kira-autobind 1" {
            return None;
        }
        let key = lines.next()?.to_owned();
        let mut rest: Vec<String> = lines.map(str::to_owned).collect();
        let adopted = rest.first().is_some_and(|line| line == "adopted");
        if adopted {
            rest.remove(0);
        }
        Some(Self {
            key,
            inputs: rest,
            adopted,
        })
    }

    /// This binding source, recorded as one the generator adopted rather than
    /// wrote.
    pub(super) fn adopted(&self) -> Stamp {
        Stamp {
            key: self.key.clone(),
            inputs: self.inputs.clone(),
            adopted: true,
        }
    }
}

/// Describes one input file the way a stamp records it.
///
/// Size and modification time rather than a content hash: the inputs are a
/// header set that may run to thousands of system headers, and reading all of
/// them to decide whether to read all of them is the cost the stamp exists to
/// avoid. A file that is missing is recorded as such, so a header that appears
/// later is a change.
pub(super) fn describe_input(path: &Path) -> String {
    let display = path.display();
    let Ok(metadata) = std::fs::metadata(path) else {
        return format!("absent {display}");
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    format!("{} {modified} {display}", metadata.len())
}

/// Where the stamp for `output` lives.
///
/// Beside the binding under the package's build directory rather than beside
/// the binding itself: a generated Kira file is source a program compiles, and
/// a `.stamp` next to it would be swept up by anything that treats the
/// directory as sources.
pub(super) fn stamp_path(build_dir: &Path, library: &str) -> PathBuf {
    build_dir.join("autobind").join(format!("{library}.stamp"))
}

/// What a stamp comparison says a caller should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Freshness {
    /// The binding is current: nothing to do.
    Current,
    /// The binding must be generated.
    Stale,
    /// The binding exists and was not generated here: adopt it as it stands.
    Adopt,
}

/// Compares the recorded stamp against today's inputs.
pub(super) fn freshness(stamp_file: &Path, output: &Path, wanted: &Stamp) -> Freshness {
    let recorded = std::fs::read_to_string(stamp_file)
        .ok()
        .and_then(|text| Stamp::parse(&text));
    match (recorded, output.exists()) {
        (_, false) => Freshness::Stale,
        (None, true) => Freshness::Adopt,
        // A binding the package shipped stays adopted: it is re-adopted every
        // build so its note repeats, and it is never regenerated over even when
        // a header's timestamp moves, because it is the package's own source.
        (Some(recorded), true) if recorded.adopted => Freshness::Adopt,
        (Some(recorded), true) if recorded.same_source(wanted) => Freshness::Current,
        (Some(_), true) => Freshness::Stale,
    }
}

/// Writes the stamp beside `output`, creating its directory.
pub(super) fn write(stamp_file: &Path, stamp: &Stamp) -> std::io::Result<()> {
    if let Some(parent) = stamp_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(stamp_file, stamp.render())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let base = std::env::temp_dir().join(format!(
                "kira-autobind-cache-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id(),
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).expect("a scratch directory");
            TempDir(base)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn stamp() -> Stamp {
        Stamp {
            key: "kiratext aarch64-macos-none all_public".to_owned(),
            inputs: vec!["120 17 /pkg/NativeLibs/Text/kira_text.h".to_owned()],
            adopted: false,
        }
    }

    #[test]
    fn a_stamp_round_trips_through_its_own_text() {
        let rendered = stamp().render();
        assert_eq!(Stamp::parse(&rendered), Some(stamp()));
    }

    #[test]
    fn an_adopted_stamp_round_trips_and_keeps_its_inputs() {
        let adopted = stamp().adopted();
        assert!(adopted.adopted);
        assert_eq!(Stamp::parse(&adopted.render()), Some(adopted.clone()));
        // The marker is not mistaken for an input.
        assert_eq!(adopted.inputs, stamp().inputs);
    }

    #[test]
    fn an_adopted_binding_stays_adopted_so_its_note_repeats() {
        let dir = TempDir::new("adopted-recurs");
        let output = dir.0.join("text.kira");
        let stamp_file = dir.0.join("text.stamp");
        std::fs::write(&output, "// shipped by the package").expect("write the binding");
        // Adoption records provenance rather than pretending the generator
        // wrote it, so every later build adopts again instead of falling silent.
        write(&stamp_file, &stamp().adopted()).expect("write the adopted stamp");
        assert_eq!(freshness(&stamp_file, &output, &stamp()), Freshness::Adopt);
        // Even a moved input does not regenerate over the package's own source.
        let mut moved = stamp();
        moved.inputs = vec!["121 18 /pkg/NativeLibs/Text/kira_text.h".to_owned()];
        assert_eq!(freshness(&stamp_file, &output, &moved), Freshness::Adopt);
    }

    #[test]
    fn text_that_is_not_a_stamp_reads_as_no_stamp() {
        assert_eq!(Stamp::parse("kira-autobind 99\nkey\n"), None);
        assert_eq!(Stamp::parse(""), None);
    }

    #[test]
    fn a_binding_with_no_stamp_is_adopted_and_one_with_no_binding_is_stale() {
        let dir = TempDir::new("freshness");
        let output = dir.0.join("text.kira");
        let stamp_file = dir.0.join("text.stamp");

        assert_eq!(
            freshness(&stamp_file, &output, &stamp()),
            Freshness::Stale,
            "a binding that does not exist has to be generated"
        );

        std::fs::write(&output, "// generated").expect("write the binding");
        assert_eq!(
            freshness(&stamp_file, &output, &stamp()),
            Freshness::Adopt,
            "a binding nobody stamped is the package's own source"
        );

        write(&stamp_file, &stamp()).expect("write the stamp");
        assert_eq!(
            freshness(&stamp_file, &output, &stamp()),
            Freshness::Current
        );

        let mut moved = stamp();
        moved.inputs = vec!["121 18 /pkg/NativeLibs/Text/kira_text.h".to_owned()];
        assert_eq!(freshness(&stamp_file, &output, &moved), Freshness::Stale);
    }

    #[test]
    fn a_missing_input_is_described_rather_than_skipped() {
        assert!(describe_input(Path::new("/definitely/not/here.h")).starts_with("absent "));
    }
}
