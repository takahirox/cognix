use cognix::{Cancellation, Error, agent::Agent, provider::Provider, runtime::Runtime};
use serde_json::{Value, json};
use std::{collections::VecDeque, fs};
struct Script(VecDeque<Result<Value, Error>>);
impl Provider for Script {
    async fn chat(&mut self, _: &[Value], _: &Value, _: &Cancellation) -> Result<Value, Error> {
        self.0.pop_front().expect("unexpected model request")
    }
}
fn call(name: &str, args: Value) -> Value {
    json!({"function":{"name":name,"arguments":args}})
}
fn calls(calls: Vec<Value>) -> Value {
    json!({"role":"assistant","content":"","thinking":"retained","tool_calls":calls})
}
fn answer(s: &str) -> Value {
    json!({"role":"assistant","content":s})
}
fn setup(script: Vec<Value>) -> (tempfile::TempDir, Agent<Script>) {
    let t = tempfile::tempdir().unwrap();
    fs::write(t.path().join("file"), "content").unwrap();
    let r = Runtime::new(t.path(), Box::new(|_, _| true), Cancellation::default()).unwrap();
    (
        t,
        Agent::new(Script(script.into_iter().map(Ok).collect()), r),
    )
}
#[tokio::test]
async fn ordering_ids_history_and_final_prose() {
    let mut explicit = call("list_files", json!({"path":"."}));
    explicit["id"] = json!("native-id");
    let (_t, mut a) = setup(vec![
        calls(vec![explicit, call("read_file", json!({"path":"file"}))]),
        answer("inspected"),
        answer("followup"),
    ]);
    let report = a.run("inspect").await;
    assert_eq!(report["status"], "final_answer");
    assert_eq!(report["model_reply"], "inspected");
    assert_eq!(a.messages[2]["thinking"], "retained");
    assert_eq!(a.messages[3]["tool_call_id"], "native-id");
    assert_eq!(a.messages[4]["tool_call_id"], "cognix-1-1");
    let result: Value = serde_json::from_str(a.messages[4]["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["content"], "content");
    assert_eq!(a.run("and now?").await["status"], "final_answer");
    assert_eq!(a.messages[1]["content"], "inspect");
    assert_eq!(a.messages[6]["content"], "and now?");
}
#[tokio::test]
async fn invalid_arguments_and_tool_errors_are_recoverable() {
    let (_t, mut a) = setup(vec![
        calls(vec![
            call("read_file", json!({"path":2})),
            call("unknown", json!({})),
            call("read_file", json!({"path":"missing"})),
        ]),
        answer("blocked"),
    ]);
    let r = a.run("try").await;
    assert_eq!(r["status"], "final_answer");
    assert_eq!(r["events"].as_array().unwrap().len(), 3);
    for m in &a.messages[3..6] {
        let r: Value = serde_json::from_str(m["content"].as_str().unwrap()).unwrap();
        assert_eq!(r["status"], "failed");
        assert!(r["error"].is_string());
    }
}
#[tokio::test]
async fn malformed_batches_execute_nothing() {
    let valid = call("read_file", json!({"path":"file"}));
    let mut duplicate = valid.clone();
    duplicate["id"] = json!("same");
    for batch in [
        calls(vec![valid.clone(), json!({"function":{}})]),
        calls(vec![duplicate.clone(), duplicate]),
        calls(vec![valid.clone(); 9]),
        json!({"role":"assistant","tool_calls":{}}),
        json!({"role":"assistant","content":3}),
        calls(vec![
            json!({"id":"","function":{"name":"read_file","arguments":{"path":"file"}}}),
        ]),
    ] {
        let (_t, mut a) = setup(vec![batch]);
        assert_eq!(a.run("try").await["status"], "error");
        assert!(a.runtime.events.is_empty());
    }
}
#[tokio::test]
async fn duplicate_across_turns_fails_before_execution() {
    let mut c = call("read_file", json!({"path":"file"}));
    c["id"] = json!("same");
    let (_t, mut a) = setup(vec![calls(vec![c.clone()]), calls(vec![c])]);
    assert_eq!(a.run("try").await["status"], "error");
    assert_eq!(a.runtime.events.len(), 1);
}
#[tokio::test]
async fn step_context_and_provider_stops() {
    let (_t, mut a) = setup(vec![calls(vec![call("read_file", json!({"path":"file"}))])]);
    a.max_steps = 1;
    assert_eq!(a.run("try").await["status"], "limit_reached");
    let (_t, mut a) = setup(vec![]);
    a.context_budget = 1;
    assert_eq!(a.run("large").await["status"], "context_limit");
    assert_eq!(a.messages.len(), 2);
    for (e, status) in [
        (Error::Provider("server failed".into()), "error"),
        (Error::Context("overflow".into()), "context_limit"),
    ] {
        let (_t, mut a) = setup(vec![]);
        a.provider.0.push_back(Err(e));
        assert_eq!(a.run("try").await["status"], status);
    }
}
struct CancelProvider;
impl Provider for CancelProvider {
    async fn chat(&mut self, _: &[Value], _: &Value, c: &Cancellation) -> Result<Value, Error> {
        c.cancel();
        Ok(calls(vec![call("read_file", json!({"path":"file"}))]))
    }
}
#[tokio::test]
async fn cancellation_before_and_during_model() {
    let (_t, mut a) = setup(vec![]);
    a.runtime.cancel.cancel();
    assert_eq!(a.run("stop").await["status"], "canceled");
    assert!(a.runtime.events.is_empty());
    let (_t, a) = setup(vec![]);
    let mut a = Agent::new(CancelProvider, a.runtime);
    assert_eq!(a.run("stop").await["status"], "canceled");
    assert!(a.runtime.events.is_empty());
}
#[tokio::test]
async fn cancellation_at_approval_records_queued_unrun_calls() {
    let (_t, mut a) = setup(vec![calls(vec![
        call("run_command", json!({"argv":["/usr/bin/true"]})),
        call("read_file", json!({"path":"file"})),
    ])]);
    a.runtime.allowed_commands = vec![vec!["/usr/bin/true".into()]];
    let c = a.cancellation();
    a.runtime.approve = Box::new(move |_, _| {
        c.cancel();
        true
    });
    if cfg!(target_os = "macos") {
        let r = a.run("do").await;
        assert_eq!(r["status"], "canceled");
        assert_eq!(r["events"][0]["result"]["executed"], false);
        assert_eq!(r["events"][1]["result"]["status"], "not_run");
        assert_eq!(a.messages.len(), 5);
    }
}
#[tokio::test]
async fn unsupported_model_claim_cannot_change_evidence() {
    for denied in [true, false] {
        let (_t, mut a) = setup(vec![
            calls(vec![call("run_command", json!({"argv":["/usr/bin/true"]}))]),
            answer("All tests passed!"),
        ]);
        if denied {
            a.runtime.allowed_commands = vec![vec!["/usr/bin/true".into()]];
        }
        a.runtime.approve = Box::new(|_, _| false);
        let r = a.run("verify").await;
        assert_eq!(
            r["checks"][0]["result"]["status"],
            if denied && cfg!(target_os = "macos") {
                "denied"
            } else {
                "not_run"
            }
        );
        assert_eq!(r["checks"][0]["result"]["executed"], false);
        assert_eq!(r["model_reply"], "All tests passed!");
    }
}
struct Fixture {
    phase: usize,
    check: Value,
}
impl Provider for Fixture {
    async fn chat(&mut self, m: &[Value], _: &Value, _: &Cancellation) -> Result<Value, Error> {
        let v = match self.phase {
            0 => calls(vec![
                call("list_files", json!({"path":"."})),
                call("read_file", json!({"path":"main.py"})),
                call("run_command", self.check.clone()),
            ]),
            1 => {
                let failure: Value =
                    serde_json::from_str(m.last().unwrap()["content"].as_str().unwrap()).unwrap();
                assert_eq!(failure["status"], "failed");
                let read: Value =
                    serde_json::from_str(m[m.len() - 2]["content"].as_str().unwrap()).unwrap();
                calls(vec![
                    call(
                        "apply_patch",
                        json!({"path":"main.py","sha256":read["sha256"],"old_text":"return numerator / denominator","new_text":"return numerator / denominator if denominator else None"}),
                    ),
                    call("run_command", self.check.clone()),
                ])
            }
            2 => {
                let success: Value =
                    serde_json::from_str(m.last().unwrap()["content"].as_str().unwrap()).unwrap();
                assert_eq!(success["status"], "passed");
                answer("Fixed and verified")
            }
            _ => panic!("unexpected request"),
        };
        self.phase += 1;
        Ok(v)
    }
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn fixture_inspect_fail_patch_verify_recovery() {
    let t = tempfile::tempdir().unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/division");
    for file in ["main.py", "test_main.py"] {
        fs::copy(fixture.join(file), t.path().join(file)).unwrap();
    }
    fs::write(t.path().join("unrelated"), "user edit").unwrap();
    let argv = vec![
        "/opt/homebrew/bin/python3",
        "-I",
        "-B",
        "-m",
        "unittest",
        "discover",
        "-s",
        ".",
        "-p",
        "test_*.py",
    ];
    let mut runtime =
        Runtime::new(t.path(), Box::new(|_, _| true), Cancellation::default()).unwrap();
    runtime.allowed_commands = vec![argv.iter().map(|s| s.to_string()).collect()];
    let mut a = Agent::new(
        Fixture {
            phase: 0,
            check: json!({"argv":argv}),
        },
        runtime,
    );
    a.context_budget = 32768;
    let r = a.run("Fix division by zero").await;
    assert_eq!(r["status"], "final_answer", "{r}");
    assert_eq!(r["modified_files"], json!(["main.py"]));
    assert_eq!(r["checks"][0]["result"]["status"], "failed");
    assert_eq!(r["checks"][1]["result"]["status"], "passed");
    assert_eq!(
        fs::read(t.path().join("test_main.py")).unwrap(),
        fs::read(fixture.join("test_main.py")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(t.path().join("unrelated")).unwrap(),
        "user edit"
    );
}
