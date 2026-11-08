//! Internal development-launcher helper; the core agent never provisions models.
use clap::{Parser, error::ErrorKind};
use cognix::{Cancellation, cli::Args, provider::Ollama, terminal};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use std::{
    io::IsTerminal,
    time::{Duration, Instant},
};

const RESPONSE_LIMIT: usize = 1024 * 1024;
const LINE_LIMIT: usize = 16 * 1024;

fn report(message: &str) {
    eprintln!("dev.sh: {}", terminal::text(message));
}

// A local Ollama name, optionally namespaced and tagged. Reject source selectors,
// URLs, traversal, control characters and shell syntax before contacting Ollama.
fn model_key(model: &str) -> Option<String> {
    fn part(value: &str, limit: usize, extra: &[u8]) -> bool {
        !value.is_empty()
            && value.len() <= limit
            && value
                .as_bytes()
                .first()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c) || extra.contains(&c))
    }
    if model.to_ascii_lowercase().contains("cloud") {
        return None;
    }
    let components: Vec<_> = model.split('/').collect();
    let (host, namespace, name) = match components.as_slice() {
        [name] => ("registry.ollama.ai", "library", *name),
        [namespace, name] => ("registry.ollama.ai", *namespace, *name),
        [host, namespace, name] => (*host, *namespace, *name),
        _ => return None,
    };
    let (name, tag) = name.split_once(':').unwrap_or((name, "latest"));
    (part(name, 80, b".")
        && part(tag, 80, b".")
        && part(namespace, 80, b"")
        && part(host, 350, b".:"))
    .then(|| format!("{host}/{namespace}/{name}:{tag}").to_ascii_lowercase())
}

async fn bounded_json(mut response: Response) -> Result<Value, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > RESPONSE_LIMIT {
            return Err("Ollama response exceeds 1 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Ollama returned invalid JSON".into())
}

async fn installed(client: &Client, endpoint: &str, model: &str) -> Result<bool, String> {
    let response = client
        .post(format!("{endpoint}/api/show"))
        .json(&json!({"model":model}))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Model lookup failed: {e}"))?;
    let status = response.status();
    let value = bounded_json(response).await?;
    // Only Ollama's specific missing-model response permits provisioning. An
    // arbitrary 404 (e.g. a proxy's missing route) is not proof of absence.
    let missing_name = value["error"]
        .as_str()
        .and_then(|s| s.strip_prefix("model '")?.strip_suffix("' not found"));
    if status == StatusCode::NOT_FOUND
        && value.as_object().is_some_and(|object| object.len() == 1)
        && missing_name
            .is_some_and(|name| model_key(name).is_some() && model_key(name) == model_key(model))
    {
        return Ok(false);
    }
    if status != StatusCode::OK || value.get("error").is_some() {
        return Err(format!("Model lookup HTTP {status}: {}", detail(&value)));
    }
    for key in ["remote_host", "remote_model"] {
        if value.get(key).is_some_and(|v| !v.is_null() && v != "") {
            return Err("Cloud-backed Ollama models are unsupported".into());
        }
    }
    if !value["model_info"].is_object() {
        return Err("Ollama returned incompatible model metadata".into());
    }
    Ok(true)
}

fn detail(value: &Value) -> String {
    value["error"]
        .as_str()
        .unwrap_or("unexpected API response")
        .chars()
        .take(500)
        .collect()
}

