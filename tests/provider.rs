mod common;
use cognix::{
    Cancellation, Error,
    provider::{Ollama, Provider},
};
use common::{Reply, Server, ready};
use serde_json::json;
use std::time::{Duration, Instant};
fn provider(url: &str) -> Ollama {
    Ollama::new("deepseek-local:test", url, Duration::from_secs(2), 16384).unwrap()
}
#[tokio::test]
async fn readiness_and_native_roundtrip() {
    let server = Server::new(|p, b| {
        if p == "/api/chat" && b["messages"][0]["content"] == "inspect" {
            Reply::json(
                json!({"done":true,"message":{"role":"assistant","content":"","thinking":"thought","tool_calls":[{"id":"native","function":{"name":"read_file","arguments":{"path":"file"}}}]}}),
            )
        } else {
            ready(p, b)
        }
    });
    let mut p = provider(&server.url);
    let c = Cancellation::default();
    p.validate(&c).await.unwrap();
    assert_eq!(p.version, "fixture-1");
    let message = p
        .chat(
            &[json!({"role":"user","content":"inspect"})],
            &cognix::schema::tools(),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(message["tool_calls"][0]["id"], "native");
    assert_eq!(message["thinking"], "thought");
    let requests = server.requests.lock().unwrap();
    let body = &requests.last().unwrap()["body"];
    assert_eq!(body["model"], "deepseek-local:test");
    assert_eq!(body["options"]["num_ctx"], 16384);
    assert_eq!(body["stream"], false);
    assert_eq!(body["tools"].as_array().unwrap().len(), 4);
}
#[tokio::test]
async fn unsupported_cloud_missing_and_behavior_failures() {
    for case in [
        "missing",
        "cloud",
        "caps",
        "bad_probe",
        "context",
        "metadata",
    ] {
        let server = Server::new(move |p, b| {
            if p == "/api/show" {
                return match case {
                    "missing" => Reply {
                        status: 404,
                        body:
                            json!({"error":"model missing; install a local tag using ollama pull"})
                                .to_string(),
                        delay: Duration::ZERO,
                    },
                    "cloud" => Reply::json(json!({"remote_host":"https://remote.example"})),
                    "caps" => Reply::json(json!({"capabilities":["completion"]})),
                    "context" => Reply::json(
                        json!({"capabilities":["tools","completion"],"model_info":{"test.context_length":4096}}),
                    ),
                    "metadata" => {
                        Reply::json(json!({"capabilities":["tools","completion"],"model_info":[]}))
                    }
                    _ => ready(p, b),
                };
            }
            if p == "/api/chat" && case == "bad_probe" {
                return Reply::json(
                    json!({"done":true,"message":{"role":"assistant","content":"I cannot call tools"}}),
                );
            }
            ready(p, b)
        });
        let e = provider(&server.url)
            .validate(&Cancellation::default())
            .await
            .unwrap_err();
        if case == "context" {
            assert!(matches!(e, Error::Context(_)));
        } else {
            assert!(matches!(e, Error::Provider(_)));
        }
    }
}
#[tokio::test]
async fn malformed_oversized_incomplete_and_generation_errors() {
    for body in [
        "invalid".into(),
        "[]".into(),
        "x".repeat(1024 * 1024 + 1),
        json!({"done":false}).to_string(),
        json!({"done":true,"done_reason":"length"}).to_string(),
        json!({"done":true,"message":{"role":"tool"}}).to_string(),
        json!({"error":"context overflow"}).to_string(),
    ] {
        let server = Server::new(move |_, _| Reply {
            status: 200,
            body: body.clone(),
            delay: Duration::ZERO,
        });
        assert!(
            provider(&server.url)
                .chat(&[], &json!([]), &Cancellation::default())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn cancel_timeout_and_missing_server() {
    let server = Server::new(|p, b| {
        let mut r = ready(p, b);
        r.delay = Duration::from_millis(300);
        r
    });
    let cancel = Cancellation::default();
    let c = cancel.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        c.cancel();
    });
    let start = Instant::now();
    assert!(matches!(
        provider(&server.url)
            .request("/api/version", None, &cancel)
            .await,
        Err(Error::Canceled)
    ));
    assert!(start.elapsed() < Duration::from_millis(250));
    worker.join().unwrap();
    let p = Ollama::new("model", &server.url, Duration::from_millis(50), 16384).unwrap();
    assert!(
        p.request("/api/version", None, &Cancellation::default())
            .await
            .is_err()
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(
        provider(&url)
            .validate(&Cancellation::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("ollama serve")
    );
}
#[test]
fn only_local_origins_and_tags() {
    for url in [
        "https://127.0.0.1",
        "http://example.com",
        "http://localhost/path",
        "http://user@localhost",
        "http://localhost?x=1",
        "http://localhost#x",
        "ftp://localhost",
    ] {
        assert!(
            Ollama::new("model", url, Duration::from_secs(1), 16384).is_err(),
            "{url}"
        );
    }
    for tag in ["", " ", "bad tag", "model:cloud", "MODEL:CLOUD"] {
        assert!(Ollama::new(tag, "http://localhost:11434", Duration::from_secs(1), 16384).is_err());
    }
    assert!(
        Ollama::new(
            "deepseek:local",
            "http://[::1]:11434",
            Duration::from_secs(1),
            16384
        )
        .is_ok()
    );
}
