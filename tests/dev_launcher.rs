#![cfg(target_os = "macos")]
mod common;

use common::{Reply, Server, ready};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::{fd::FromRawFd, unix::fs::PermissionsExt},
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

// Native disposable child: unlike background shell scripts, it can restore
// SIGINT handling. It also gives the Ollama stub a real descendant to clean up.
#[test]
fn launcher_child() {
    let Ok(mode) = std::env::var("LAUNCHER_CHILD") else {
        return;
    };
    extern "C" fn interrupted(_: i32) {
        unsafe { libc::_exit(130) }
    }
    unsafe { libc::signal(libc::SIGINT, interrupted as *const () as usize) };
    let dir = std::env::var("FIXTURE").unwrap();
    fs::write(format!("{dir}/{mode}.pid"), std::process::id().to_string()).unwrap();
    if mode == "ollama" {
        let mut worker = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "launcher_child", "--nocapture"])
            .env("LAUNCHER_CHILD", "worker")
            .spawn()
            .unwrap();
        let _ = worker.wait();
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

struct Fixture {
    dir: tempfile::TempDir,
}
fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [
            "repo with spaces/scripts",
            "repo with spaces/target/debug",
            "bin",
            "logs",
            "cwd",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::write(root.join("repo with spaces/Cargo.toml"), "fixture").unwrap();
        executable(
            &root.join("repo with spaces/scripts/dev.sh"),
            include_str!("../scripts/dev.sh"),
        );
        executable(
            &root.join("bin/cargo"),
            r#"#!/bin/bash
printf '%s\0' "$@" > "$FIXTURE/cargo-argv"
pwd > "$FIXTURE/cargo-cwd"
if [[ ${BUILD_WAIT:-0} == 1 ]]; then
    export LAUNCHER_CHILD=build
    exec "$HELPER_EXE" --exact launcher_child --nocapture
fi
exit "${BUILD_EXIT:-0}"
"#,
        );
        executable(
            &root.join("repo with spaces/target/debug/cognix-dev-model"),
            r#"#!/bin/bash
if [[ ${PROVISION_WAIT:-0} == 1 ]]; then
    export LAUNCHER_CHILD=provision
    exec "$HELPER_EXE" --exact launcher_child --nocapture
fi
exit "${PROVISION_EXIT:-0}"
"#,
        );
        executable(
            &root.join("repo with spaces/target/debug/cognix"),
            r#"#!/bin/bash
printf '%s\0' "$@" > "$FIXTURE/argv"
pwd > "$FIXTURE/cwd-record"
printf '%s' "${COGNIX_MODEL:-}" > "$FIXTURE/model"
if [[ ${COGNIX_WAIT:-0} == 1 ]]; then
    export LAUNCHER_CHILD=cognix
    exec "$HELPER_EXE" --exact launcher_child --nocapture
fi
if [[ ${READ_STDIN:-0} == 1 ]]; then
    IFS= read -r line
    printf '%s' "$line" > "$FIXTURE/stdin-record"
fi
exit "${COGNIX_EXIT:-0}"
"#,
        );
        executable(
            &root.join("bin/curl"),
            r#"#!/bin/bash
printf '%s\0' "$@" >> "$FIXTURE/curl-argv"
while (($#)); do
    if [[ $1 == --output ]]; then output=$2; shift; fi
    shift
done
case ${CURL_MODE:-managed} in
    managed) [[ -f $FIXTURE/ollama.pid ]] || exit 7 ;;
    absent) exit 7 ;;
    invalid) printf '{"version":42}' > "$output"; printf 200; exit ;;
    timeout) exit 28 ;;
