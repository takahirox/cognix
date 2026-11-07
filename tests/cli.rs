mod common;
use cognix::terminal;
use common::{Reply, Server, ready};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn command(root: &std::path::Path, url: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_cognix"));
    c.args([
        "--root",
        root.to_str().unwrap(),
        "--ollama-url",
        url,
        "--model",
        "fixture:local",
    ]);
    c
}
fn run(c: &mut Command, input: &str) -> std::process::Output {
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
fn tool(name: &str, args: Value) -> Reply {
    Reply::json(
        json!({"done":true,"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":name,"arguments":args}}]}}),
    )
}
fn final_answer() -> Reply {
    Reply::json(
        json!({"done":true,"message":{"role":"assistant","content":"All tests passed!\u{1b}[31m"}}),
    )
}
fn finished(child: &mut Child) -> std::process::ExitStatus {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if Instant::now() > end {
            child.kill().unwrap();
            panic!("CLI did not terminate");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn interactive_task_approval_and_exit() {
    let server = Server::new(|p, b| {
        if p != "/api/chat" || b["messages"].as_array().unwrap().len() == 1 {
            return ready(p, b);
        }
        if b["messages"].as_array().unwrap().len() == 2 {
            tool("run_command", json!({"argv":["/usr/bin/true"]}))
        } else {
            final_answer()
        }
    });
    let t = tempfile::tempdir().unwrap();
    let out = run(
        command(t.path(), &server.url).args(["--allow-command", "[\"/usr/bin/true\"]"]),
        "verify\ny\n/exit\n",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("Proposed command"));
    assert!(s.contains("Approve this action?"));
    assert!(s.contains("Tool: run_command"));
    assert!(s.contains("executed=true"));
    assert!(s.contains("not verification evidence"));
    assert!(!s.contains('\u{1b}'));
    assert!(s.contains("\\x1b"));
}
#[test]
fn eof_denies_and_invalid_options_fail() {
    let server = Server::new(|p, b| {
        if p == "/api/chat" && b["messages"].as_array().unwrap().len() > 1 {
            if b["messages"].as_array().unwrap().len() == 2 {
                tool("run_command", json!({"argv":["/usr/bin/true"]}))
            } else {
                final_answer()
            }
        } else {
            ready(p, b)
        }
    });
    let t = tempfile::tempdir().unwrap();
    let out = run(
        command(t.path(), &server.url).args([
            "--task",
            "verify",
            "--allow-command",
            "[\"/usr/bin/true\"]",
        ]),
        "",
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("denied"));
    for opts in [
        vec!["--max-steps", "0"],
        vec!["--num-ctx", "8191"],
        vec!["--task", " "],
        vec!["--root", "/missing-cognix-root"],
        vec!["--allow-command", "[\"relative\"]"],
        vec!["--command-timeout", "no"],
        vec!["--unknown"],
    ] {
        assert!(
            !run(command(t.path(), &server.url).args(opts), "")
                .status
                .success()
        );
    }
}
#[test]
fn configurable_default_and_single_task() {
    let server = Server::new(|p, b| {
        if p == "/api/chat" && b["messages"].as_array().unwrap().len() > 1 {
            final_answer()
        } else {
            ready(p, b)
        }
    });
    let t = tempfile::tempdir().unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_cognix"));
    c.args(["--root", t.path().to_str().unwrap(), "--task", "inspect"])
        .env("COGNIX_MODEL", "deepseek:fixture")
        .env("COGNIX_OLLAMA_URL", &server.url);
    let out = run(&mut c, "");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Verification: not run"));
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["path"] == "/api/chat")
            .all(|r| r["body"]["model"] == "deepseek:fixture")
    );
    assert!(
        Command::new(env!("CARGO_BIN_EXE_cognix"))
            .arg("--help")
            .current_dir(t.path())
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn terminal_safety_and_all_evidence_statuses() {
    assert_eq!(
        terminal::text("é\u{202e}\u{200b}\\\"'"),
        "é\\x202e\\x200b\\\"'"
    );
    for status in [
        "passed",
        "failed",
        "not_run",
        "denied",
        "canceled",
        "limit_reached",
    ] {
        let event = json!({"tool":"run_command","arguments":{"argv":["/usr/bin/true"]},"result":{"status":status,"executed":status=="passed" || status=="failed","output":"\u{1b}[31m","exit_code":if status=="passed"{0}else{1}}});
        let report = json!({"status":"final_answer","error":null,"model_reply":"All tests passed!","modified_files":[],"checks":[event],"events":[event]});
        let rendered = terminal::render(&report);
        assert!(rendered.contains(status));
        assert!(rendered.contains("not verification evidence"));
        assert!(rendered.contains("Recorded execution evidence"));
        assert!(!rendered.contains('\u{1b}'));
    }
}
#[test]
fn sigint_during_input_and_model_wait() {
    for during_model in [false, true] {
        let reached = Arc::new(Mutex::new(false));
        let flag = reached.clone();
        let server = Server::new(move |p, b| {
            if p == "/api/chat" && b["messages"].as_array().unwrap().len() > 1 {
                *flag.lock().unwrap() = true;
                let mut r = final_answer();
                r.delay = Duration::from_millis(350);
                r
            } else {
                ready(p, b)
            }
        });
        let t = tempfile::tempdir().unwrap();
        let mut c = command(t.path(), &server.url);
        if during_model {
            c.args(["--task", "inspect"]);
        }
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            let requests = server.requests.lock().unwrap().len();
            if (during_model && *reached.lock().unwrap()) || (!during_model && requests >= 3) {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        // Allow the startup reply to finish before signaling the interactive prompt.
        if !during_model {
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        assert_eq!(finished(&mut child).code(), Some(130));
    }
}
#[cfg(target_os = "macos")]
#[test]
fn sigint_after_patch_and_during_command_preserves_evidence() {
    for command_running in [false, true] {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("file"), "before").unwrap();
        let argv = json!([
            "/opt/homebrew/bin/python3",
            "-c",
            "import time;print('launched',flush=True);time.sleep(20)"
        ]);
        let stage = Arc::new(Mutex::new(0usize));
        let state = stage.clone();
        let allow = argv.clone();
        let server = Server::new(move |p, b| {
            if p != "/api/chat" || b["messages"].as_array().unwrap().len() == 1 {
                return ready(p, b);
            }
            let n = *state.lock().unwrap();
            *state.lock().unwrap() += 1;
            match n {
                0 => tool("read_file", json!({"path":"file"})),
                1 => {
                    let last = b["messages"].as_array().unwrap().last().unwrap();
                    let read: Value =
                        serde_json::from_str(last["content"].as_str().unwrap()).unwrap();
                    tool(
                        "apply_patch",
                        json!({"path":"file","sha256":read["sha256"],"old_text":"before","new_text":"after"}),
                    )
                }
                _ if command_running => tool("run_command", json!({"argv":allow})),
                _ => {
                    let mut r = final_answer();
                    r.delay = Duration::from_millis(400);
                    r
                }
            }
        });
        let mut child = command(t.path(), &server.url)
            .args(["--task", "fix", "--allow-command", &argv.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(b"y\ny\n").unwrap();
        let end = Instant::now() + Duration::from_secs(4);
        loop {
            if *stage.lock().unwrap() >= 3 {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(10));
        }
        if command_running {
            std::thread::sleep(Duration::from_millis(150));
        }
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        assert_eq!(finished(&mut child).code(), Some(130));
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        assert_eq!(fs::read_to_string(t.path().join("file")).unwrap(), "after");
        assert!(output.contains("Modified files: [\"file\"]"), "{output}");
        if command_running {
            assert!(output.contains("executed=true"), "{output}");
            assert!(output.contains("canceled"));
            assert!(output.contains("exit=-9"));
        }
    }
}
