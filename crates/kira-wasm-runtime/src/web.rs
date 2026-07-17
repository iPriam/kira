//! The Web surface a built module is served behind.
//!
//! Generated here rather than shipped as a checked-in asset: the page is part
//! of the Web target's output, and it has to agree with the module's import
//! names and memory layout, which this crate owns.
//!
//! The host's whole job is the two imports. `print` reads a string the module
//! already built out of the module's own memory and shows it; `trap` does the
//! same and stops. Neither formats anything — that is why the page and the
//! terminal show the same bytes the VM would.

/// The name the entrypoint is exported under.
pub const MAIN_EXPORT: &str = "kira_main";

/// Decodes a string out of the module's memory and drives the two imports.
///
/// A Kira string is a 4-byte little-endian length then its UTF-8 bytes, and
/// `print` is handed the address of the bytes with the length alongside — so
/// the host never has to know the header exists.
fn host_js() -> String {
    r#"const decoder = new TextDecoder();

// The module's memory can be replaced by a grow, so the view is taken per call
// rather than cached.
function readText(instance, pointer, length) {
  const memory = new Uint8Array(instance.exports.memory.buffer);
  return decoder.decode(memory.subarray(Number(pointer), Number(pointer) + length));
}

// Inlined rather than imported: the page has no second file to fetch.
function makeImports(getInstance, onLine, onTrap) {
  return {
    kira: {
      print(pointer, length) {
        onLine(readText(getInstance(), pointer, length));
      },
      trap(pointer, length) {
        onTrap(readText(getInstance(), pointer, length));
      },
    },
  };
}
"#
    .to_owned()
}

/// Escapes text for use inside HTML or a JavaScript string literal.
///
/// The inputs are Kira's own — a source file's stem and the module's file name —
/// but "our own content" is not an argument for splicing raw text into a page.
/// A file called `</script>.kira` would otherwise generate a page that breaks,
/// and the difference between that and one that runs is not worth trusting to a
/// naming convention.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// The page served for a `wasm32`/`wasm64` run: fetches the module, runs it,
/// and shows what it printed.
pub fn page(module_name: &str, wasm_file: &str) -> String {
    let module_name = &escape(module_name);
    let wasm_file = &escape(wasm_file);
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>{module_name} — Kira</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{
    margin: 0;
    padding: 2rem;
    font: 14px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace;
  }}
  h1 {{ font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.08em; opacity: 0.6; margin: 0 0 1rem; font-weight: 600; }}
  #output {{ white-space: pre-wrap; margin: 0; }}
  .trap {{ color: #b3261e; }}
  @media (prefers-color-scheme: dark) {{ .trap {{ color: #f2b8b5; }} }}
</style>
<h1>{module_name}</h1>
<pre id="output"></pre>
<script type="module">
{shared}
const output = document.getElementById("output");
function write(text, className) {{
  const line = document.createElement("span");
  if (className) line.className = className;
  line.textContent = text + "\n";
  output.appendChild(line);
}}

let instance = null;
const imports = makeImports(
  () => instance,
  (text) => write(text),
  (text) => write("kirac: runtime trap: " + text, "trap"),
);

const source = await WebAssembly.instantiateStreaming(fetch("{wasm_file}"), imports);
instance = source.instance;
try {{
  instance.exports.{main}();
}} catch (error) {{
  // A trap already said why; `unreachable` is how the module stops afterwards.
  if (!String(error).includes("unreachable")) throw error;
}}
</script>
"#,
        module_name = module_name,
        wasm_file = wasm_file,
        shared = host_js(),
        main = MAIN_EXPORT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_names_the_module_it_fetches_and_the_export_it_calls() {
        let html = page("demo", "demo.wasm");
        assert!(html.contains(r#"fetch("demo.wasm")"#));
        assert!(html.contains(&format!("exports.{MAIN_EXPORT}()")));
        assert!(html.contains("<title>demo — Kira</title>"));
    }

    #[test]
    fn the_host_supplies_exactly_the_two_imports() {
        // Two, and no more: anything else the page decided would be something
        // a Kira program's behaviour depended on the browser for.
        let host = page("demo", "demo.wasm");
        assert!(host.contains("kira: {"));
        assert!(host.contains("print(pointer, length)"));
        assert!(host.contains("trap(pointer, length)"));
    }

    #[test]
    fn a_name_cannot_break_out_of_the_page_it_is_written_into() {
        let html = page("</script><script>stolen()</script>", "a\".wasm");
        assert!(!html.contains("<script>stolen()"));
        assert!(html.contains("&lt;/script&gt;"));
        assert!(html.contains(r#"fetch("a&quot;.wasm")"#));
    }

    #[test]
    fn the_page_reports_a_trap_the_way_the_cli_does() {
        // The exact prefix the CLI prints, so a trapping program reads the
        // same on the Web as on the host.
        assert!(page("demo", "demo.wasm").contains(r#""kirac: runtime trap: ""#));
    }
}
