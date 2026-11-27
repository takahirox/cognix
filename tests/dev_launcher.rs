#![cfg(target_os = "macos")]
mod common;

use common::{Reply, Server, ready};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::{
        fd::FromRawFd,
        unix::fs::{PermissionsExt, symlink},
    },
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

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
            "home",
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
    printf '%s' "$$" > "$FIXTURE/build.pid"
    exec /bin/sleep 30
fi
if [[ ${BUILD_EXIT:-0} != 0 ]]; then
    /bin/sleep 10 </dev/null >/dev/null 2>&1 &
    printf '%s' "$!" > "$FIXTURE/build-worker.pid"
fi
exit "${BUILD_EXIT:-0}"
"#,
        );
        executable(
            &root.join("repo with spaces/target/debug/cognix"),
            r#"#!/bin/bash
printf '%s\0' "$@" > "$FIXTURE/argv"
pwd > "$FIXTURE/cwd-record"
printf '%s' "${COGNIX_MODEL:-}" > "$FIXTURE/model"
if [[ ${READ_STDIN:-0} == 1 ]]; then
    IFS= read -r line
    printf '%s' "$line" > "$FIXTURE/stdin-record"
fi
exit "${COGNIX_EXIT:-0}"
"#,
        );
        Self { dir }
    }
    fn path(&self, path: &str) -> std::path::PathBuf {
        self.dir.path().join(path)
    }
    fn command(&self) -> Command {
        let mut c = Command::new(self.path("repo with spaces/scripts/dev.sh"));
        self.configure(&mut c);
        c
    }
    fn direct(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cognix"));
        self.configure(&mut c);
        c
    }
    fn configure(&self, c: &mut Command) {
        c.current_dir(self.path("cwd"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("FIXTURE", self.dir.path())
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("logs"))
            .env("COGNIX_DEV_STARTUP_TIMEOUT", "2")
            .env_remove("COGNIX_OLLAMA_URL")
            .env_remove("COGNIX_MODEL")
            .env_remove("BASH_ENV")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
    fn native_cognix(&self) {
        let target = self.path("repo with spaces/target/debug/cognix");
        fs::remove_file(&target).unwrap();
        symlink(env!("CARGO_BIN_EXE_cognix"), target).unwrap();
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
            child.wait().unwrap();
            panic!("launcher did not finish");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn status(out: &Output, expected: i32) {
    assert_eq!(
        out.status.code(),
        Some(expected),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn build_preserves_arguments_stdin_working_directory_and_exit_status() {
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
    assert_eq!(
        f.argv("argv"),
        [vec!["--dev-provision-model"], args.to_vec()].concat()
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
    assert_eq!(cargo[6..], ["--bin", "cognix"]);
}

#[test]
fn build_failure_and_hangup_never_start_cognix() {
    let failed = Fixture::new();
    status(
        &finished(failed.command().env("BUILD_EXIT", "17").spawn().unwrap()),
        17,
    );
    failed.assert_stopped("build-worker.pid");
    assert!(!failed.path("argv").exists());

    let canceled = Fixture::new();
    let child = canceled.command().env("BUILD_WAIT", "1").spawn().unwrap();
    canceled.wait_for("build.pid");
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGHUP) }, 0);
    status(&finished(child), 129);
    canceled.assert_stopped("build.pid");
    assert!(!canceled.path("argv").exists());
}

#[test]
fn existing_real_http_server_and_native_cli_work_from_both_directories() {
    let server = Server::new(ready);
    let f = Fixture::new();
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
    // Help works even when the effective endpoint is unavailable.
    status(
        &finished(
            f.command()
                .env("COGNIX_OLLAMA_URL", "http://127.0.0.1:1")
                .arg("--help")
                .spawn()
                .unwrap(),
        ),
        0,
    );
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["path"] == "/api/version")
            .count(),
        2
    );
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
        let out = finished(
            f.direct()
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
                "Install a suitable local tag explicitly"
            } else {
                "completion and tools"
            }),
            "{text}"
        );
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
    f.native_cognix();
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
fn effective_models_are_checked_exactly_without_prompt() {
    for (args, environment, expected) in [
        (
            vec![
                "--model",
                "namespace/fixture:tag",
                "--allow-command",
                r#"["/bin/echo","two words"]"#,
            ],
            "env:ignored",
            "namespace/fixture:tag",
        ),
        (
            vec!["--model=fixture:equal"],
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
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 4); // version, provisioning, capabilities, tool probe
        for show in &requests[1..3] {
            assert_eq!(show["path"], "/api/show");
            assert_eq!(show["body"]["model"], expected);
        }
    }
}