esac
printf '{"version":"0.34.4"}' > "$output"
printf 200
"#,
        );
        executable(
            &root.join("bin/ollama"),
            r#"#!/bin/bash
printf '%s\0' "$@" > "$FIXTURE/ollama-argv"
printf '%s' "$OLLAMA_HOST" > "$FIXTURE/ollama-host"
if [[ ${OLLAMA_FAIL:-0} == 1 ]]; then echo 'fixture bind failure'; exit 9; fi
export LAUNCHER_CHILD=ollama
exec "$HELPER_EXE" --exact launcher_child --nocapture
"#,
        );
        Self { dir }
    }
    fn path(&self, path: &str) -> std::path::PathBuf {
        self.dir.path().join(path)
    }
    fn command(&self) -> Command {
        let mut c = Command::new(self.path("repo with spaces/scripts/dev.sh"));
        c.current_dir(self.path("cwd"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("FIXTURE", self.dir.path())
            .env("HELPER_EXE", std::env::current_exe().unwrap())
            .env("TMPDIR", self.path("logs"))
            .env("COGNIX_DEV_STARTUP_TIMEOUT", "2")
            .env_remove("COGNIX_OLLAMA_URL")
            .env_remove("COGNIX_MODEL")
            .env_remove("BASH_ENV")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
    fn argv(&self, file: &str) -> Vec<String> {
        fs::read(self.path(file))
            .unwrap()
            .split(|b| *b == 0)
            .filter(|v| !v.is_empty())
            .map(|v| String::from_utf8(v.to_vec()).unwrap())
            .collect()
    }
    fn wait_for(&self, file: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.path(file).exists() {
            assert!(Instant::now() < deadline, "missing fixture file {file}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn assert_stopped(&self, file: &str) {
        let Ok(pid) = fs::read_to_string(self.path(file)) else {
            panic!("child was never started: {file}");
        };
        let pid: i32 = pid.parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(Instant::now() < deadline, "child {file} ({pid}) leaked");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn native_model_helper(&self) {
        fs::copy(
            env!("CARGO_BIN_EXE_cognix-dev-model"),
            self.path("repo with spaces/target/debug/cognix-dev-model"),
        )
        .unwrap();
    }
    fn native_cognix(&self) {
        self.native_model_helper();
        fs::copy(
            env!("CARGO_BIN_EXE_cognix"),
            self.path("repo with spaces/target/debug/cognix"),
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Test failures must not leave disposable children behind either.
        for file in [
            "ollama.pid",
            "worker.pid",
            "cognix.pid",
            "build.pid",
            "provision.pid",
        ] {
            if let Some(pid) = fs::read_to_string(self.path(file))
                .ok()
                .and_then(|pid| pid.parse::<i32>().ok())
            {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}
fn finished(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("launcher did not finish");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn status(out: &Output, expected: i32) {
    assert_eq!(
        out.status.code(),
        Some(expected),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn managed_startup_forwards_arguments_stdin_environment_and_exit_status() {
    let f = Fixture::new();
    let args = [
        "--root",
        "project with spaces",
        "--model",
        "qwen3.8:27b",
        "--task",
        "quotes ' \" $HOME *\nnext",
        "--allow-command",
        r#"["/bin/echo","two words"]"#,
    ];
    let mut child = f
        .command()
        .args(args)
        .env("COGNIX_EXIT", "23")
        .env("READ_STDIN", "1")
        .env("COGNIX_MODEL", "env:local")
        .env("OLLAMA_HOST", "wrong-address:9999")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"approval input\n")
        .unwrap();
    let out = finished(child);
    status(&out, 23);
    assert_eq!(f.argv("argv"), args);
    assert_eq!(f.argv("ollama-argv"), ["serve"]);
    assert_eq!(
        fs::read_to_string(f.path("ollama-host")).unwrap(),
        "127.0.0.1:11434"
    );
    assert_eq!(fs::read_to_string(f.path("model")).unwrap(), "env:local");
    assert_eq!(
        fs::read_to_string(f.path("stdin-record")).unwrap(),
        "approval input"
    );
    assert_eq!(
        fs::read_to_string(f.path("cwd-record")).unwrap().trim(),
        fs::canonicalize(f.path("cwd")).unwrap().to_str().unwrap()
    );
    let cargo = f.argv("cargo-argv");
    assert_eq!(cargo[0..3], ["build", "--locked", "--manifest-path"]);
    assert_eq!(
        Path::new(&cargo[3]),
        fs::canonicalize(f.path("repo with spaces/Cargo.toml")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(f.path("cargo-cwd")).unwrap().trim(),
        fs::canonicalize(f.path("repo with spaces"))
            .unwrap()
            .to_str()
            .unwrap()
    );
    f.assert_stopped("ollama.pid");
    // Wait until spawned during readiness/build, then verify group cleanup.
    f.assert_stopped("worker.pid");
}

#[test]
fn existing_real_http_server_and_native_cli_work_from_both_directories() {
    let server = Server::new(ready);
    let f = Fixture::new();
    fs::remove_file(f.path("bin/curl")).unwrap();
    f.native_cognix();
    for cwd in ["cwd", "repo with spaces"] {
        let out = finished(
            f.command()
                .current_dir(f.path(cwd))
                .env("COGNIX_OLLAMA_URL", &server.url)
                .args([
                    "--root",
                    f.path("cwd").to_str().unwrap(),
                    "--model",
                    "fixture:local",
                ])
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        status(&out, 0);
        assert!(String::from_utf8_lossy(&out.stdout).contains("Ollama fixture-1"));
    }
    assert!(!f.path("ollama-argv").exists());
    // The existing server still serves a third launch after both sessions exit.
    status(
        &finished(
            f.command()
                .env("COGNIX_OLLAMA_URL", "http://127.0.0.1:1")
                .arg(format!("--ollama-url={}", server.url))
                .arg("--help")
                .spawn()
                .unwrap(),
        ),
        0,
    );
}

#[test]
fn refused_custom_endpoint_missing_ollama_and_invalid_listeners_fail_before_launch() {
    for (mode, endpoint, missing, message) in [
        (
            "absent",
            "http://localhost:11434",
            false,
            "auto-start supports only",
        ),
        (
            "absent",
            "http://127.0.0.1:11434",
            true,
            "ollama command not found",
        ),
        (
            "invalid",
            "http://127.0.0.1:11434",
            false,
            "No valid Ollama",
        ),
        (
            "timeout",
            "http://127.0.0.1:11434",
            false,
            "No valid Ollama",
        ),
        (
            "existing",
            "https://example.com",
            false,
            "local HTTP origin",
        ),
    ] {
        let f = Fixture::new();
        if missing {
            fs::remove_file(f.path("bin/ollama")).unwrap();
        }
        let out = finished(
            f.command()
                .env("CURL_MODE", mode)
                .env("COGNIX_OLLAMA_URL", endpoint)
                .spawn()
                .unwrap(),
        );
        status(&out, 1);
        assert!(String::from_utf8_lossy(&out.stderr).contains(message));
        assert!(!f.path("cargo-argv").exists());
        assert!(!f.path("ollama-argv").exists());
    }
}

#[test]
fn real_http_wrong_service_and_redirect_are_not_reused() {
    for (code, body) in [
        (200, "welcome"),
        (200, r#"{"version"="fake";}"#),
        (200, r#"{"version":1}"#),
        (200, r#"{"version":""}"#),
        (302, r#"{"version":"1"}"#),
    ] {
        let server = Server::new(move |_, _| Reply {
            status: code,
            body: body.into(),
            delay: Duration::ZERO,
        });
        let f = Fixture::new();
        fs::remove_file(f.path("bin/curl")).unwrap();
        let out = finished(
            f.command()
                .args(["--ollama-url", &server.url])
                .spawn()
                .unwrap(),
        );
        status(&out, 1);
        assert!(String::from_utf8_lossy(&out.stderr).contains("No valid Ollama"));
        assert!(!f.path("ollama-argv").exists());
        assert!(!f.path("cargo-argv").exists());
    }
}

#[test]
fn startup_failure_timeout_and_build_failure_report_and_clean_up() {
    for (mode, fail, build_exit, provision_exit, message) in [
        ("managed", "1", "0", "0", "exited before becoming ready"),
        ("absent", "0", "0", "0", "did not become ready within"),
        ("managed", "0", "17", "0", "startup log:"),
        ("managed", "0", "0", "19", "startup log:"),
    ] {
        let f = Fixture::new();
        let out = finished(
            f.command()
                .env("CURL_MODE", mode)
                .env("OLLAMA_FAIL", fail)
                .env("BUILD_EXIT", build_exit)
                .env("PROVISION_EXIT", provision_exit)
                .spawn()
                .unwrap(),
        );
        status(
            &out,
            match (build_exit, provision_exit) {
                ("17", _) => 17,
                (_, "19") => 19,
                _ => 1,
            },
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(message), "{err}");
        let log = err
            .lines()
            .find_map(|line| line.split_once("startup log: ").map(|(_, path)| path))
            .unwrap();
        assert!(Path::new(log).is_file());
        assert!(!f.path("argv").exists());
        if fail == "0" {
            f.assert_stopped("ollama.pid");
            f.assert_stopped("worker.pid");
        }
    }
}

#[test]
fn signals_cancel_cognix_build_and_startup_without_leaking_owned_processes() {
    for (signal, stage, mode) in [
        (libc::SIGINT, "cognix.pid", "managed"),
        (libc::SIGTERM, "cognix.pid", "managed"),
        (libc::SIGHUP, "build.pid", "managed"),
        (libc::SIGTERM, "ollama.pid", "absent"),
        (libc::SIGTERM, "cognix.pid", "existing"),
        (libc::SIGINT, "provision.pid", "managed"),
        (libc::SIGTERM, "provision.pid", "managed"),
    ] {
        let f = Fixture::new();
        let child = f
            .command()
            .env("CURL_MODE", mode)
            .env("COGNIX_WAIT", "1")
            .env(
                "PROVISION_WAIT",
                if stage == "provision.pid" { "1" } else { "0" },
            )
            .env("BUILD_WAIT", if stage == "build.pid" { "1" } else { "0" })
            .spawn()
            .unwrap();
        f.wait_for(stage);
        if mode == "managed" {
            f.wait_for("worker.pid");
        }
        unsafe { libc::kill(child.id() as i32, signal) };
        status(&finished(child), 128 + signal);
        f.assert_stopped(stage);
        if mode == "managed" {
            f.assert_stopped("ollama.pid");
            f.assert_stopped("worker.pid");
        }
        if mode == "existing" {
            assert!(!f.path("ollama-argv").exists());
        }
    }
}

#[test]
fn native_model_errors_pass_through_without_pulls_or_fallbacks() {
    for missing in [true, false] {
        let server = Server::new(move |path, body| {
            if path != "/api/show" {
                return ready(path, body);
            }
            if missing {
                Reply {
                    status: 404,
                    body: json!({"error":"model 'fixture:local' not found"}).to_string(),
                    delay: Duration::ZERO,
                }
            } else {
                Reply::json(json!({"capabilities":["completion"],"model_info":{}}))
            }
        });
        let f = Fixture::new();
        fs::remove_file(f.path("bin/curl")).unwrap();
        f.native_cognix();
        let out = finished(
            f.command()
                .args(["--ollama-url", &server.url, "--model", "fixture:local"])
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        status(&out, 1);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains(if missing {
                "Non-interactive input; no download"
            } else {
                "completion and tools"
            }),
            "{text}"
        );
        assert!(!f.path("ollama-argv").exists());
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| matches!(r["path"].as_str(), Some("/api/version" | "/api/show")))
        );
    }
}

// A disposable terminal exercises the real isatty/consent path. Pipes must never
// serve as an automatic approval mechanism. No controlling host terminal is used.
fn terminal_input(command: &mut Command) -> fs::File {
    let (mut master, mut slave) = (0, 0);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    command.stdin(Stdio::from(slave));
    master
}

fn provisioning_fixture() -> Fixture {
    let f = Fixture::new();
    f.native_model_helper();
    fs::remove_file(f.path("bin/curl")).unwrap();
    f
}

fn missing(model: &str) -> Reply {
    Reply {
        status: 404,
        body: json!({"error":format!("model '{model}' not found")}).to_string(),
        delay: Duration::ZERO,
    }
}

#[test]
fn effective_models_are_checked_exactly_without_prompt_or_changed_arguments() {
    for (args, environment, expected) in [
        (
            vec![
                "--model",
                "namespace/fixture:tag",
                "--task=--model=decoy",
                "--allow-command",
                r#"["/bin/echo","two words"]"#,
            ],
            "env:ignored",
            "namespace/fixture:tag",
        ),
        (
            vec!["--model=fixture:equal", "--task", "quoted ' \" words\nnext"],
            "env:ignored",
            "fixture:equal",
        ),
        (vec![], "env:chosen", "env:chosen"),
        (vec![], "", "qwen3-coder:30b"),
    ] {
        let server = Server::new(ready);
        let f = provisioning_fixture();
        let mut command = f.command();
        command
            .args(&args)
            .env("COGNIX_OLLAMA_URL", &server.url)
            .stdin(Stdio::null());
        if !environment.is_empty() {
            command.env("COGNIX_MODEL", environment);
        }
        let out = finished(command.spawn().unwrap());
        status(&out, 0);
        assert!(!String::from_utf8_lossy(&out.stdout).contains("Download"));
        assert_eq!(f.argv("argv"), args);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2); // version and exact show; never pull
        assert_eq!(requests[1]["body"]["model"], expected);
    }
}

#[test]
fn decline_default_eof_and_noninteractive_never_download_or_start_cognix() {
    for answer in [Some("n\n"), Some("\n"), Some("\x04"), None] {
        let server = Server::new(|path, body| {
            if path == "/api/show" {
                missing(body["model"].as_str().unwrap())
            } else {
                ready(path, body)
            }
        });
        let f = provisioning_fixture();
        let mut command = f.command();
        command.args(["--ollama-url", &server.url, "--model=fixture:missing"]);
        let mut terminal = answer.map(|_| terminal_input(&mut command));
        if answer.is_none() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().unwrap();
        if let (Some(terminal), Some(answer)) = (&mut terminal, answer) {
            terminal.write_all(answer.as_bytes()).unwrap();
        } else {
            // Even an affirmative pipe is non-interactive and cannot consent.
            child.stdin.take().unwrap().write_all(b"yes\n").unwrap();
        }
        let out = finished(child);
        status(&out, 1);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("many GB") && err.contains("disk space"),
            "{err}"
        );
        assert!(
            err.contains(&format!(
                "OLLAMA_HOST='{}' ollama pull 'fixture:missing'",
                server.url
            )),
            "{err}"
        );
        assert!(!f.path("argv").exists());
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r["path"] != "/api/pull")
        );
    }
}

#[test]
fn approved_download_targets_selected_server_verifies_and_reuses_model() {
    let present = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let installed = present.clone();
    let server = Server::new(move |path, body| match path {
        "/api/show" if !installed.load(std::sync::atomic::Ordering::SeqCst) => {
            missing(body["model"].as_str().unwrap())
        }
        "/api/pull" => {
            installed.store(true, std::sync::atomic::Ordering::SeqCst);
            let progress = format!(
                "{}\n",
                json!({"status":"pulling fixture","completed":5,"total":10})
            );
            Reply {
                status: 200,
                body: format!("{}{{\"status\":\"success\"}}\n", progress.repeat(2000)),
                delay: Duration::ZERO,
            }
        }
        _ => ready(path, body),
    });
    let f = provisioning_fixture();
    let args = [
        "--ollama-url",
        &server.url,
        "--model",
        "fixture:chosen",
        "--task",
        "keep ' quotes\nand spaces",
    ];
    let mut command = f.command();
    command
        .args(args)
        .env("COGNIX_OLLAMA_URL", "http://127.0.0.1:1")
        .env("READ_STDIN", "1");
    let mut terminal = terminal_input(&mut command);
    let child = command.spawn().unwrap();
    terminal.write_all(b"YES\nnext approval\n").unwrap();
    let out = finished(child);
    status(&out, 0);
    assert_eq!(f.argv("argv"), args);
    assert_eq!(
        fs::read_to_string(f.path("stdin-record")).unwrap(),
        "next approval"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("5/10 bytes") && err.contains("success"),
        "{err}"
    );
    assert!(err.len() < 3000, "raw progress was not bounded");
    // Cached rerun is non-interactive and neither prompts nor updates the tag.
    status(
        &finished(f.command().args(args).stdin(Stdio::null()).spawn().unwrap()),
        0,
    );
    let requests = server.requests.lock().unwrap();
    let pulls: Vec<_> = requests
        .iter()
        .filter(|r| r["path"] == "/api/pull")
        .collect();
    assert_eq!(pulls.len(), 1);
    assert_eq!(
        pulls[0]["body"],
        json!({"model":"fixture:chosen","stream":true})
    );
    assert!(
        requests
            .iter()
            .filter(|r| r["path"] == "/api/show")
            .all(|r| r["body"]["model"] == "fixture:chosen")
    );
    assert!(!f.path("ollama-argv").exists());
}

#[test]
fn invalid_models_and_cli_are_rejected_before_model_requests() {
    let server = Server::new(ready);
    for model in [
        "",
        "has space",
        "bad\nname",
        "model:cloud",
        "CLOUD-remote",
        "../model",
        "https://remote/model",
        "x@cloud",
        "tag:",
        "-bad",
        "x:one:two",
    ] {
        let f = provisioning_fixture();
        let out = finished(
            f.command()
                .args(["--ollama-url", &server.url, "--model", model])
                .spawn()
                .unwrap(),
        );
        assert!(!out.status.success(), "{model}");
        assert!(!f.path("argv").exists());
    }
    let f = provisioning_fixture();
    status(
        &finished(
            f.command()
                .args(["--ollama-url", &server.url, "--unknown-option"])
                .spawn()
                .unwrap(),
        ),
        2,
    );
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r["path"] == "/api/version")
    );
}

#[test]
fn lookup_errors_and_cloud_metadata_are_never_treated_as_missing() {
    for (code, body) in [
        (401, r#"{"error":"authenticate"}"#),
        (500, r#"{"error":"storage unavailable"}"#),
        (404, r#"{"error":"unknown route"}"#),
        (404, r#"{"error":"model 'fixture:other' not found"}"#),
        (
            404,
            r#"{"error":"model 'fixture:local' not found","remote_host":"https://example.com"}"#,
        ),
        (302, r#"{"error":"redirect"}"#),
        (200, r#"{"error":"not ready"}"#),
        (200, "not JSON"),
        (200, "{}"),
        (
            200,
            r#"{"model_info":{},"remote_host":"https://example.com"}"#,
        ),
        (200, r#"{"model_info":{},"remote_model":"remote"}"#),
    ] {
        let server = Server::new(move |path, data| {
            if path == "/api/show" {
                Reply {
                    status: code,
                    body: body.into(),
                    delay: Duration::ZERO,
                }
            } else {
                ready(path, data)
            }
        });
        let f = provisioning_fixture();
        let mut command = f.command();
        command.args(["--ollama-url", &server.url, "--model=fixture:local"]);
        let mut terminal = terminal_input(&mut command);
        let child = command.spawn().unwrap();
        terminal.write_all(b"y\n").unwrap();
        status(&finished(child), 1);
        assert!(!f.path("argv").exists());
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r["path"] != "/api/pull")
        );
    }
}

#[test]
fn failed_incomplete_and_unverified_downloads_never_start_cognix() {
    for (code, body, delay, message) in [
        (
            200,
            "{\"error\":\"no space left on device\"}\n",
            0,
            "no space left",
        ),
        (
            503,
            "{\"error\":\"network unavailable\"}",
            0,
            "network unavailable",
        ),
        (
            200,
            "{\"status\":\"pulling\"}\n",
            0,
            "without a complete success",
        ),
        (
            200,
            "{\"status\":\"success\"}",
            0,
            "without a complete success",
        ),
        (200, "invalid\n", 0, "invalid pull progress"),
        (200, "{}\n", 0, "incompatible pull progress"),
        (200, "{\"status\":\"success\"}\n", 0, "still missing"),
        (200, "{\"status\":\"success\"}\n", 2, "timed out after 1s"),
    ] {
        let server = Server::new(move |path, data| match path {
            "/api/show" => missing(data["model"].as_str().unwrap()),
            "/api/pull" => Reply {
                status: code,
                body: body.into(),
                delay: Duration::from_secs(delay),
            },
            _ => ready(path, data),
        });
        let f = provisioning_fixture();
        let mut command = f.command();
        command
            .args(["--ollama-url", &server.url, "--model=fixture:local"])
            .env("COGNIX_DEV_PULL_TIMEOUT", "1");
        let mut terminal = terminal_input(&mut command);
        let child = command.spawn().unwrap();
        terminal.write_all(b"y\n").unwrap();
        let out = finished(child);
        status(&out, 1);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!f.path("argv").exists());
    }
}

#[test]
fn real_pull_cancellation_preserves_status_and_reused_server() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let server = Server::new(|path, data| match path {
            "/api/show" => missing(data["model"].as_str().unwrap()),
            "/api/pull" => Reply {
                status: 200,
                body: "{\"status\":\"success\"}\n".into(),
                delay: Duration::from_secs(2),
            },
            _ => ready(path, data),
        });
        let f = provisioning_fixture();
        let mut command = f.command();
        command.args(["--ollama-url", &server.url, "--model=fixture:local"]);
        let mut terminal = terminal_input(&mut command);
        let child = command.spawn().unwrap();
        terminal.write_all(b"y\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "/api/pull")
        {
            assert!(Instant::now() < deadline, "pull did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe { libc::kill(child.id() as i32, signal) };
        status(&finished(child), 128 + signal);
        assert!(!f.path("argv").exists());
        assert!(!f.path("ollama-argv").exists());
    }
}

#[test]
fn downloaded_models_still_undergo_native_capability_and_tool_call_checks() {
    for mode in ["compatible", "no-tools", "bad-probe"] {
        let present = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let installed = present.clone();
        let server = Server::new(move |path, body| match path {
            "/api/show" if !installed.load(std::sync::atomic::Ordering::SeqCst) => {
                missing(body["model"].as_str().unwrap())
            }
            "/api/show" if mode == "no-tools" => {
                Reply::json(json!({"model_info":{},"capabilities":["completion"]}))
            }
            "/api/pull" => {
                installed.store(true, std::sync::atomic::Ordering::SeqCst);
                Reply {
                    status: 200,
                    body: "{\"status\":\"success\"}\n".into(),
                    delay: Duration::ZERO,
                }
            }
            "/api/chat" if mode == "bad-probe" => Reply::json(
                json!({"done":true,"message":{"role":"assistant","content":"no tool call"}}),
            ),
            _ => ready(path, body),
        });
        let f = provisioning_fixture();
        f.native_cognix();
        let mut command = f.command();
        command.args(["--ollama-url", &server.url, "--model=fixture:local"]);
        let mut terminal = terminal_input(&mut command);
        let child = command.spawn().unwrap();
        terminal.write_all(b"y\n\x04").unwrap();
        let out = finished(child);
        status(&out, if mode == "compatible" { 0 } else { 1 });
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(match mode {
                "compatible" => "Ollama fixture-1",
                "no-tools" => "completion and tools",
                _ => "Tool-calling compatibility probe failed",
            }),
            "{stdout}"
        );
        let requests = server.requests.lock().unwrap();
        assert_eq!(
            requests.iter().filter(|r| r["path"] == "/api/pull").count(),
            1
        );
        assert_eq!(
            requests.iter().any(|r| r["path"] == "/api/chat"),
            mode != "no-tools"
        );
    }
}

#[test]
fn failed_model_connection_does_not_trigger_provisioning() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let f = Fixture::new();
    f.native_model_helper();
    // The curl stub reports a ready version; the native model request encounters
    // a real refused connection. It must fail rather than assume a missing tag.
    let out = finished(
        f.command()
            .env("CURL_MODE", "existing")
            .args(["--ollama-url", &endpoint])
            .spawn()
            .unwrap(),
    );
    status(&out, 1);
    assert!(String::from_utf8_lossy(&out.stderr).contains("Model lookup failed"));
    assert!(!f.path("argv").exists());
    assert!(!f.path("ollama-argv").exists());
}

#[test]
fn canonical_missing_model_responses_support_implicit_tags_and_registry_names() {
    for (selected, reported) in [
        ("fixture", "fixture:latest"),
        ("registry.ollama.ai/library/fixture:local", "fixture:local"),
        ("namespace/Fixture:local", "Namespace/fixture:local"),
    ] {
        let server = Server::new(move |path, body| {
            if path == "/api/show" {
                missing(reported)
            } else {
                ready(path, body)
            }
        });
        let f = provisioning_fixture();
        let out = finished(
            f.command()
                .args(["--ollama-url", &server.url, "--model", selected])
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        status(&out, 1);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("Non-interactive input; no download")
        );
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r["path"] != "/api/pull")
        );
    }
}
