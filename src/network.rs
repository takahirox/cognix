//! A fixed HTTP(S) GET transport. Project processes never inherit its sockets.
use reqwest::Url;
use serde_json::{Value, json};

pub(crate) const CURL: &str = "/usr/bin/curl";

pub(crate) struct Fetch(Url);

impl Fetch {
    pub(crate) fn from_argv(argv: &[String]) -> Result<Option<Self>, String> {
        // Only this literal form requests network access. Other clients and curl
        // options retain the default-deny subprocess sandbox.
        match argv {
            [curl, url] if matches!(curl.as_str(), "curl" | CURL) && !url.starts_with('-') => {
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
                    return Err("Use a network URL without a fragment, port zero or a trailing dot in the host".into());
                }
                Ok(Some(Self(url)))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn origin(&self) -> String {
        self.0.origin().ascii_serialization()
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
}

pub(crate) fn deny_origin(origin: &str) -> Result<String, String> {
    let argv = ["curl".into(), origin.into()];
    let fetch = Fetch::from_argv(&argv)?.ok_or("Expected a denied HTTP(S) origin")?;
    if fetch.0.path() != "/" || fetch.0.query().is_some() {
        return Err("deny_network entries must be HTTP(S) origins without paths or queries".into());
    }
    Ok(fetch.origin())
}