#[test]
fn launcher_reuses_the_normalized_endpoint_and_model_from_user_configuration() {
    let server = Server::new(ready);
    let f = provisioning_fixture();
    fs::create_dir(f.path("home/.cognix")).unwrap();
    fs::write(
        f.path("home/.cognix/config.toml"),
        format!(
            "model = 'saved:local'\nollama_url = '{} '\n",
            server.url.replace("127.0.0.1", "LOCALHOST")
        ),
    )
    .unwrap();
    let out = finished(f.command().stdin(Stdio::null()).spawn().unwrap());
    status(&out, 0);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0]["path"], "/api/version");
    assert_eq!(requests[1]["body"]["model"], "saved:local");
}

#[test]
fn launcher_applies_one_settings_snapshot_through_probe_and_provisioning() {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

    let other_server = Server::new(ready);
    for edit_at in ["/api/version", "/api/pull"] {
        let f = provisioning_fixture();
        fs::create_dir(f.path("home/.cognix")).unwrap();
        let config = f.path("home/.cognix/config.toml");
        let edited_config = config.clone();
        let other_url = other_server.url.clone();
        let installed = AtomicBool::new(false);
        let server = Server::new(move |path, body| {
            if path == edit_at {
                let edited = if path == "/api/version" {
                    format!("model = 'original:local'\nollama_url = '{other_url}'\n")
                } else {
                    fs::read_to_string(&edited_config)
                        .unwrap()
                        .replace("original:local", "edited:local")
                };
                fs::write(&edited_config, edited).unwrap();
            }
            match path {
                "/api/show" if !installed.load(Relaxed) => missing(body["model"].as_str().unwrap()),
                "/api/pull" => {
                    installed.store(true, Relaxed);
                    Reply {
                        status: 200,
                        body: "{\"status\":\"success\"}\n".into(),
                        delay: Duration::ZERO,
                    }
                }
                _ => ready(path, body),
            }
        });
        fs::write(
            &config,
            format!("model = 'original:local'\nollama_url = '{}'\n", server.url),
        )
        .unwrap();
        let mut command = f.command();
        let mut terminal = terminal_input(&mut command);
        let child = command.spawn().unwrap();
        terminal.write_all(b"y\n\x04").unwrap();
        let out = finished(child);
        status(&out, 0);
        let requests = server.requests.lock().unwrap();
        let paths: Vec<_> = requests
            .iter()
            .map(|r| r["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "/api/version",
                "/api/show",
                "/api/pull",
                "/api/show",
                "/api/show",
                "/api/chat"
            ]
        );
        assert!(
            requests[1..]
                .iter()
                .all(|r| r["body"]["model"] == "original:local")
        );
    }
    assert!(other_server.requests.lock().unwrap().is_empty());
}

#[test]
fn decline_default_eof_and_noninteractive_never_download() {
    for answer in [
        Some("n\n"),
        Some("\n"),
        Some("\x04"),
        // Canonical terminals flush buffered text on the first Ctrl-D and
        // report EOF on the second. Neither is a newline-submitted approval.
        Some("y\x04\x04"),
        Some("yes\x04\x04"),
        None,
    ] {
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
    let args = ["--ollama-url", &server.url, "--model", "fixture:chosen"];
    let mut command = f.command();
    command
        .args(args)
        .env("COGNIX_OLLAMA_URL", "http://127.0.0.1:1");
    let mut terminal = terminal_input(&mut command);
    let child = command.spawn().unwrap();
    // Both lines remain available to their respective prompts. With the PTY
    // held open, consuming /exit at the consent prompt would hang the session.
    terminal.write_all(b"YES\n/exit\n").unwrap();
    let out = finished(child);
    status(&out, 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("\n> "));
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

#[test]
fn failed_incomplete_and_unverified_downloads_stop_startup() {
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
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request["path"] != "/api/chat")
        );
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
                .all(|r| matches!(r["path"].as_str(), Some("/api/version" | "/api/show")))
        );
    }
}
