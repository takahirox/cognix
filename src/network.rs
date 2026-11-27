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

pub(crate) struct Fetch(Url);

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
        if url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        {
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
        args.retain(|arg| arg != "--include" && arg != "--fail");
        args.extend(["--write-out".into(), "\\n%{http_code}".into()]);
        args
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

/// Private per-command HTTP GET endpoint. HTTP headers and bodies supplied by
/// the project are never forwarded. No CONNECT, redirects or general tunneling.
pub(crate) struct Broker {
    listener: UnixListener,
    socket: PathBuf,
    _directory: tempfile::TempDir,
    pending: Option<(Request, Vec<u8>, Instant)>,
    remaining: usize,
}

pub(crate) struct Request(UnixStream);

impl Request {
    pub(crate) fn respond(mut self, status: u16, body: &[u8]) {
        // Keep a child that stops reading from holding the broker open.
        if self.0.set_nonblocking(false).is_ok()
            && self
                .0
                .set_write_timeout(Some(Duration::from_millis(100)))
                .is_ok()
        {
            let _ = write!(
                self.0,
                "HTTP/1.1 {status} Broker response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ).and_then(|()| self.0.write_all(body));
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
        })
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn next(&mut self) -> Option<(Request, Fetch)> {
        if self.pending.is_none() {
            let (stream, _) = self.listener.accept().ok()?;
            stream.set_nonblocking(true).ok()?;
            let request = Request(stream);
            if self.remaining == 0 {
                request.respond(429, b"Too many requests for this action");
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
            request.respond(400, b"Incomplete or oversized broker request");
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
                request.respond(400, b"Use an absolute HTTP(S) URL in a GET request");
                None
            }
        }
    }
}
