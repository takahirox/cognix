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

struct CreateFixture(usize);
impl Provider for CreateFixture {
    async fn chat(
        &mut self,
        messages: &[Value],
        _: &Value,
        _: &Cancellation,
    ) -> Result<Value, Error> {
        let reply = match self.0 {
            0 => calls(vec![call(
                "apply_patch",
                json!({"path":"new.rs","old_text":null,"new_text":"// new\n"}),
            )]),
            1 => {
                let result: Value =
                    serde_json::from_str(messages.last().unwrap()["content"].as_str().unwrap())
                        .unwrap();
                assert_eq!(result["status"], "ok");
                assert_eq!(result["executed"], true);
                assert_eq!(result["path"], "new.rs");
                calls(vec![call(
                    "apply_patch",
                    json!({"path":"new.rs","old_text":"new","new_text":"created"}),
                )])
            }
            2 => answer("Created and updated new.rs"),
            _ => panic!("unexpected request"),
        };
        self.0 += 1;
        Ok(reply)
    }
}
#[tokio::test]
async fn model_requested_creation_in_multistep_task_is_reported() {
    let (t, a) = setup(vec![]);
    let mut a = Agent::new(CreateFixture(0), a.runtime);
    let result = a.run("Create a Rust module and update it").await;
    assert_eq!(result["status"], "final_answer", "{result}");
    assert_eq!(result["modified_files"], json!(["new.rs"]));
    assert_eq!(result["events"][0]["tool"], "apply_patch");
    assert_eq!(result["events"][0]["result"]["status"], "ok");
    assert_eq!(result["events"][0]["arguments"]["new_text"], "// new\n");
    assert_eq!(result["checks"], json!([]));
    assert_eq!(
        fs::read_to_string(t.path().join("new.rs")).unwrap(),
        "// created\n"
    );
    assert_eq!(
        fs::read_to_string(t.path().join("file")).unwrap(),
        "content"
    );
}
#[tokio::test]
async fn ordering_ids_history_and_final_prose() {
    let mut explicit = call(
        "apply_patch",
        json!({"path":"new","old_text":null,"new_text":"new"}),
    );
    explicit["id"] = json!("native-id");
    let (_t, mut a) = setup(vec![
        calls(vec![
            explicit,
            call(
                "apply_patch",
                json!({"path":"file","old_text":"content","new_text":"updated"}),
            ),
        ]),
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
    assert_eq!(result["status"], "ok");
    assert_eq!(a.run("and now?").await["status"], "final_answer");
    assert_eq!(a.messages[1]["content"], "inspect");
    assert_eq!(a.messages[6]["content"], "and now?");
}
#[tokio::test]
async fn invalid_arguments_and_tool_errors_are_recoverable() {
    let (_t, mut a) = setup(vec![
        calls(vec![
            call(
                "apply_patch",
                json!({"path":2,"old_text":null,"new_text":"content"}),
            ),
            call("unknown", json!({})),
            call(
                "apply_patch",
                json!({"path":"missing","old_text":"content","new_text":"updated"}),
            ),
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
    let valid = call(
        "apply_patch",
        json!({"path":"file","old_text":"content","new_text":"updated"}),
    );
    let mut duplicate = valid.clone();
    duplicate["id"] = json!("same");
    for batch in [
        calls(vec![valid.clone(), json!({"function":{}})]),
        calls(vec![duplicate.clone(), duplicate]),
        calls(vec![valid.clone(); 9]),
        json!({"role":"assistant","tool_calls":{}}),
        json!({"role":"assistant","content":3}),
        calls(vec![
            json!({"id":"","function":{"name":"apply_patch","arguments":{"path":"file","old_text":"content","new_text":"updated"}}}),
        ]),
    ] {
        let (_t, mut a) = setup(vec![batch]);
        assert_eq!(a.run("try").await["status"], "error");
        assert!(a.runtime.events.is_empty());
    }
}
#[tokio::test]
async fn duplicate_across_turns_fails_before_execution() {
    let mut c = call(
        "apply_patch",
        json!({"path":"file","old_text":"content","new_text":"updated"}),
    );
    c["id"] = json!("same");
    let (_t, mut a) = setup(vec![calls(vec![c.clone()]), calls(vec![c])]);
    assert_eq!(a.run("try").await["status"], "error");
    assert_eq!(a.runtime.events.len(), 1);
}
#[tokio::test]
async fn step_context_and_provider_stops() {
    let (_t, mut a) = setup(vec![calls(vec![call(
        "apply_patch",
        json!({"path":"file","old_text":"content","new_text":"updated"}),
    )])]);
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
        Ok(calls(vec![call(
            "apply_patch",
            json!({"path":"file","old_text":"content","new_text":"updated"}),
        )]))
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
        call(
            "apply_patch",
            json!({"path":"file","old_text":"content","new_text":"updated"}),
        ),
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
    for restricted in [true, false] {
        let (_t, mut a) = setup(vec![
            calls(vec![call("run_command", json!({"argv":["/usr/bin/true"]}))]),
            answer("All tests passed!"),
        ]);
        if restricted {
            a.runtime.allowed_commands = vec![vec!["/usr/bin/false".into()]];
        }
        a.runtime.approve = Box::new(|_, _| false);
        let r = a.run("verify").await;
        assert_eq!(
            r["checks"][0]["result"]["status"],
            if !restricted && cfg!(target_os = "macos") {
                "denied"
            } else {
                "not_run"
            }
        );
        assert_eq!(r["checks"][0]["result"]["executed"], false);
        assert_eq!(r["model_reply"], "All tests passed!");
    }
}
#[cfg(target_os = "macos")]
struct Fixture {
    phase: usize,
    check: Value,
}
#[cfg(target_os = "macos")]
impl Provider for Fixture {
    async fn chat(&mut self, m: &[Value], _: &Value, _: &Cancellation) -> Result<Value, Error> {
        let result = |offset| -> Value {
            let message: &Value = &m[m.len() - offset];
            serde_json::from_str(message["content"].as_str().unwrap()).unwrap()
        };
        let v = match self.phase {
            0 => calls(vec![
                call("run_command", json!({"argv":["ls","-1"]})),
                call("run_command", json!({"argv":["cat","main.py"]})),
                call(
                    "run_command",
                    json!({"argv":["grep","-n","return","main.py"]}),
                ),
                call("run_command", self.check.clone()),
            ]),
            1 => {
                assert_eq!(result(4)["output"], "main.py\ntest_main.py\nunrelated\n");
                assert!(
                    result(3)["output"]
                        .as_str()
                        .unwrap()
                        .contains("def divide(")
                );
                assert_eq!(
                    result(2)["output"],
                    "2:    return numerator / denominator\n"
                );
                let failure = result(1);
                assert_eq!(failure["status"], "failed");
                assert_eq!(failure["exit_code"], 1);
                assert!(
                    failure["output"]
                        .as_str()
                        .unwrap()
                        .contains("ZeroDivisionError")
                );
                calls(vec![
                    call(
                        "apply_patch",
                        json!({"path":"main.py","old_text":"return numerator / denominator","new_text":"return numerator / denominator if denominator else None"}),
                    ),
                    call(
                        "apply_patch",
                        json!({"path":"test_division_edges.py","old_text":null,"new_text":"import unittest\nfrom main import divide\n\nclass DivisionEdgeTests(unittest.TestCase):\n    def test_negative_denominator(self):\n        self.assertEqual(divide(6, -2), -3)\n"}),
                    ),
                    call("run_command", self.check.clone()),
                ])
            }
            2 => {
                assert_eq!(result(3)["executed"], true);
                assert_eq!(result(2)["executed"], true);
                let success = result(1);
                assert_eq!(success["status"], "passed");
                assert_eq!(success["exit_code"], 0);
                assert!(success["output"].as_str().unwrap().contains("Ran 3 tests"));
                answer("Fixed division by zero. All 3 tests passed.")
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
    let runtime = Runtime::new(t.path(), Box::new(|_, _| true), Cancellation::default()).unwrap();
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
    assert_eq!(
        r["modified_files"],
        json!(["main.py", "test_division_edges.py"])
    );
    assert_eq!(r["checks"][3]["result"]["status"], "failed");
    assert_eq!(r["checks"][4]["result"]["status"], "passed");
    assert!(
        r["model_reply"]
            .as_str()
            .unwrap()
            .contains("All 3 tests passed")
    );
    assert_eq!(
        fs::read(t.path().join("test_main.py")).unwrap(),
        fs::read(fixture.join("test_main.py")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(t.path().join("unrelated")).unwrap(),
        "user edit"
    );
}
