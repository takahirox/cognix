//! Shared argument contract for Cognix and the development launcher helper.
use crate::provider::{normalize_ollama_url, validate_model};
use clap::{CommandFactory, FromArgMatches, Parser, error::ErrorKind, parser::ValueSource};
use serde::Deserialize;
use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

fn parse_number(s: &str) -> Result<usize, String> {
    s.parse().map_err(|_| "must be a positive integer".into())
}
fn positive(n: usize) -> Result<usize, String> {
    (n > 0)
        .then_some(n)
        .ok_or("must be greater than zero".into())
}
fn seconds(n: usize) -> Result<usize, String> {
    positive(n)?;
    (n <= 86400)
        .then_some(n)
        .ok_or("must be at most 86400 seconds".into())
}
fn context(n: usize) -> Result<usize, String> {
    (n >= 8192)
        .then_some(n)
        .ok_or("must be at least 8192".into())
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    model: Option<String>,
    ollama_url: Option<String>,
    max_steps: Option<usize>,
    num_ctx: Option<usize>,
    model_timeout: Option<usize>,
    command_timeout: Option<usize>,
    output_limit: Option<usize>,
}

impl Config {
    fn load() -> Result<Self, clap::Error> {
        let Some(home) = std::env::home_dir().filter(|home| home.is_absolute()) else {
            return Ok(Self::default());
        };
        let path = home.join(".cognix/config.toml");
        let error = |detail: &str| {
            clap::Error::raw(
                ErrorKind::ValueValidation,
                format!(
                    "Configuration {}: {detail}",
                    crate::terminal::text(&path.to_string_lossy())
                ),
            )
        };
        // A FIFO must open without waiting for a writer so we can check its type.
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(error(&e.to_string())),
        };
        if !file
            .metadata()
            .map_err(|e| error(&e.to_string()))?
            .is_file()
        {
            return Err(error("expected a regular file"));
        }
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|e| error(&e.to_string()))?;
        let config: Self =
            toml::from_str(&contents).map_err(|e| error(&crate::terminal::text(&e.to_string())))?;
        if let Some(model) = &config.model {
            validate_model(model).map_err(|e| error(&format!("model: {e}")))?;
        }
        if let Some(url) = &config.ollama_url {
            normalize_ollama_url(url).map_err(|e| error(&format!("ollama_url: {e}")))?;
        }
        for (key, value, validate) in [
            ("max_steps", config.max_steps, positive as fn(usize) -> _),
            ("num_ctx", config.num_ctx, context),
            ("model_timeout", config.model_timeout, seconds),
            ("command_timeout", config.command_timeout, seconds),
            ("output_limit", config.output_limit, positive),
        ] {
            if let Some(value) = value {
                validate(value).map_err(|e| error(&format!("{key} {e}")))?;
            }
        }
        Ok(config)
    }
}

#[derive(Parser)]
#[command(
    version,
    about = "Cognix: bounded local edit-and-verify agent. Ctrl-C cancels; /exit quits."
)]
pub struct Args {
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
    #[arg(long, env = "COGNIX_MODEL", default_value = "qwen3-coder:30b")]
    pub model: String,
    #[arg(
        long,
        env = "COGNIX_OLLAMA_URL",
        default_value = "http://127.0.0.1:11434"
    )]
    pub ollama_url: String,
    #[arg(long)]
    pub task: Option<String>,
    /// Restrict command proposals to these exact argv arrays. Approval is still required.
    #[arg(long, value_name = "JSON_ARGV")]
    pub allow_command: Vec<String>,
    #[arg(long, default_value = "16", value_parser = |s: &str| parse_number(s).and_then(positive))]
    pub max_steps: usize,
    #[arg(long, default_value = "16384", value_parser = |s: &str| parse_number(s).and_then(context))]
    pub num_ctx: usize,
    #[arg(long, default_value = "120", value_parser = |s: &str| parse_number(s).and_then(seconds))]
    pub model_timeout: usize,
    #[arg(long, default_value = "30", value_parser = |s: &str| parse_number(s).and_then(seconds))]
    pub command_timeout: usize,
    #[arg(long, default_value = "16384", value_parser = |s: &str| parse_number(s).and_then(positive))]
    pub output_limit: usize,
}

impl Args {
    pub fn try_parse_configured(
        args: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, clap::Error> {
        // Parse first so --help and --version work even with a broken config.
        let matches = Self::command().try_get_matches_from(args)?;
        let mut args = Self::from_arg_matches(&matches)?;
        let config = Config::load()?;
        let use_config = |key| matches.value_source(key) == Some(ValueSource::DefaultValue);
        if use_config("model") {
            args.model = config.model.unwrap_or(args.model);
        }
        if use_config("ollama_url") {
            args.ollama_url = config.ollama_url.unwrap_or(args.ollama_url);
        }
        if use_config("max_steps") {
            args.max_steps = config.max_steps.unwrap_or(args.max_steps);
        }
        if use_config("num_ctx") {
            args.num_ctx = config.num_ctx.unwrap_or(args.num_ctx);
        }
        if use_config("model_timeout") {
            args.model_timeout = config.model_timeout.unwrap_or(args.model_timeout);
        }
        if use_config("command_timeout") {
            args.command_timeout = config.command_timeout.unwrap_or(args.command_timeout);
        }
        if use_config("output_limit") {
            args.output_limit = config.output_limit.unwrap_or(args.output_limit);
        }
        Ok(args)
    }
}