async fn pull(client: &Client, endpoint: &str, model: &str) -> Result<(), String> {
    let mut response = client
        .post(format!("{endpoint}/api/pull"))
        .json(&json!({"model":model,"stream":true}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status() != StatusCode::OK {
        let status = response.status();
        return Err(format!(
            "Pull HTTP {status}: {}",
            detail(&bounded_json(response).await?)
        ));
    }
    let mut line = Vec::new();
    let mut success = false;
    let mut last_output = Instant::now() - Duration::from_secs(1);
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        for byte in chunk {
            if byte != b'\n' {
                if line.len() == LINE_LIMIT {
                    return Err("Ollama pull progress line exceeds 16 KiB".into());
                }
                line.push(byte);
                continue;
            }
            let value: Value = serde_json::from_slice(&line)
                .map_err(|_| "Ollama returned invalid pull progress")?;
            line.clear();
            if value.get("error").is_some() {
                return Err(detail(&value));
            }
            let status = value["status"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("Ollama returned incompatible pull progress")?;
            if success {
                return Err("Ollama sent progress after its final success response".into());
            }
            success = status == "success";
            // At most one update per second, plus the final success, without
            // accumulating the stream or printing untrusted raw responses.
            if success || last_output.elapsed() >= Duration::from_secs(1) {
                let status: String = status.chars().take(160).collect();
                if let (Some(done), Some(total)) =
                    (value["completed"].as_u64(), value["total"].as_u64())
                {
                    report(&format!("{status}: {done}/{total} bytes"));
                } else {
                    report(&status);
                }
                last_output = Instant::now();
            }
        }
    }
    if !line.is_empty() || !success {
        return Err("Ollama pull ended without a complete success response".into());
    }
    Ok(())
}

async fn provision(args: &Args, cancel: &Cancellation) -> Result<(), String> {
    // Reuse the core provider's URL/model policy without changing its behavior.
    Ollama::new(
        &args.model,
        &args.ollama_url,
        Duration::from_secs(10),
        args.num_ctx,
    )
    .map_err(|e| e.to_string())?;
    if model_key(&args.model).is_none() {
        return Err("Select a valid local Ollama model name (optional namespace and :tag); cloud-backed tags are unsupported".into());
    }
    let seconds = std::env::var("COGNIX_DEV_PULL_TIMEOUT")
        .unwrap_or_else(|_| "3600".into())
        .parse::<u64>()
        .ok()
        .filter(|s| (1..=86400).contains(s))
        .ok_or("COGNIX_DEV_PULL_TIMEOUT must be 1–86400 seconds")?;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let endpoint = args.ollama_url.trim_end_matches('/');
    if installed(&client, endpoint, &args.model).await? {
        return Ok(());
    }
    report(&format!(
        "Model {} is missing at {endpoint}. Downloading may transfer many GB and use substantial network bandwidth and disk space.",
        args.model
    ));
    let manual = format!(
        "No ready model; Cognix was not started. Install manually with: OLLAMA_HOST='{endpoint}' ollama pull '{}'",
        args.model
    );
    if !std::io::stdin().is_terminal() {
        return Err(format!("Non-interactive input; no download. {manual}"));
    }
    let answer = terminal::submitted_input(&format!("Download {}? [y/N] ", args.model), cancel)?;
    if !answer.is_some_and(|a| matches!(a.trim().to_ascii_lowercase().as_str(), "y" | "yes")) {
        return Err(format!("Download declined or EOF; no download. {manual}"));
    }
    report(&format!(
        "Pulling {} at {endpoint} (timeout {seconds}s; Ctrl-C cancels)...",
        args.model
    ));
    tokio::time::timeout(Duration::from_secs(seconds), pull(&client, endpoint, &args.model))
        .await.map_err(|_| format!("Download timed out after {seconds}s; increase COGNIX_DEV_PULL_TIMEOUT if needed"))?
        .map_err(|e| format!("Download failed: {e}. Check network access and free disk space, then retry. Ollama-managed partial data was left intact."))?;
    if !installed(&client, endpoint, &args.model).await? {
        return Err("Download reported success but the selected model is still missing; Cognix was not started".into());
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let args = match Args::try_parse() {
        Ok(args) => args,
        // Let the actual Cognix invocation print help/version exactly once.
        Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => return,
        Err(e) => e.exit(),
    };
    let cancel = Cancellation::default();
    let signal = cancel.clone();
    if let Err(e) = ctrlc::set_handler(move || signal.cancel()) {
        report(&format!("Cannot install cancellation handler: {e}"));
        std::process::exit(1);
    }
    let result = tokio::select! { biased;
        _ = cancel.wait() => Err("Model provisioning canceled; Cognix was not started. Ollama-managed partial data was left intact".into()),
        result = provision(&args, &cancel) => result,
    };
    if let Err(e) = result {
        report(&e);
        std::process::exit(if cancel.is_canceled() { 130 } else { 1 });
    }
}
