//! Shared argument contract for Cognix and the development launcher helper.
use crate::{
    action::{ApprovalMode, SandboxMode},
    network::deny_origin,
    provider::{normalize_ollama_url, validate_model},
};
use clap::{CommandFactory, FromArgMatches, Parser, error::ErrorKind, parser::ValueSource};
use serde::Deserialize;
use sha2::{Digest, Sha256};
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
    review_model: Option<String>,
    approval_mode: Option<ApprovalMode>,
    sandbox_mode: Option<SandboxMode>,
    deny_network: Option<Vec<String>>,
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
        let mut config: Self =
            toml::from_str(&contents).map_err(|e| error(&crate::terminal::text(&e.to_string())))?;
        if let Some(model) = &config.model {
            validate_model(model).map_err(|e| error(&format!("model: {e}")))?;
        }
        if let Some(model) = &config.review_model {
            validate_model(model).map_err(|e| error(&format!("review_model: {e}")))?;
        }
        if let Some(origins) = &mut config.deny_network {
            for origin in origins {
                *origin = deny_origin(origin).map_err(|e| error(&format!("deny_network: {e}")))?;
            }
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
    /// Local reviewer model in auto mode. Defaults to the effective working model.
    #[arg(long, value_parser = |s: &str| validate_model(s).map(|()| s.to_owned()))]
    pub review_model: Option<String>,
    /// Approval mechanism selected for this session.
    #[arg(long, value_enum, default_value = "manual")]
    pub approval_mode: ApprovalMode,
    /// Filesystem isolation used in manual and auto modes.
    #[arg(long, value_enum, default_value = "workspace-write")]
    pub sandbox_mode: SandboxMode,
    #[arg(skip)]
    pub deny_network: Vec<String>,
    #[arg(
        long,
        env = "COGNIX_OLLAMA_URL",
        default_value = "http://127.0.0.1:11434"
    )]
    pub ollama_url: String,
    #[arg(long)]
    pub task: Option<String>,
    /// Restrict command proposals to these exact argv arrays in manual or auto mode.
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
        args.review_model = args.review_model.or(config.review_model);
        if use_config("approval_mode") {
            args.approval_mode = config.approval_mode.unwrap_or(args.approval_mode);
        }
        if use_config("sandbox_mode") {
            args.sandbox_mode = config.sandbox_mode.unwrap_or(args.sandbox_mode);
        }
        args.deny_network = config.deny_network.unwrap_or_default();
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
        if std::env::var_os("COGNIX_DEV_SETTINGS_DIGEST")
            .is_some_and(|expected| expected != args.launcher_settings_digest().as_str())
        {
            return Err(clap::Error::raw(
                ErrorKind::ValueValidation,
                "Cognix settings changed during development launcher startup. Run scripts/dev.sh again.",
            ));
        }
        Ok(args)
    }

    pub fn reviewer_model(&self) -> &str {
        self.review_model.as_deref().unwrap_or(&self.model)
    }

    #[doc(hidden)]
    pub fn launcher_settings_digest(&self) -> String {
        // Only these effective settings can change when the same argv and
        // environment are parsed again after the user configuration was edited.
        let settings = serde_json::json!([
            self.model,
            self.reviewer_model(),
            self.approval_mode,
            self.sandbox_mode,
            self.deny_network,
            self.ollama_url,
            self.max_steps,
            self.num_ctx,
            self.model_timeout,
            self.command_timeout,
            self.output_limit,
        ]);
        format!("{:x}", Sha256::digest(settings.to_string()))
    }
}
