//! A fixed HTTP(S) GET transport. Project processes never inherit its sockets.
use reqwest::Url;
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) const CURL: &str = "/usr/bin/curl";
pub(crate) const MAX_BROKER_BODY: usize = 4 * 1024 * 1024;
const MAX_BROKER_HEADERS: usize = 64 * 1024;
// Curl appends a newline and the decimal body length.
pub(crate) const MAX_BROKER_OUTPUT: usize = MAX_BROKER_BODY + MAX_BROKER_HEADERS + 24;

pub(crate) struct Fetch(Url);

pub(crate) struct BrokerResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub location: Option<Fetch>,
}

fn valid_url_characters(url: &str) -> bool {
    !url.chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
}

impl Fetch {
    pub(crate) fn from_argv(argv: &[String]) -> Result<Option<Self>, String> {
        // Only this literal form requests network access. Other clients and curl
        // options retain the default-deny subprocess sandbox.
        match argv {
            [curl, url] if matches!(curl.as_str(), "curl" | CURL) && !url.starts_with('-') => {
                Self::parse(url).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn parse(url: &str) -> Result<Self, String> {
        if !valid_url_characters(url) {
            return Err("Network URL contains whitespace, controls or backslashes".into());
        }
        let scheme = url.split_once("://").ok_or("Use an absolute HTTP(S) URL")?;
        if !matches!(scheme.0, "http" | "https")
            || scheme.1.starts_with('/')
            || scheme
                .1
                .split(['/', '?', '#'])
                .next()
                .unwrap_or_default()
                .contains('@')
        {
            return Err("Use an absolute HTTP(S) URL without credentials".into());
        }
        let url = Url::parse(url).map_err(|_| "Invalid network URL")?;
        if url.fragment().is_some()
            || url.port_or_known_default() == Some(0)
            || url.domain().is_some_and(|host| host.ends_with('.'))
        {
            return Err(
                "Use a network URL without a fragment, port zero or a trailing dot in the host"
                    .into(),
            );
        }
        Ok(Self(url))
    }

    pub(crate) fn origin(&self) -> String {
        self.0.origin().ascii_serialization()
    }

    pub(crate) fn url(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn scope(&self) -> Value {
        json!({"url":self.0.as_str(),"protocol":self.0.scheme(),"host":self.0.host_str(),
               "port":self.0.port_or_known_default(),"method":"GET","redirects":false})
    }

    pub(crate) fn args(&self) -> Vec<String> {
        // Use only Cognix-owned flags. --disable must come first. libcurl is
        // prohibited from using proxies, expanding URLs, or following redirects.
        let protocol = format!("={}", self.0.scheme());
        [
            "--disable",
            "--noproxy",
            "*",
            "--globoff",
            "--proto",
            &protocol,
            "--max-redirs",
            "0",
            "--silent",
            "--show-error",
            "--include",
            "--fail",
            "--url",
            self.0.as_str(),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    pub(crate) fn broker_args(&self) -> Vec<String> {
        let mut args = self.args();
        args.retain(|arg| arg != "--fail");
        args.extend(["--write-out".into(), "\\n%{size_download}".into()]);
        args
    }

    pub(crate) fn broker_response(&self, mut bytes: Vec<u8>) -> Result<BrokerResponse, String> {
        let malformed = "Invalid upstream response";
        let footer = bytes.iter().rposition(|&b| b == b'\n').ok_or(malformed)?;
        let body_size = std::str::from_utf8(&bytes[footer + 1..])
            .ok()
            .and_then(|text| text.parse::<usize>().ok())
            .filter(|&size| size <= MAX_BROKER_BODY && size <= footer)
            .ok_or(malformed)?;
        // With --include, curl can write informational headers before the final
        // headers and chunked trailers after the body. Only relay the body bytes
        // and consult the final response block for Location.
        let extra = footer - body_size;
        if extra > MAX_BROKER_HEADERS {
            return Err("Upstream response exceeded its header limit".into());
        }
        let mut headers = &bytes[..extra];
        let (status, location) = loop {
            let end = headers
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .ok_or(malformed)?;
            let mut lines = headers[..end].split(|&b| b == b'\n');
            let status = lines
                .next()
                .and_then(|line| line.strip_prefix(b"HTTP/"))
                .and_then(|line| line.split(|&b| b == b' ').nth(1))
                .and_then(|code| std::str::from_utf8(code).ok())
                .and_then(|code| code.trim_end_matches('\r').parse::<u16>().ok())
                .filter(|code| (100..600).contains(code))
                .ok_or(malformed)?;
            headers = &headers[end + 4..];
            if status >= 200 {
                let location = if matches!(status, 300 | 301 | 302 | 303 | 307 | 308) {
                    self.redirect_location(lines)
                } else {
                    None
                };
                break (status, location);
            }
        };
        let body_start = extra - headers.len();
        bytes.truncate(body_start + body_size);
        Ok(BrokerResponse {
            status,
            body: bytes.split_off(body_start),
            location,
        })
    }

    fn redirect_location<'a>(&self, lines: impl Iterator<Item = &'a [u8]>) -> Option<Self> {
        let mut location = None;
        let mut previous_was_location = false;
        for line in lines {
            if previous_was_location && matches!(line.first(), Some(b' ' | b'\t')) {
                return None;
            }
            let mut parts = line.splitn(2, |&b| b == b':');
            previous_was_location = parts.next()?.eq_ignore_ascii_case(b"location");
            if previous_was_location {
                if location.is_some() {
                    return None;
                }
                let value = parts.next()?;
                location =
                    Some(std::str::from_utf8(value.strip_suffix(b"\r").unwrap_or(value)).ok()?);
            }
        }
        let location = location?.trim_matches([' ', '\t']);
        if location.is_empty() || !valid_url_characters(location) {
            return None;
        }
        if location.starts_with("//") {
            Self::parse(&format!("{}:{location}", self.0.scheme())).ok()
        } else if location.split(['/', '?', '#']).next()?.contains(':') {
            Self::parse(location).ok()
        } else {
            Self::parse(self.0.join(location).ok()?.as_str()).ok()
        }
    }
}

pub(crate) fn deny_origin(origin: &str) -> Result<String, String> {
    let argv = ["curl".into(), origin.into()];
    let fetch = Fetch::from_argv(&argv)?.ok_or("Expected a denied HTTP(S) origin")?;
    if fetch.0.path() != "/" || fetch.0.query().is_some() {
        return Err("deny_network entries must be HTTP(S) origins without paths or queries".into());
    }
    Ok(fetch.origin())
}

const MAX_REQUEST_BYTES: usize = 8192;
const MAX_REQUESTS: usize = 16;
const REQUEST_LIMIT_REJECTION: (u16, &str) = (429, "Too many requests for this action");

/// Private per-command HTTP GET endpoint. HTTP headers and bodies supplied by
/// the project are never forwarded. No CONNECT, redirect following or tunneling.
pub(crate) struct Broker {
    listener: UnixListener,
    socket: PathBuf,
    _directory: tempfile::TempDir,
    pending: Option<(Request, Vec<u8>, Instant)>,
    remaining: usize,
    rejected_over_budget: bool,
}

pub(crate) struct Request(UnixStream);

impl Request {
    pub(crate) fn respond(mut self, status: u16, body: &[u8], location: Option<&Fetch>) {
        // Keep a child that stops reading from holding the broker open.
        if self.0.set_nonblocking(false).is_ok()
            && self
                .0
                .set_write_timeout(Some(Duration::from_millis(100)))
                .is_ok()
        {
            let location = location.map_or_else(String::new, |target| {
                format!("Location: {}\r\n", target.url())
            });
            let _ = write!(
                self.0,
                "HTTP/1.1 {status} Broker response\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .and_then(|()| self.0.write_all(body));
        }
    }
}

impl Broker {
    pub(crate) fn new() -> io::Result<Self> {
        // Keep the endpoint outside the writable project and command scratch.
        // /private/tmp also leaves space within Darwin's sockaddr_un limit.
        let directory = tempfile::Builder::new()
            .prefix("cognix-http-")
            .tempdir_in("/private/tmp")?;
        let socket = directory.path().join("get");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            socket,
            _directory: directory,
            pending: None,
            remaining: MAX_REQUESTS,
            rejected_over_budget: false,
        })
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn request_limit_rejection(&self) -> Option<(u16, &'static str)> {
        self.rejected_over_budget.then_some(REQUEST_LIMIT_REJECTION)
    }

    pub(crate) fn next(&mut self) -> Option<(Request, Fetch)> {
        if self.pending.is_none() {
            let (stream, _) = self.listener.accept().ok()?;
            stream.set_nonblocking(true).ok()?;
            let request = Request(stream);
            if self.remaining == 0 {
                self.rejected_over_budget = true;
                let (status, error) = REQUEST_LIMIT_REJECTION;
                request.respond(status, error.as_bytes(), None);
                return None;
            }
            self.remaining -= 1;
            self.pending = Some((request, Vec::new(), Instant::now()));
        }
        let (mut request, mut bytes, started) = self.pending.take()?;
        let mut buffer = [0; 4096];
        match request.0.read(&mut buffer) {
            Ok(0) => return None,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => return None,
        }
        if bytes.len() > MAX_REQUEST_BYTES || started.elapsed() >= Duration::from_secs(1) {
            request.respond(400, b"Incomplete or oversized broker request", None);
            return None;
        }
        if !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
            self.pending = Some((request, bytes, started));
            return None;
        }
        let fetch = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|header| header.split_once("\r\n"))
            .and_then(|(line, _)| line.strip_prefix("GET "))
            .and_then(|target| target.strip_suffix(" HTTP/1.1"))
            .and_then(|url| Fetch::parse(url).ok());
        match fetch {
            Some(fetch) => Some((request, fetch)),
            None => {
                request.respond(400, b"Use an absolute HTTP(S) URL in a GET request", None);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_only_exposes_a_single_valid_redirect_location() {
        let fetch = Fetch::parse("https://example.test/artifacts/v1/start").unwrap();
        for (headers, expected) in [
            (
                "Location: ../target?asset=42",
                Some("https://example.test/artifacts/target?asset=42"),
            ),
            (
                "Location: //other.test/path",
                Some("https://other.test/path"),
            ),
            ("Location: file:///tmp/private", None),
            ("Location: //user@other.test/path", None),
            ("Location: /target#fragment", None),
            ("Location: /target\r\nLocation: /duplicate", None),
            ("Location: /target\r\n\t/folded", None),
        ] {
            let response = format!("HTTP/1.1 302 Found\r\n{headers}\r\n\r\nbody\n4");
            let reply = fetch.broker_response(response.into_bytes()).unwrap();
            assert_eq!(reply.status, 302);
            assert_eq!(reply.body, b"body");
            assert_eq!(
                reply.location.as_ref().map(Fetch::url),
                expected,
                "{headers}"
            );
        }
    }
}
