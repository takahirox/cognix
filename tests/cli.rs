mod common;
use cognix::terminal;
use common::{Reply, Server, ready};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn cli(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_cognix"));
    c.env("HOME", home)
        .env_remove("COGNIX_MODEL")
        .env_remove("COGNIX_OLLAMA_URL");
    c
}
fn command(root: &Path, url: &str) -> Command {
    let mut c = cli(root);
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

#[cfg(target_os = "macos")]
fn command_task_server(argv: Value, http_get: bool) -> Server {
    Server::new(move |p, b| {
        if p != "/api/chat" || b["messages"].as_array().unwrap().len() == 1 {
            ready(p, b)
        } else if b["messages"].as_array().unwrap().len() == 2 {
            tool("run_command", json!({"argv":argv,"http_get":http_get}))
        } else {
            final_answer()
        }
    })
}

#[cfg(target_os = "macos")]
#[test]
fn default_workspace_profile_builds_and_tests_a_ruby_gem() {
    let project = tempfile::tempdir().unwrap();
    for (path, content) in [
        (
            "Rakefile",
            r#"require 'openssl'
require 'rake/testtask'

task :build do
  sh '/usr/bin/ruby', '-S', 'gem', 'build', 'sample.gemspec'
end
Rake::TestTask.new { |t| t.pattern = 'test_sample.rb' }
task default: [:build, :test]
"#,
        ),
        (
            "sample.gemspec",
            r#"Gem::Specification.new do |s|
  s.name = 'sample'
  s.version = '0.1.0'
  s.summary = 'Sandbox test fixture'
  s.authors = ['Cognix']
  s.license = 'MIT'
  s.files = ['sample.rb']
end
"#,
        ),
        ("sample.rb", "def add(a, b)\n  a + b\nend\n"),
        (
            "test_sample.rb",
            r#"require 'minitest/autorun'
require_relative 'sample'

class TestSample < Minitest::Test
  def test_add
    assert_equal 5, add(2, 3)
  end
end
"#,
        ),
    ] {
        fs::write(project.path().join(path), content).unwrap();
    }
    let server = command_task_server(json!(["/usr/bin/ruby", "-S", "rake"]), false);
    let out = run(
        command(project.path(), &server.url).args(["--task", "Build and test the gem"]),
        "y\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains("\"passed\"; executed=true; exit=0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("1 runs, 1 assertions, 0 failures"),
        "{stdout}"
    );
    assert!(project.path().join("sample-0.1.0.gem").is_file());
}

#[cfg(target_os = "macos")]
#[test]
fn default_workspace_profile_builds_with_make_and_cargo() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    // Cargo would reject this inherited configuration if it reached the build.
    fs::write(outside.path().join("config.toml"), "invalid Cargo config").unwrap();
    for path in ["src", ".cargo", "vendor/sandbox-dependency/src"] {
        fs::create_dir_all(project.path().join(path)).unwrap();
    }
    for (path, content) in [
        ("Makefile", "all:\n\t/bin/echo built > make-artifact\n"),
        (
            "Cargo.toml",
            "[package]\nname = 'sandbox-fixture'\nversion = '0.1.0'\nedition = '2021'\n\
             [dependencies]\nsandbox-dependency = '=0.1.0'\n",
        ),
        (
            "src/main.rs",
            "fn main() { println!(\"{}\", sandbox_dependency::message()); }\n",
        ),
        (
            ".cargo/config.toml",
            "[source.crates-io]\nreplace-with = 'vendored'\n[source.vendored]\ndirectory = 'vendor'\n",
        ),
        (
            "vendor/sandbox-dependency/Cargo.toml",
            "[package]\nname = 'sandbox-dependency'\nversion = '0.1.0'\nedition = '2021'\n",
        ),
        (
            "vendor/sandbox-dependency/src/lib.rs",
            "pub fn message() -> &'static str { \"built with a vendored dependency\" }\n",
        ),
        (
            "vendor/sandbox-dependency/.cargo-checksum.json",
            r#"{"package":null,"files":{}}"#,
        ),
    ] {
        fs::write(project.path().join(path), content).unwrap();
    }
    for (argv, artifact) in [
        (json!(["/usr/bin/make"]), "make-artifact"),
        (
            json!(["/opt/homebrew/bin/cargo", "build", "--offline"]),
            "target/debug/sandbox-fixture",
        ),
    ] {
        let server = command_task_server(argv, false);
        let out = run(
            command(project.path(), &server.url)
                .env("CARGO_HOME", outside.path())
                .args(["--task", "Build the fixture"]),
            "y\n",
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}");
        assert!(
            stdout.contains("\"passed\"; executed=true; exit=0"),
            "{stdout}"
        );
        assert!(project.path().join(artifact).is_file(), "{stdout}");
    }
    let artifact = Command::new(project.path().join("target/debug/sandbox-fixture"))
        .output()
        .unwrap();
    assert!(artifact.status.success());
    assert_eq!(artifact.stdout, b"built with a vendored dependency\n");
}

