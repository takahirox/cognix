#![cfg(target_os = "macos")]
mod common;

use common::{Reply, Server, ready};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
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
    fn native_cognix(&self) {
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
        for file in ["ollama.pid", "worker.pid", "cognix.pid", "build.pid"] {
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
    for (mode, fail, build_exit, message) in [
        ("managed", "1", "0", "exited before becoming ready"),
        ("absent", "0", "0", "did not become ready within"),
        ("managed", "0", "17", "startup log:"),
    ] {
        let f = Fixture::new();
        let out = finished(
            f.command()
                .env("CURL_MODE", mode)
                .env("OLLAMA_FAIL", fail)
                .env("BUILD_EXIT", build_exit)
                .spawn()
                .unwrap(),
        );
        status(&out, if build_exit == "17" { 17 } else { 1 });
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
    ] {
        let f = Fixture::new();
        let child = f
            .command()
            .env("CURL_MODE", mode)
            .env("COGNIX_WAIT", "1")
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
                    body: json!({"error":"fixture model missing"}).to_string(),
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
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains(if missing {
                "Cannot load local model"
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
