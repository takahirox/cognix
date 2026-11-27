#[path = "../tests/common/mod.rs"]
mod http_fixture;

use super::*;
use http_fixture::{Reply, Server, ready};
use std::{fs, net::TcpListener, path::Path, process::Command};

// The executable runs as the child and starts an HTTP listener of its own.
#[test]
fn serve_fixture() {
    let Some(directory) = std::env::var_os("COGNIX_TEST_OLLAMA") else {
        return;
    };
    let directory = Path::new(&directory);
    fs::write(directory.join("server.pid"), std::process::id().to_string()).unwrap();
    let mode = std::env::var("COGNIX_TEST_OLLAMA_MODE").unwrap();
    if mode == "timeout" {
        // Exercise forced cleanup for both a stubborn server and its worker.
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
    }
    let mut worker = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    fs::write(directory.join("worker.pid"), worker.id().to_string()).unwrap();
    if mode == "exit" {
        eprintln!("fixture failed to start");
        std::process::exit(9);
    }
    if mode == "bind-race" {
        while !directory.join("winner-bound").exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let error = TcpListener::bind(std::env::var("OLLAMA_HOST").unwrap()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        std::process::exit(9);
    }
    let _server = if matches!(mode.as_str(), "ready" | "slow") {
        if mode == "ready" {
            // Leave the port refused until a readiness retry is necessary.
            std::thread::sleep(Duration::from_millis(120));
        }
        let listener = TcpListener::bind(std::env::var("OLLAMA_HOST").unwrap()).unwrap();
        if mode == "slow" {
            // Hold the first connection open until its request times out.
            let (connection, _) = listener.accept().unwrap();
            std::thread::sleep(PROBE_TIMEOUT + Duration::from_millis(200));
            drop(connection);
        }
        Some(Server::listen(listener, ready))
    } else {
        None
    };
    let _ = worker.wait();
}

fn unused_endpoint() -> String {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", socket.local_addr().unwrap())
}

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn command(&self, mode: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "ollama_server::tests::serve_fixture",
            "--nocapture",
            "--",
        ])
        .env("COGNIX_TEST_OLLAMA", self.0.path())
        .env("COGNIX_TEST_OLLAMA_MODE", mode)
        .env("OLLAMA_HOST", "bad-address");
        cmd
    }

    fn assert_stopped(&self) {
        for file in ["server.pid", "worker.pid"] {
            let pid: i32 = fs::read_to_string(self.0.path().join(file))
                .unwrap()
                .parse()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while unsafe { libc::kill(pid, 0) } == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{file} {pid} survived"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[tokio::test]
async fn starts_on_refusal_waits_for_readiness_and_stops_only_owned_group() {
    let fixture = Fixture::new();
    let endpoint = unused_endpoint();
    let cancel = Cancellation::default();
    let (version, owner) = ensure_ready_with(
        &endpoint,
        &endpoint,
        PROBE_TIMEOUT,
        Duration::from_secs(3),
        &cancel,
        fixture.command("ready"),
    )
    .await
    .unwrap();
    assert_eq!(version, "fixture-1");
    assert!(owner.is_some());

    let (version, reused) = ensure_ready_with(
        &endpoint,
        &endpoint,
        PROBE_TIMEOUT,
        Duration::from_secs(3),
        &cancel,
        Command::new("/missing-ollama"),
    )
    .await
    .unwrap();
    assert_eq!(version, "fixture-1");
    assert!(reused.is_none());
    drop(reused);
    let client = Client::new();
    assert!(matches!(
        probe(&client, &endpoint, PROBE_TIMEOUT).await,
        Probe::Ready(_)
    ));

    drop(owner);
    fixture.assert_stopped();
    assert!(matches!(
        probe(&client, &endpoint, PROBE_TIMEOUT).await,
        Probe::Refused
    ));
}

#[tokio::test]
async fn retries_a_timed_out_probe_until_owned_server_responds() {
    let fixture = Fixture::new();
    let endpoint = unused_endpoint();
    let (version, owner) = ensure_ready_with(
        &endpoint,
        &endpoint,
        PROBE_TIMEOUT,
        Duration::from_secs(4),
        &Cancellation::default(),
        fixture.command("slow"),
    )
    .await
    .unwrap();
    assert_eq!(version, "fixture-1");
    assert!(owner.is_some());

    drop(owner);
    fixture.assert_stopped();
}

#[tokio::test]
async fn reuses_bind_race_winner_after_stopping_failed_owned_group() {
    let fixture = Fixture::new();
    let endpoint = unused_endpoint();
    let cancel = Cancellation::default();
    let startup = ensure_ready_with(
        &endpoint,
        &endpoint,
        PROBE_TIMEOUT,
        Duration::from_secs(3),
        &cancel,
        fixture.command("bind-race"),
    );
    let start_winner = async {
        let worker_path = fixture.0.path().join("worker.pid");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !worker_path.exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixture did not start");
        let server_pid: libc::pid_t = fs::read_to_string(fixture.0.path().join("server.pid"))
            .unwrap()
            .parse()
            .unwrap();
        let listener = TcpListener::bind(endpoint.strip_prefix("http://").unwrap()).unwrap();
        fs::write(fixture.0.path().join("winner-bound"), "").unwrap();
        Server::listen(listener, move |path, body| {
            // The winner answers only after the losing child exits.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    server_pid as _,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            assert!(result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD));
            ready(path, body)
        })
    };
    let (outcome, winner) = tokio::join!(startup, start_winner);
    let (version, owner) = outcome.unwrap();
    assert_eq!(version, "fixture-1");
    assert!(owner.is_none());
    fixture.assert_stopped();
    assert!(matches!(
        probe(&Client::new(), &winner.url, PROBE_TIMEOUT).await,
        Probe::Ready(_)
    ));
}

#[tokio::test]
async fn startup_exit_timeout_and_cancellation_stop_owned_group() {
    for (mode, expected) in [
        ("exit", "exited before becoming ready"),
        ("timeout", "did not become ready within"),
        ("cancel", "Interrupted"),
    ] {
        let fixture = Fixture::new();
        let endpoint = unused_endpoint();
        let cancel = Cancellation::default();
        let worker_path = fixture.0.path().join("worker.pid");
        let timeout = if mode == "timeout" {
            Duration::from_millis(400)
        } else {
            Duration::from_secs(3)
        };
        let startup = ensure_ready_with(
            &endpoint,
            &endpoint,
            PROBE_TIMEOUT,
            timeout,
            &cancel,
            fixture.command(mode),
        );
        let cancel_when_started = async {
            if mode == "cancel" {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !worker_path.exists() && Instant::now() < deadline {
                    sleep(Duration::from_millis(10)).await;
                }
                assert!(worker_path.exists(), "fixture did not start");
                cancel.cancel();
            }
        };
        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(startup, cancel_when_started)
        })
        .await
        .expect("startup did not finish within the test deadline");
        let error = outcome.err().expect("startup should fail").to_string();
        assert!(error.contains(expected), "{error}");
        fixture.assert_stopped();
    }
}

