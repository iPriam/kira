//! The local development server behind `kira run --device wasm32`, and opening
//! the browser at it.
//!
//! A Kira wasm module cannot be run by opening a file: `file://` pages have a
//! null origin, so `fetch` of the module beside them is refused, and
//! `instantiateStreaming` needs the module served as `application/wasm`
//! regardless. So `run` serves rather than opens — which is also what makes the
//! command mean the same thing on the Web as it does natively: it runs the
//! program.
//!
//! Hand-rolled on `std::net`, like the rest of the CLI is hand-rolled: this
//! serves two files from one directory to one browser on loopback, and that is
//! not worth a dependency.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Why serving could not start or continue.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The loopback listener could not be bound.
    #[error("cannot serve on {address}: {source}")]
    Bind {
        /// The address that was tried.
        address: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The bound port could not be read back.
    #[error("cannot read the served address: {0}")]
    Address(#[source] std::io::Error),
}

/// A local static file server rooted at one directory.
pub struct Server {
    listener: TcpListener,
    root: PathBuf,
    address: SocketAddr,
}

impl Server {
    /// Binds a server on loopback, letting the OS pick a free port.
    ///
    /// An OS-assigned port is the point: a fixed one collides with whatever is
    /// already on it, and two `kira run`s at once must not fight.
    pub fn bind(root: PathBuf) -> Result<Self, ServeError> {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|source| ServeError::Bind {
                address: "127.0.0.1:0".to_owned(),
                source,
            })?;
        let address = listener.local_addr().map_err(ServeError::Address)?;
        Ok(Self {
            listener,
            root,
            address,
        })
    }

    /// The URL the served page is at.
    pub fn url(&self) -> String {
        format!("http://{}/", self.address)
    }

    /// Serves until the process is interrupted.
    ///
    /// One connection at a time: a browser loading a page, a module, and
    /// nothing else does not need concurrency, and a sequential loop cannot
    /// leave a half-served file behind.
    pub fn serve_forever(&self) -> Result<(), ServeError> {
        for stream in self.listener.incoming() {
            // One bad connection must not end the run — not a failed accept
            // either. A transient refusal (the process is briefly out of file
            // descriptors) would otherwise tear down a server whose whole job
            // is to still be there when the browser asks again.
            let Ok(mut stream) = stream else {
                continue;
            };
            let _ = self.respond(&mut stream);
        }
        Ok(())
    }

    /// Answers one request.
    fn respond(&self, stream: &mut TcpStream) -> std::io::Result<()> {
        let Some(target) = read_target(stream)? else {
            return write_response(stream, "400 Bad Request", "text/plain", b"bad request");
        };

        let Some(path) = self.resolve(&target) else {
            return write_response(stream, "404 Not Found", "text/plain", b"not found");
        };

        match std::fs::read(&path) {
            Ok(body) => write_response(stream, "200 OK", content_type(&path), &body),
            Err(_) => write_response(stream, "404 Not Found", "text/plain", b"not found"),
        }
    }

    /// Resolves a request target to a file under the root, or `None`.
    ///
    /// Only plain names resolve: anything with a parent or root component is
    /// refused rather than normalized, so no request can name a file outside
    /// the directory being served.
    fn resolve(&self, target: &str) -> Option<PathBuf> {
        let trimmed = target.split(['?', '#']).next().unwrap_or(target);
        let relative = trimmed.strip_prefix('/')?;
        if relative.is_empty() {
            return Some(self.root.join("index.html"));
        }

        let candidate = Path::new(relative);
        if candidate
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return None;
        }
        Some(self.root.join(candidate))
    }
}

/// The longest request line that will be read.
///
/// A request line is a method, a path, and a version. Anything longer is not a
/// browser asking for a file, and reading it to its end would let whatever is
/// on the other end decide how much memory this process uses.
const MAX_REQUEST_LINE: u64 = 8 * 1024;

/// Reads the request target from the request line, ignoring the headers.
fn read_target(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut reader = BufReader::new(stream.try_clone()?.take(MAX_REQUEST_LINE));
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    // A line that filled the cap without ending is not a request being
    // truncated into a smaller one — it is refused.
    if !line.ends_with('\n') {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next();
    let target = parts.next();
    Ok(match (method, target) {
        (Some("GET"), Some(target)) => Some(target.to_owned()),
        _ => None,
    })
}

/// Writes one response.
fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// The content type for a path.
///
/// `application/wasm` is not decoration: `instantiateStreaming` refuses a module
/// served as anything else, so a wrong type here is a page that does not run.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Opens `url` in the default browser.
///
/// Best-effort by design: a machine with no browser — a container, a CI runner,
/// an SSH session — still has a served URL, and the caller prints it. Failing
/// the run because a browser did not open would be refusing to do the thing that
/// worked.
pub fn open_browser(url: &str) -> bool {
    let (program, leading) = if cfg!(target_os = "macos") {
        ("open", &[][..])
    } else if cfg!(target_os = "windows") {
        ("cmd", &["/C", "start", ""][..])
    } else {
        ("xdg-open", &[][..])
    };

    Command::new(program)
        .args(leading)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        Server::bind(PathBuf::from("/tmp/kira-serve-test")).expect("binds on loopback")
    }

    #[test]
    fn the_root_serves_the_page() {
        let server = server();
        assert_eq!(
            server.resolve("/"),
            Some(PathBuf::from("/tmp/kira-serve-test/index.html"))
        );
    }

    #[test]
    fn a_query_string_is_not_part_of_the_file_name() {
        let server = server();
        assert_eq!(
            server.resolve("/demo.wasm?v=2"),
            Some(PathBuf::from("/tmp/kira-serve-test/demo.wasm"))
        );
    }

    #[test]
    fn nothing_outside_the_served_directory_resolves() {
        let server = server();
        for escape in [
            "/../../etc/passwd",
            "/nested/../../etc/passwd",
            "//etc/passwd",
            "/./../secret",
        ] {
            assert_eq!(server.resolve(escape), None, "{escape} escaped the root");
        }
    }

    #[test]
    fn the_wasm_module_is_served_as_wasm() {
        // instantiateStreaming rejects any other type, so this is the
        // difference between a page that runs and one that does not.
        assert_eq!(content_type(Path::new("a.wasm")), "application/wasm");
        assert_eq!(
            content_type(Path::new("a.html")),
            "text/html; charset=utf-8"
        );
        // Anything else is not something this server hands a browser.
        assert_eq!(
            content_type(Path::new("a.kira")),
            "application/octet-stream"
        );
    }

    #[test]
    fn the_url_names_a_loopback_port_the_os_chose() {
        let server = server();
        assert!(server.url().starts_with("http://127.0.0.1:"));
        assert!(!server.url().ends_with(":0/"));
    }
}
