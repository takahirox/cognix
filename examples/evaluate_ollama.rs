//! Fixture-only evaluator; never exposes automatic approvals in the product CLI.
use clap::Parser;
use cognix::{
    Cancellation,
    agent::Agent,
    commands,
    provider::Ollama,
    runtime::{Runtime, hash},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    model: String,
    #[arg(long, default_value = "/opt/homebrew/bin/python3")]
    python: String,
    #[arg(long, default_value_t = 3)]
    runs: usize,
    #[arg(long)]
    output: PathBuf,
}

fn verified_division(events: &[Value], check_argv: &Value) -> bool {
    let committed_edit =
        |event: &Value| event["tool"] == "apply_patch" && event["result"]["executed"] == true;
    let Some(first_edit) = events.iter().position(committed_edit) else {
        return false;
    };
    let last_edit = events.iter().rposition(committed_edit).unwrap();
    let mut checks = events.iter().enumerate().filter(|(_, event)| {
        event["tool"] == "run_command" && event["arguments"]["argv"] == *check_argv
    });
    let Some((baseline_index, baseline)) = checks.next() else {
        return false;
    };
    let Some((final_index, final_check)) = checks.next_back() else {
        return false;
    };
    let baseline = &baseline["result"];
    let final_check = &final_check["result"];
    baseline_index < first_edit
        && last_edit < final_index
        && baseline["executed"] == true
        && baseline["status"] == "failed"
        && baseline["exit_code"] == 1
        && baseline["output"]
            .as_str()
            .is_some_and(|output| output.contains("ZeroDivisionError"))
        && final_check["executed"] == true
        && final_check["status"] == "passed"
        && final_check["exit_code"] == 0
        && final_check["output"]
            .as_str()
            .is_some_and(|output| output.contains("Ran 3 tests"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.runs == 0 {
        return Err("--runs must be positive".into());
    }
    let check = commands::parse_command(
        &json!([
            args.python,
            "-I",
            "-B",
            "-m",
            "unittest",
            "discover",
            "-s",
            ".",
            "-p",
            "test_*.py"
        ])
        .to_string(),
    )?;
    let allowed = [
        vec!["ls".into()],
        vec!["cat".into(), "main.py".into()],
        vec!["cat".into(), "test_main.py".into()],
        check.clone(),
    ];
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = project.join("tests/fixtures/division");
    let mut sources = serde_json::Map::new();
    for dir in ["src", "examples"] {
        for entry in fs::read_dir(project.join(dir))? {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e == "rs") {
                sources.insert(
                    format!("{dir}/{}", p.file_name().unwrap().to_string_lossy()),
                    json!(hash(&fs::read(p)?)),
                );
            }
        }
    }
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "tests/fixtures/division/main.py",
        "tests/fixtures/division/test_main.py",
    ] {
        sources.insert(path.into(), json!(hash(&fs::read(project.join(path))?)));
    }
    let hardware = |key: &str| {
        String::from_utf8_lossy(
            &std::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", key])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_owned()
    };
    let cancel = Cancellation::default();
    let signal = cancel.clone();
    ctrlc::set_handler(move || signal.cancel())?;
    let mut evidence = json!({"implementation":"Rust","model":args.model,"fixture":"tests/fixtures/division","command":check,"source_sha256":sources,"cpu":hardware("machdep.cpu.brand_string"),"memory_bytes":hardware("hw.memsize"),"date_unix":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),"attempts":[]});
    for attempt in 1..=args.runs {
        if cancel.is_canceled() {
            break;
        }
        let temp = tempfile::Builder::new()
            .prefix("cognix-rust-trial-")
            .tempdir()?;
        for file in ["main.py", "test_main.py"] {
            fs::copy(fixture.join(file), temp.path().join(file))?;
        }
        let approvals = Rc::new(RefCell::new(Vec::new()));
        let decisions = approvals.clone();
        let mut runtime = Runtime::new(
            temp.path(),
            Box::new(move |kind, preview| {
                let approved = kind == "command"
                    || (kind == "patch"
                        && (preview.starts_with("--- main.py\n+++ main.py\n")
                            || preview.starts_with("Create \"test_division_edges.py\" (")));
                decisions
                    .borrow_mut()
                    .push(json!({"kind":kind,"preview":preview,"approved":approved}));
                approved
            }),
            cancel.clone(),
        )?;
        runtime.allowed_commands = allowed.to_vec();
        let mut provider = Ollama::new(
            &args.model,
            "http://127.0.0.1:11434",
            Duration::from_secs(120),
            32768,
        )?;
        let started = Instant::now();
        let result = provider.validate(&cancel).await;
        evidence["ollama_version"] = json!(provider.version);
        let record = match result {
            Err(e) => json!({"success":false,"error":e.to_string()}),
            Ok(()) => {
                evidence["model_metadata"] = provider
                    .request("/api/show", Some(json!({"model":args.model})), &cancel)
                    .await?;
                let mut agent = Agent::new(provider, runtime);
                agent.context_budget = 32768;
                agent.activity = Box::new(move |s| println!("Trial {attempt}: {s}"));
                let report = agent.run("List the project with ls and read main.py and test_main.py with cat. Run the allowed unittest command before any edits to reproduce the error. Fix divide(a, 0) to return None and preserve the existing tests. Create test_division_edges.py with one additional unittest that asserts divide(6, -2) == -3. Rerun the unittest command and report the actual test results.").await;
                let success = report["status"] == "final_answer"
                    && report["modified_files"] == json!(["main.py", "test_division_edges.py"])
                    && verified_division(report["events"].as_array().unwrap(), &json!(check))
                    && fs::read(temp.path().join("test_main.py"))?
                        == fs::read(fixture.join("test_main.py"))?;
                json!({"success":success,"report":report,"messages":agent.messages})
            }
        };
        let mut record = record;
        record["attempt"] = json!(attempt);
        record["seconds"] = json!(started.elapsed().as_secs_f64());
        record["approvals"] = json!(*approvals.borrow());
        println!("Trial {attempt}: success={}", record["success"]);
        evidence["attempts"].as_array_mut().unwrap().push(record);
        let attempts = evidence["attempts"].as_array().unwrap();
        let successes = attempts.iter().filter(|a| a["success"] == true).count();
        let failures = attempts.len() - successes;
        evidence["successes"] = json!(successes);
        evidence["failures"] = json!(failures);
        fs::write(
            &args.output,
            format!("{}\n", serde_json::to_string_pretty(&evidence)?),
        )?;
    }
    if evidence["failures"] != 0 || evidence["attempts"].as_array().unwrap().len() != args.runs {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unittest_evidence_must_surround_committed_edits() {
        let argv = json!(["python3", "-m", "unittest"]);
        let check = |status, exit_code, output| {
            json!({"tool":"run_command","arguments":{"argv":argv},
                   "result":{"executed":true,"status":status,"exit_code":exit_code,"output":output}})
        };
        let baseline = check("failed", 1, "ZeroDivisionError");
        let passing = check("passed", 0, "Ran 3 tests\nOK");
        let edit = json!({"tool":"apply_patch","result":{"status":"ok","executed":true}});
        let denied = json!({"tool":"apply_patch","result":{"status":"denied","executed":false}});

        assert!(verified_division(
            &[
                denied.clone(),
                baseline.clone(),
                edit.clone(),
                edit.clone(),
                passing.clone(),
                denied
            ],
            &argv,
        ));
        assert!(!verified_division(
            &[
                edit.clone(),
                baseline.clone(),
                edit.clone(),
                passing.clone()
            ],
            &argv,
        ));
        assert!(!verified_division(
            &[baseline, edit.clone(), passing, edit],
            &argv,
        ));
    }
}