#[tokio::test]
async fn distinguishes_missing_binary_custom_refusal_and_incompatible_listener() {
    let cancel = Cancellation::default();
    let endpoint = unused_endpoint();
    let timeout = Duration::from_secs(2);
    for (managed, expected) in [
        (endpoint.as_str(), "ollama command not found"),
        (DEFAULT_OLLAMA_URL, "Start the custom server manually"),
    ] {
        let error = ensure_ready_with(
            &endpoint,
            managed,
            PROBE_TIMEOUT,
            timeout,
            &cancel,
            Command::new("/missing-ollama"),
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains(expected), "{error}");
    }

    for (status, body, delay) in [
        (200, "welcome", Duration::ZERO),
        (200, r#"{"version":1}"#, Duration::ZERO),
        (200, r#"{"version":" "}"#, Duration::ZERO),
        (302, r#"{"version":"1"}"#, Duration::ZERO),
        (
            200,
            r#"{"version":"1","error":"unavailable"}"#,
            Duration::ZERO,
        ),
        (
            200,
            r#"{"version":"1"}"#,
            PROBE_TIMEOUT + Duration::from_millis(100),
        ),
    ] {
        let server = Server::new(move |_, _| Reply {
            status,
            body: body.into(),
            delay,
        });
        let error = ensure_ready_with(
            &server.url,
            &server.url,
            PROBE_TIMEOUT,
            timeout,
            &cancel,
            Command::new("/missing-ollama"),
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("No valid Ollama"), "{error}");
        assert_eq!(server.requests.lock().unwrap()[0]["path"], "/api/version");
    }
}