#[cfg(target_os = "macos")]
const BROKER_CURL: &str = r#"/usr/bin/curl --disable --silent --fail --unix-socket "$COGNIX_HTTP_SOCKET" --request-target "$1" http://localhost/"#;

#[cfg(target_os = "macos")]
#[test]
fn denied_broker_request_fails_task_and_preserves_parent_exit_code() {
    let project = tempfile::tempdir().unwrap();
    let destination = Server::new(|_, _| Reply::json(json!("must stay blocked")));
    let url = format!("{}/artifact", destination.url);
    let script = format!("if {BROKER_CURL}; then exit 2; fi; exit 0");
    let server = command_task_server(json!(["/bin/bash", "-c", script, "get", url]), true);
    let out = run(
        command(project.path(), &server.url).args(["--task", "Get the artifact"]),
        "y\nn\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"passed\"; executed=true; exit=0"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("Network GET \"{url}\" \"denied\"")),
        "{stdout}"
    );
    assert!(destination.requests.lock().unwrap().is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn failed_broker_request_fails_task_and_preserves_parent_exit_code() {
    let project = tempfile::tempdir().unwrap();
    // Accept the connection but close it without sending an HTTP response.
    let endpoint = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/artifact", endpoint.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let (connection, _) = endpoint.accept().unwrap();
        drop(connection);
    });
    let script = format!("if {BROKER_CURL}; then exit 2; fi; exit 0");
    let server = command_task_server(json!(["/bin/bash", "-c", script, "get", url]), true);
    let out = run(
        command(project.path(), &server.url).args(["--task", "Get the artifact"]),
        "y\ny\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("\"passed\"; executed=true; exit=0"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("Network GET \"{url}\" \"failed\"")),
        "{stdout}"
    );
    worker.join().unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn upstream_http_error_fails_task_even_when_the_parent_handles_it() {
    let project = tempfile::tempdir().unwrap();
    for status in [404, 503] {
        let destination = Server::new(move |_, _| Reply {
            status,
            ..Reply::json(json!("artifact unavailable"))
        });
        let url = format!("{}/artifact", destination.url);
        let script = format!("if {BROKER_CURL}; then exit 2; fi; exit 0");
        let server = command_task_server(json!(["/bin/bash", "-c", script, "get", url]), true);
        let out = run(
            command(project.path(), &server.url).args(["--task", "Get the artifact"]),
            "y\ny\n",
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(1), "{stdout}");
        assert!(
            stdout.contains("\"passed\"; executed=true; exit=0"),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!("Network GET \"{url}\" \"failed\" (HTTP {status})")),
            "{stdout}"
        );
        assert_eq!(destination.requests.lock().unwrap().len(), 1);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn broker_prompt_stops_when_command_exits_or_times_out() {
    let project = tempfile::tempdir().unwrap();
    let destination = Server::new(|_, _| Reply::json(json!("must stay blocked")));
    let url = format!("{}/artifact", destination.url);
    for (script, timeout, expected_code, expected_result) in [
        (
            BROKER_CURL.to_owned(),
            "1",
            1,
            "\"limit_reached\"; executed=true; exit=-9",
        ),
        (
            // Keep the output pipe open while the leader exits. The broker
            // must stop the prompt even before the command output reaches EOF.
            format!("{BROKER_CURL} & /bin/sleep 1; exit 0"),
            "10",
            0,
            "\"passed\"; executed=true; exit=0",
        ),
    ] {
        let server = command_task_server(json!(["/bin/bash", "-c", script, "get", url]), true);
        let mut child = command(project.path(), &server.url)
            .args(["--task", "Get the artifact", "--command-timeout", timeout])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // Leave stdin open without answering the destination prompt.
        child.stdin.as_mut().unwrap().write_all(b"y\n").unwrap();
        let status = finished(&mut child);
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        assert_eq!(status.code(), Some(expected_code), "{stdout}");
        assert_eq!(
            stdout.matches("Approve this action?").count(),
            2,
            "{stdout}"
        );
        assert!(stdout.contains(expected_result), "{stdout}");
        assert!(
            stdout.contains(&format!("Network GET \"{url}\" \"stopped\"")),
            "{stdout}"
        );
    }
    assert!(destination.requests.lock().unwrap().is_empty());
}

#[test]
fn create_file_preview_approval_defaults_and_eof() {
    for input in ["y\n", "n\n", "\n", "", "y"] {
        let server = Server::new(|p, b| {
            if p != "/api/chat" || b["messages"].as_array().unwrap().len() == 1 {
                ready(p, b)
            } else if b["messages"].as_array().unwrap().len() == 2 {
                tool(
                    "apply_patch",
                    json!({"path":"new.txt","old_text":null,"new_text":"hello\n"}),
                )
            } else {
                final_answer()
            }
        });
        let t = tempfile::tempdir().unwrap();
        let out = run(
            command(t.path(), &server.url).args(["--task", "create a file"]),
            input,
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let preview = stdout.find("Proposed patch").unwrap();
        let addition = stdout.find("+hello").unwrap();
        let approval = stdout.find("Approve this action? [y/N]").unwrap();
        assert!(preview < addition && addition < approval, "{stdout}");
        assert!(stdout.contains("6 UTF-8 bytes"));
        let approved = input == "y\n";
        assert_eq!(
            out.status.code(),
            Some(if approved { 0 } else { 1 }),
            "{stdout}"
        );
        assert_eq!(t.path().join("new.txt").exists(), approved);
        if approved {
            assert_eq!(
                fs::read_to_string(t.path().join("new.txt")).unwrap(),
                "hello\n"
            );
            assert!(stdout.contains("Modified files: [\"new.txt\"]"));
        } else {
            assert!(stdout.contains("denied"));
            assert!(stdout.contains("Modified files: []"));
            assert_eq!(fs::read_dir(t.path()).unwrap().count(), 0);
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn mixed_actions_require_separate_approval_and_preserve_committed_evidence() {
    let path = "new\u{1b}[31m.txt";
    let content = "before\u{202e}\n";
    let argv = json!(["/usr/bin/printf", "%s", "arg\n\"\u{1b}"]);
    let command_policy = argv.to_string();
    let results = Arc::new(Mutex::new(Vec::new()));
    let captured = results.clone();
    let server = Server::new(move |p, b| {
        if p != "/api/chat" || b["messages"].as_array().unwrap().len() == 1 {
            return ready(p, b);
        }
        if b["messages"].as_array().unwrap().len() == 2 {
            let calls = [
                ("apply_patch", json!({"path":path,"old_text":null,"new_text":content})),
                ("apply_patch", json!({"path":path,"old_text":"before","new_text":"after"})),
                ("run_command", json!({"argv":argv})),
            ].into_iter().enumerate().map(|(i, (name, arguments))| {
                json!({"id":format!("action-{i}"),"function":{"name":name,"arguments":arguments}})
            }).collect::<Vec<_>>();
            return Reply::json(
                json!({"done":true,"message":{"role":"assistant","content":"","tool_calls":calls}}),
            );
        }
        *captured.lock().unwrap() = b["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .cloned()
            .collect();
        final_answer()
    });
    let t = tempfile::tempdir().unwrap();
    // Consent covers only creation. EOF must independently deny patch and command.
    let out = run(
        command(t.path(), &server.url).args([
            "--task",
            "edit and verify",
            "--allow-command",
            &command_policy,
        ]),
        "y\n",
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(fs::read_to_string(t.path().join(path)).unwrap(), content);
    assert_eq!(fs::read_dir(t.path()).unwrap().count(), 1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.matches("Approve this action? [y/N]").count(), 3);
    let creation = stdout.find("Proposed patch").unwrap();
    let patch = stdout.rfind("Proposed patch").unwrap();
    let command = stdout.find("Proposed command").unwrap();
    assert!(creation < patch && patch < command);
    assert!(stdout.contains("argv = [\"/usr/bin/printf\",\"%s\",\"arg\\n\\\"\\u001b\"]"));
    assert!(!stdout.contains('\u{1b}') && !stdout.contains('\u{202e}'));
    assert!(stdout.contains("\\x1b") && stdout.contains("\\x202e"));
    assert!(stdout.contains(&format!("Modified files: {}", json!([path]))));
    let results = results.lock().unwrap();
    assert_eq!(results.len(), 3);
    for (i, message) in results.iter().enumerate() {
        assert_eq!(message["tool_call_id"], format!("action-{i}"));
        let result: Value = serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
        if i == 0 {
            assert_eq!(result["status"], "ok");
            assert_eq!(result["executed"], true);
            assert_eq!(result["sha256"], cognix::runtime::hash(content.as_bytes()));
        } else {
            assert_eq!(result["status"], "denied");
            assert_eq!(result["executed"], false);
        }
    }
}
fn finished(child: &mut Child) -> std::process::ExitStatus {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if Instant::now() > end {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("CLI did not terminate");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[cfg(target_os = "macos")]
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
    let out = run(&mut command(t.path(), &server.url), "verify\ny\n/exit\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("Proposed command:\nargv = [\"/usr/bin/true\"]"));
    assert!(s.contains("Approve this action?"));
    assert!(s.contains("Tool: run_command"));
    assert!(s.contains("executed=true"));
    assert!(s.contains("not verification evidence"));
    assert!(!s.contains('\u{1b}'));
    assert!(s.contains("\\x1b"));
    let requests = server.requests.lock().unwrap();
    let messages = requests.last().unwrap()["body"]["messages"]
        .as_array()
        .unwrap();
    assert!(
        !messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("allowlist")
    );
    let result: Value =
        serde_json::from_str(messages.last().unwrap()["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["executed"], true);
}
#[cfg(target_os = "macos")]
fn check_approved_hard_link_command(git_alias: bool) {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let source = if git_alias {
        fs::create_dir(project.path().join(".git")).unwrap();
        project.path().join(".git/config")
    } else {
        outside.path().join("secret")
    };
    let marker = "private hard-link contents";
    fs::write(&source, marker).unwrap();
    fs::create_dir(project.path().join("nested")).unwrap();
    fs::hard_link(&source, project.path().join("nested/alias")).unwrap();
    let argv = json!(["/bin/cat", "nested/alias"]);
    let request = argv.clone();
    let server = Server::new(move |p, b| {
        if p != "/api/chat" || b["messages"].as_array().unwrap().len() == 1 {
            ready(p, b)
        } else {
            match b["messages"].as_array().unwrap().len() {
                2 => tool("run_command", json!({"argv":request})),
                _ => final_answer(),
            }
        }
    });
    let out = run(
        command(project.path(), &server.url).args([
            "--task",
            "verify",
            "--allow-command",
            &argv.to_string(),
        ]),
        "y\n",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(stdout.contains("Approve this action?"), "{stdout}");
    assert!(!stdout.contains(marker), "{stdout}");
    assert!(!stderr.contains(marker), "{stderr}");
    let requests = server.requests.lock().unwrap();
    let results: Vec<Value> = requests.last().unwrap()["body"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| serde_json::from_str(m["content"].as_str().unwrap()).unwrap())
        .collect();
    let result = &results[0];
    assert_eq!(result["status"], "not_run", "{result}");
    assert_eq!(result["executed"], false, "{result}");
    assert!(
        result["error"].as_str().unwrap().contains("hard link"),
        "{result}"
    );
    assert!(result["exit_code"].is_null(), "{result}");
    assert!(result["output"].is_null(), "{result}");
    assert_eq!(fs::read_to_string(&source).unwrap(), marker);
}
#[cfg(target_os = "macos")]
#[test]
fn approved_command_rejects_outside_hard_link() {
    check_approved_hard_link_command(false);
}
#[cfg(target_os = "macos")]
#[test]
fn approved_command_rejects_git_hard_link() {
    check_approved_hard_link_command(true);
}
#[cfg(target_os = "macos")]
#[test]
fn approved_command_cannot_use_inherited_outside_descriptors() {
    use std::os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    };

    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret_path = outside.path().join("secret");
    fs::write(&secret_path, "outside descriptor marker").unwrap();
    let secret = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&secret_path)
        .unwrap();
    // Keep these CLOEXEC in the test runner so parallel tests cannot inherit them.
    // Only the Cognix subprocess clears the flags, simulating an embedding host.
    let duplicate = |min: i32| {
        let fd = unsafe { libc::fcntl(secret.as_raw_fd(), libc::F_DUPFD_CLOEXEC, min) };
        assert!(fd >= min);
        unsafe { OwnedFd::from_raw_fd(fd) }
    };
    let low = duplicate(3);
    let high = duplicate(4096);
    let fds = [low.as_raw_fd(), high.as_raw_fd()];
    let code = format!(
        "import errno,os\nfor fd in {fds:?}:\n for action in [lambda:os.read(fd,100),lambda:os.write(fd,b'corrupt'),lambda:os.fstat(fd)]:\n  try: action()\n  except OSError as e: assert e.errno==errno.EBADF,e\n  else: raise AssertionError('inherited outside handle accessible')\nassert os.read(0,1)==b''\nos.write(1,b'stdout preserved\\n')\nos.write(2,b'stderr preserved\\n')"
    );
    let argv = json!([common::FRAMEWORK_PYTHON, "-c", code]);
    let request = argv.clone();
    let server = Server::new(move |p, b| {
        if p != "/api/chat" || b["messages"].as_array().unwrap().len() == 1 {
            ready(p, b)
        } else if b["messages"].as_array().unwrap().len() == 2 {
            tool("run_command", json!({"argv":request}))
        } else {
            final_answer()
        }
    });
    let mut c = command(project.path(), &server.url);
    c.args(["--task", "verify", "--allow-command", &argv.to_string()]);
    // Change only the subprocess's limits. The high descriptor remains open even
    // above the hard limit, defeating sanitizers that sweep only to that limit.
    unsafe {
        c.pre_exec(move || {
            for fd in fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            let limit = libc::rlimit {
                rlim_cur: 256,
                rlim_max: 256,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = run(&mut c, "y\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Approve this action?"), "{stdout}");
    // Inspect the actual tool result sent back to the model, not its final prose.
    let requests = server.requests.lock().unwrap();
    let result = requests
        .iter()
        .flat_map(|r| r["body"]["messages"].as_array().into_iter().flatten())
        .find(|m| m["role"] == "tool" && m["tool_name"] == "run_command")
        .unwrap();
    let result: Value = serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["status"], "passed", "{result}");
    assert_eq!(result["executed"], true, "{result}");
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(result["output"], "stdout preserved\nstderr preserved\n");
    assert_eq!(
        fs::read_to_string(&secret_path).unwrap(),
        "outside descriptor marker"
    );
}
#[cfg(target_os = "macos")]
#[test]
fn runtime_command_denial_and_eof_leave_it_unexecuted() {
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
    for answer in ["n\n", "", "y"] {
        let out = run(
            command(t.path(), &server.url).args(["--task", "verify"]),
            answer,
        );
        assert_eq!(out.status.code(), Some(1));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("Proposed command"));
        assert!(
            stdout.contains("denied") && stdout.contains("executed=false"),
            "{stdout}"
        );
    }
}

#[test]
fn invalid_options_fail() {
    let t = tempfile::tempdir().unwrap();
    for (opts, detail) in [
        (vec!["--max-steps", "0"], "must be greater than zero"),
        (vec!["--num-ctx", "8191"], "must be at least 8192"),
        (vec!["--task", " "], "--task must contain a request"),
        (vec!["--root", "/missing-cognix-root"], "No such file"),
        (
            vec!["--allow-command", "[\"relative\"]"],
            "Command not found",
        ),
        (
            vec!["--command-timeout", "no"],
            "must be a positive integer",
        ),
        (vec!["--unknown"], "unexpected argument"),
    ] {
        let out = run(
            cli(t.path())
                .current_dir(t.path())
                .args(["--ollama-url", "http://127.0.0.1:1"])
                .args(opts),
            "",
        );
        let message = String::from_utf8_lossy(&out.stderr).into_owned()
            + &String::from_utf8_lossy(&out.stdout);
        assert!(
            !out.status.success() && message.contains(detail),
            "{message}"
        );
    }
}
#[test]
fn startup_configuration_precedence_from_a_project_directory() {
    let handler = |p: &str, b: &Value| {
        if p == "/api/chat" && b["messages"].as_array().unwrap().len() > 1 {
            final_answer()
        } else {
            ready(p, b)
        }
    };
    let config_server = Server::new(handler);
    let env_server = Server::new(handler);
    let flag_server = Server::new(handler);
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let config = home.path().join(".cognix");
    fs::create_dir(&config).unwrap();
    // With no settings file, only the endpoint flag is needed for the fixture.
    let mut defaults = cli(home.path());
    defaults.args(["--ollama-url", &flag_server.url]);
    let out = run(defaults.current_dir(project.path()), "/exit\n");
    assert!(out.status.success());
    let requests = flag_server.requests.lock().unwrap();
    assert_eq!(requests.last().unwrap()["body"]["model"], "qwen3-coder:30b");
    assert_eq!(
        requests.last().unwrap()["body"]["options"]["num_ctx"],
        16384
    );
    drop(requests);

    fs::write(
        config.join("config.toml"),
        format!(
            r#"
model = "saved:local"
ollama_url = "{} "
num_ctx = 12288
"#,
            config_server.url
        ),
    )
    .unwrap();
    // A project file cannot supply user configuration.
    fs::create_dir(project.path().join(".cognix")).unwrap();
    fs::write(project.path().join(".cognix/config.toml"), "broken").unwrap();

    for (layer, server, model, ctx) in [
        ("file", &config_server, "saved:local", 12288),
        ("env", &env_server, "env:local", 12288),
        ("flag", &flag_server, "flag:local", 16384),
    ] {
        let mut command = cli(home.path());
        command.current_dir(project.path());
        if layer != "file" {
            command
                .env("COGNIX_MODEL", "env:local")
                .env("COGNIX_OLLAMA_URL", &env_server.url);
        }
        if layer == "flag" {
            command.args([
                "--model",
                model,
                "--ollama-url",
                &server.url,
                "--num-ctx",
                "16384",
            ]);
        }
        let out = run(&mut command, "inspect\n/exit\n");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains(&format!(
                "Project: {}",
                project.path().canonicalize().unwrap().display()
            )),
            "{stdout}"
        );
        assert!(stdout.contains("Verification: not run"));
        let requests = server.requests.lock().unwrap();
        let body = &requests.last().unwrap()["body"];
        assert_eq!(body["model"], model);
        assert_eq!(body["options"]["num_ctx"], ctx);
        assert_eq!(body["messages"][1]["content"], "inspect");
    }
}

#[test]
fn relative_home_does_not_load_project_configuration() {
    let server = Server::new(ready);
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join(".cognix")).unwrap();
    fs::write(
        project.path().join(".cognix/config.toml"),
        "model = 'project:local'",
    )
    .unwrap();

    let out = run(
        cli(Path::new("."))
            .current_dir(project.path())
            .args(["--ollama-url", &server.url]),
        "/exit\n",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.last().unwrap()["body"]["model"], "qwen3-coder:30b");
}

#[test]
fn configuration_fifo_is_rejected_without_a_writer() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".cognix");
    fs::create_dir(&dir).unwrap();
    let config = dir.join("config.toml");
    let fifo = std::ffi::CString::new(config.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    let mut child = cli(home.path())
        .args(["--ollama-url", "http://127.0.0.1:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(!finished(&mut child).success());
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.stdout.is_empty(),
        "invalid config must fail before startup"
    );
    assert!(
        stderr.contains(config.to_str().unwrap()) && stderr.contains("expected a regular file"),
        "{stderr}"
    );
}

#[test]
fn invalid_user_configuration_names_the_file_and_setting() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".cognix");
    fs::create_dir(&dir).unwrap();
    let config = dir.join("config.toml");
    for (contents, detail) in [
        ("model = [", "TOML parse error"),
        ("modle = 'typo'", "unknown field"),
        ("model = ''", "nonempty local model tag"),
        ("ollama_url = 'https://example.com'", "local HTTP origin"),
        ("approval_mode = 'unsafe'", "approval_mode"),
        ("sandbox_mode = 'arbitrary'", "sandbox_mode"),
        ("review_model = 'remote-cloud'", "review_model"),
        (
            "deny_network = ['https://example.com/path']",
            "deny_network",
        ),
        ("max_steps = 0", "max_steps must be greater than zero"),
        ("num_ctx = 8191", "num_ctx must be at least 8192"),
        (
            "model_timeout = 86401",
            "model_timeout must be at most 86400 seconds",
        ),
        ("command_timeout = '30'", "invalid type"),
        ("output_limit = 0", "output_limit must be greater than zero"),
    ] {
        fs::write(&config, contents).unwrap();
        let out = run(&mut cli(home.path()), "");
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(config.to_str().unwrap()) && stderr.contains(detail),
            "{stderr}"
        );
        assert!(
            out.stdout.is_empty(),
            "invalid config must fail before startup"
        );
    }
    assert!(
        cli(home.path())
            .arg("--help")
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
            common::FRAMEWORK_PYTHON,
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
                0 => tool(
                    "apply_patch",
                    json!({"path":"file","old_text":"before","new_text":"after"}),
                ),
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
            if *stage.lock().unwrap() >= 2 {
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
