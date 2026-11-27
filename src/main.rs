mod dev_model;
mod ollama_server;

use cognix::cli::Args;
use cognix::{
    Cancellation,
    agent::Agent,
    commands,
    provider::Ollama,
    review::Reviewer,
    runtime::{ApprovalMode, Runtime},
    terminal::{input, render, show, submitted_input},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
    time::Duration,
};
use tokio::signal::unix::{SignalKind, signal};

#[tokio::main]
async fn main() {
    let args = Args::try_parse_configured(std::env::args_os()).unwrap_or_else(|e| e.exit());
    std::process::exit(run(args).await);
}
async fn run(args: Args) -> i32 {
    let cancel = Cancellation::default();
    let status = Arc::new(AtomicI32::new(0));
    if let Err(e) = watch_signals(cancel.clone(), status.clone()) {
        show(&format!("Cannot install cancellation handlers: {e}"));
        return 1;
    }
    let code = match session(args, &cancel).await {
        Ok(code) => code,
        Err(e) => {
            show(&format!("Error: {e}"));
            1
        }
    };
    match status.load(Ordering::SeqCst) {
        0 => code,
        signal_status => signal_status,
    }
}

fn watch_signals(cancel: Cancellation, status: Arc<AtomicI32>) -> std::io::Result<()> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        let received = tokio::select! {
            _ = interrupt.recv() => libc::SIGINT,
            _ = terminate.recv() => libc::SIGTERM,
            _ = hangup.recv() => libc::SIGHUP,
        };
        status.store(128 + received, Ordering::SeqCst);
        cancel.cancel();
    });
    Ok(())
}

fn startup_timeout(dev: bool) -> Result<Duration, String> {
    let override_value = dev
        .then(|| std::env::var_os("COGNIX_DEV_STARTUP_TIMEOUT"))
        .flatten()
        .filter(|value| !value.is_empty());
    if let Some(value) = override_value {
        return value
            .to_str()
            .filter(|value| !value.starts_with('0') && value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| (1..=300).contains(seconds))
            .map(Duration::from_secs)
            .ok_or_else(|| "COGNIX_DEV_STARTUP_TIMEOUT must be 1–300 seconds".into());
    }
    Ok(Duration::from_secs(30))
}

async fn session(args: Args, cancel: &Cancellation) -> Result<i32, String> {
    if args.task.as_ref().is_some_and(|s| s.trim().is_empty()) {
        return Err("--task must contain a request".into());
    }
    let allowed = args
        .allow_command
        .iter()
        .filter(|_| args.approval_mode != ApprovalMode::FullAccess)
        .map(|s| commands::parse_command(s))
        .collect::<Result<Vec<_>, _>>()?;
    let mut runtime = Runtime::new(&args.root, Box::new(|_, _| false), cancel.clone())?
        .with_cancellable_approval(Box::new(|kind, preview, action_cancel| {
            show(&format!("\nProposed {kind}:\n{preview}"));
            submitted_input("Approve this action? [y/N] ", action_cancel)
                .ok()
                .flatten()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("y"))
        }));
    runtime.allowed_commands = allowed;
    runtime.approval_mode = args.approval_mode;
    runtime.sandbox_mode = args.sandbox_mode;
    runtime.denied_network_origins = args.deny_network.clone();
    if args.approval_mode == ApprovalMode::FullAccess {
        show(
            "WARNING: Full access is enabled. Commands can read and change files outside the project, use the network and inherit your environment without approval.",
        );
    }
    runtime.timeout = Duration::from_secs(args.command_timeout as u64);
    runtime.output_limit = args.output_limit;
    let model_timeout = Duration::from_secs(args.model_timeout as u64);
    let initial_version_timeout = if args.dev_provision_model {
        Duration::from_secs(1)
    } else {
        model_timeout
    };
    let mut provider = Ollama::new(&args.model, &args.ollama_url, model_timeout, args.num_ctx)
        .map_err(|e| e.to_string())?;
    let (version, owned_server) = ollama_server::ensure_ready(
        provider.endpoint(),
        initial_version_timeout,
        startup_timeout(args.dev_provision_model)?,
        cancel,
    )
    .await
    .map_err(|e| e.to_string())?;
    provider.version = version;
    if args.dev_provision_model {
        if owned_server.is_none() {
            dev_model::report(&format!("Reusing Ollama at {}", provider.endpoint()));
        }
        let result = tokio::select! { biased;
            _ = cancel.wait() => Err("Model provisioning canceled. Ollama-managed partial data was left intact".into()),
            result = dev_model::provision(&args, provider.endpoint(), cancel) => result,
        };
        if let Err(error) = result {
            dev_model::report(&error);
            return Ok(1);
        }
    }
    show(&format!(
        "Project: {}\nChecking local model {} and tool calling...",
        runtime.root.display(),
        args.model
    ));
    provider
        .validate_model_and_tools(cancel)
        .await
        .map_err(|e| e.to_string())?;
    show(&format!(
        "Ollama {}; model {}. Ctrl-C cancels. /exit quits.",
        provider.version, args.model
    ));
    if args.approval_mode == ApprovalMode::Auto {
        let model = args.reviewer_model();
        let review_provider = Ollama::new(
            model,
            &args.ollama_url,
            model_timeout,
            args.num_ctx.min(32768),
        );
        let review_provider = match review_provider {
            Ok(mut provider) => match provider.validate_local_model(false, cancel).await {
                Ok(()) => Ok(provider),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        match review_provider {
            Ok(provider) => {
                show(&format!("Automatic review model: {model}"));
                runtime.reviewer = Some(Reviewer::new(provider));
            }
            Err(error) => show(&format!(
                "Automatic reviewer {model:?} unavailable. Actions requiring approval will ask for confirmation. {error}"
            )),
        }
    }
    let mut agent = Agent::new(provider, runtime);
    agent.max_steps = args.max_steps;
    agent.context_budget = args.num_ctx;
    agent.activity = Box::new(show);
    loop {
        if cancel.is_canceled() {
            show(&render(&agent.report(
                "canceled",
                Some("Interrupted".into()),
                "",
            )));
            return Ok(130);
        }
        let task = match &args.task {
            Some(t) => t.clone(),
            None => match input("\n> ", cancel) {
                Ok(Some(s)) => s.trim().to_owned(),
                Ok(None) => return Ok(0),
                Err(e) => {
                    let canceled = cancel.is_canceled();
                    show(&render(&agent.report(
                        if canceled { "canceled" } else { "error" },
                        Some(e),
                        "",
                    )));
                    return Ok(if canceled { 130 } else { 1 });
                }
            },
        };
        if args.task.is_none() && task == "/exit" {
            return Ok(0);
        }
        if task.is_empty() {
            continue;
        }
        let report = agent.run(&task).await;
        show(&render(&report));
        if report["status"] != "final_answer" {
            return Ok(if report["status"] == "canceled" {
                130
            } else {
                1
            });
        }
        if args.task.is_some() {
            let failed = agent.runtime.events.iter().any(|e| {
                let result = &e["result"];
                !matches!(result["status"].as_str(), Some("ok" | "passed"))
                    || result["network_requests"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|request| {
                            matches!(
                                request["status"].as_str(),
                                Some("denied" | "not_run" | "failed")
                            )
                        })
            });
            return Ok(if failed { 1 } else { 0 });
        }
    }
}
