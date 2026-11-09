use clap::Parser;
use cognix::cli::Args;
use cognix::{
    Cancellation,
    agent::Agent,
    commands,
    provider::Ollama,
    runtime::Runtime,
    terminal::{input, render, show, submitted_input},
};
use std::time::Duration;

#[tokio::main]
async fn main() {
    std::process::exit(run(Args::parse()).await);
}
async fn run(args: Args) -> i32 {
    let cancel = Cancellation::default();
    let signal = cancel.clone();
    if let Err(e) = ctrlc::set_handler(move || signal.cancel()) {
        show(&format!("Cannot install cancellation handler: {e}"));
        return 1;
    }
    let result = session(args, &cancel).await;
    match result {
        Ok(code) => code,
        Err(e) => {
            show(&format!("Error: {e}"));
            if cancel.is_canceled() { 130 } else { 1 }
        }
    }
}

async fn session(args: Args, cancel: &Cancellation) -> Result<i32, String> {
    if args.task.as_ref().is_some_and(|s| s.trim().is_empty()) {
        return Err("--task must contain a request".into());
    }
    if args.num_ctx < 8192 {
        return Err("--num-ctx must be at least 8192".into());
    }
    let allowed = args
        .allow_command
        .iter()
        .map(|s| commands::parse_command(s))
        .collect::<Result<Vec<_>, _>>()?;
    let approval_cancel = cancel.clone();
    let mut runtime = Runtime::new(
        &args.root,
        Box::new(move |kind, preview| {
            show(&format!("\nProposed {kind}:\n{preview}"));
            submitted_input("Approve this action? [y/N] ", &approval_cancel)
                .ok()
                .flatten()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("y"))
        }),
        cancel.clone(),
    )?;
    runtime.allowed_commands = allowed;
    runtime.timeout = Duration::from_secs(args.command_timeout as u64);
    runtime.output_limit = args.output_limit;
    let mut provider = Ollama::new(
        &args.model,
        &args.ollama_url,
        Duration::from_secs(args.model_timeout as u64),
        args.num_ctx,
    )
    .map_err(|e| e.to_string())?;
    show(&format!(
        "Project: {}\nChecking local model {} and tool calling...",
        runtime.root.display(),
        args.model
    ));
    provider.validate(cancel).await.map_err(|e| e.to_string())?;
    show(&format!(
        "Ollama {}; model {}. Ctrl-C cancels. /exit quits.",
        provider.version, args.model
    ));
    if runtime.allowed_commands.is_empty() {
        show(
            "No verification commands allowed. Restart with --allow-command JSON_ARGV to enable checks.",
        );
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
            return Ok(
                if agent
                    .runtime
                    .events
                    .iter()
                    .any(|e| !matches!(e["result"]["status"].as_str(), Some("ok" | "passed")))
                {
                    1
                } else {
                    0
                },
            );
        }
    }
}
