#[cfg(not(target_os = "macos"))]
use cognix::commands::sandbox_argv;
#[cfg(target_os = "macos")]
mod common;
use cognix::{
    Cancellation,
    commands::{parse_command, run},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};
fn execute(
    root: &Path,
    code: &str,
    cancel: &Cancellation,
    timeout: Duration,
    limit: usize,
) -> Value {
    run(
        &root.canonicalize().unwrap(),
        &["/opt/homebrew/bin/python3".into(), "-c".into(), code.into()],
        cancel,
        timeout,
        limit,
    )
    .unwrap()
}
#[test]
fn invalid_command_policy() {
    for s in [
        "[]",
        "{}",
        "[2]",
        "[\"cognix-test-missing-command\"]",
        "[\"./cat\"]",
        "[\"/missing\"]",
        "[\"/bin/sh\",\"\"]",
    ] {
        assert!(parse_command(s).is_err(), "{s}");
    }
    assert_eq!(
        parse_command("[\"/bin/sh\",\"-c\",\"echo x\"]").unwrap(),
        vec!["/bin/sh", "-c", "echo x"]
    );
    assert_eq!(
        parse_command("[\"cat\",\"main.py\"]").unwrap(),
        ["cat", "main.py"]
    );
}
#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_fails_closed() {
    assert!(
        sandbox_argv(
            Path::new("/project"),
            Path::new("/scratch"),
            &["/bin/true".into()]
        )
        .is_err()
    );
}
#[cfg(target_os = "macos")]
#[test]
fn real_sandbox_isolation_and_environment() {
    let t = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret");
    fs::write(&secret, "private").unwrap();
    std::os::unix::fs::symlink(&secret, t.path().join("link")).unwrap();
    fs::create_dir(t.path().join(".git")).unwrap();
    fs::write(t.path().join(".git/config"), "metadata").unwrap();
    let cancel = Cancellation::default();
    let exec = |code: &str| execute(t.path(), code, &cancel, Duration::from_secs(5), 16384);
    assert_eq!(exec("print('yes')")["output"], "yes\n");
    assert_eq!(exec("raise SystemExit(3)")["exit_code"], 3);
    for code in [
        format!("open({}).read()", json!(secret)),
        "open('link').read()".into(),
        "open('created','w').write('bad')".into(),
        format!("open({},'w').write('bad')", json!(secret)),
        "import socket; socket.create_connection(('127.0.0.1',11434),timeout=1)".into(),
        "open('.git/config').read()".into(),
        "open('.GIT/config').read()".into(),
        "import os;os.fork()".into(),
        "import subprocess;subprocess.run(['/usr/bin/true'],check=True)".into(),
    ] {
        let r = exec(&code);
        assert_eq!(r["status"], "failed", "{code}: {r}");
        assert_eq!(r["executed"], true);
    }
    assert_eq!(fs::read_to_string(secret).unwrap(), "private");
    assert!(!t.path().join("created").exists());
    let result = exec(
        "import os,tempfile;assert set(os.environ)<=set('PATH HOME TMPDIR TMP TEMP LANG PYTHONDONTWRITEBYTECODE PYTHONNOUSERSITE GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL GIT_TERMINAL_PROMPT LC_CTYPE'.split());f=tempfile.TemporaryFile();f.write(b'ok');print('scratch')",
    );
    assert_eq!(result["status"], "passed", "{result}");
}
#[cfg(target_os = "macos")]
#[test]
fn time_output_and_closed_pipe_limits() {
    let t = tempfile::tempdir().unwrap();
    for code in [
        "import time;time.sleep(20)",
        "import os,time;os.close(1);os.close(2);time.sleep(20)",
    ] {
        let start = Instant::now();
        let r = execute(
            t.path(),
            code,
            &Cancellation::default(),
            Duration::from_millis(200),
            16384,
        );
        assert_eq!(r["status"], "limit_reached", "{r}");
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(r["executed"], true);
    }
    let r = execute(
        t.path(),
        "print('x'*100000)",
        &Cancellation::default(),
        Duration::from_secs(5),
        100,
    );
    assert_eq!(r["status"], "limit_reached");
    assert!(r["output"].as_str().unwrap().len() <= 100);
    let r = execute(
        t.path(),
        "import os;os.write(1,bytes([255])*1000)",
        &Cancellation::default(),
        Duration::from_secs(5),
        100,
    );
    assert_eq!(r["status"], "limit_reached");
    assert!(r["output"].as_str().unwrap().len() <= 100);
}
#[cfg(target_os = "macos")]
#[test]
fn cancellation_reaps_and_preserves_launch_evidence() {
    let t = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let timer = cancel.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        timer.cancel();
    });
    let r = execute(
        t.path(),
        "import time;time.sleep(20)",
        &cancel,
        Duration::from_secs(10),
        16384,
    );
    worker.join().unwrap();
    assert_eq!(r["status"], "canceled");
    assert_eq!(r["executed"], true);
    assert_eq!(r["exit_code"], -libc::SIGKILL);
    assert_eq!(
        unsafe { libc::kill(r["pid"].as_i64().unwrap() as i32, 0) },
        -1
    );
    let r = execute(
        t.path(),
        "raise Exception('should not launch')",
        &cancel,
        Duration::from_secs(5),
        16384,
    );
    assert_eq!(r, json!({"status":"canceled","executed":false}));
}

#[cfg(target_os = "macos")]
#[test]
fn workspace_write_runs_build_children_and_enforces_file_and_network_boundaries() {
    use cognix::runtime::{Runtime, SandboxMode};
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().canonicalize().unwrap().join("secret");
    let nonexistent = outside.path().canonicalize().unwrap().join("new");
    fs::write(&secret, "private").unwrap();
    fs::create_dir(project.path().join(".git")).unwrap();
    std::os::unix::fs::symlink(&secret, project.path().join("outside-link")).unwrap();
    fs::write(
        project.path().join("verify.sh"),
        "set -e\n/bin/mkdir -p build\n/bin/echo built > build/result\n/bin/cat build/result\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(
        project.path(),
        Box::new(|_, _| true),
        Cancellation::default(),
    )
    .unwrap();
    let build = json!({"argv":["/bin/bash","verify.sh"]});
    let result = runtime.execute("run_command", build);
    assert_eq!(result["status"], "passed", "{result}");
    assert_eq!(
        fs::read_to_string(project.path().join("build/result")).unwrap(),
        "built\n"
    );

    let endpoint = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    endpoint.set_nonblocking(true).unwrap();
    let probe = r#"
import os, sys
pid = os.posix_spawn(sys.executable, [sys.executable, '-c', sys.argv[1]], os.environ)
_, status = os.waitpid(pid, 0)
assert os.waitstatus_to_exitcode(status) == 1
"#;
    for code in [
        format!("open({}).read()", json!(secret)),
        format!("open({},'w').write('bad')", json!(secret)),
        format!("open({},'w').write('bad')", json!(nonexistent)),
        format!("import os; os.link({}, 'outside-hardlink')", json!(secret)),
        "open('outside-link','w').write('bad')".into(),
        "open('.git/config','w').write('bad')".into(),
        "import os; os.rename('.git','metadata')".into(),
        format!(
            "import socket; socket.create_connection(('127.0.0.1',{}), timeout=1)",
            endpoint.local_addr().unwrap().port()
        ),
    ] {
        let result = runtime.execute(
            "run_command",
            json!({"argv":[common::FRAMEWORK_PYTHON,"-c",probe,code]}),
        );
        assert_eq!(result["status"], "passed", "{code} {result}");
        assert!(
            result["output"]
                .as_str()
                .unwrap()
                .contains("PermissionError:"),
            "{code} {result}"
        );
    }
    assert!(endpoint.accept().is_err());
    assert_eq!(fs::read_to_string(secret).unwrap(), "private");
    assert!(!nonexistent.exists());
    assert!(project.path().join(".git").is_dir());
    assert!(!project.path().join(".git/config").exists());

    runtime.sandbox_mode = SandboxMode::ReadOnly;
    let result = runtime.execute(
        "run_command",
        json!({"argv":["/bin/mkdir","readonly-output"]}),
    );
    assert_eq!(result["status"], "failed", "{result}");
    assert!(!project.path().join("readonly-output").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn fixed_inspection_bypasses_approval_but_options_and_hard_policy_do_not() {
    use cognix::runtime::Runtime;
    let project = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new(
        project.path(),
        Box::new(|_, _| false),
        Cancellation::default(),
    )
    .unwrap();
    assert_eq!(
        runtime.execute("run_command", json!({"argv":["ls"]}))["status"],
        "passed"
    );
    assert_eq!(
        runtime.execute("run_command", json!({"argv":["ls","-l"]}))["status"],
        "denied"
    );
    runtime.allowed_commands = vec![vec!["cat".into(), "file".into()]];
    assert_eq!(
        runtime.execute("run_command", json!({"argv":["ls"]}))["status"],
        "not_run"
    );
}

#[test]
fn full_access_requires_opt_in_and_uses_the_project_executable() {
    use cognix::runtime::{ApprovalMode, Runtime};
    use std::os::unix::fs::PermissionsExt;

    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let destination = outside.path().join("full-access-only");
    let program = project.path().join("local-script");
    fs::write(&program, "#!/bin/sh\nprintf confirmed > \"$1\"\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let mut runtime = Runtime::new(
        project.path(),
        Box::new(|_, _| panic!("unexpected approval")),
        Cancellation::default(),
    )
    .unwrap();
    runtime.allowed_commands = vec![vec!["/usr/bin/true".into()]];
    let args = json!({"argv":["./local-script",destination]});
    assert_eq!(
        runtime.execute("run_command", args.clone())["status"],
        "not_run"
    );
    assert!(!destination.exists());
    runtime.approval_mode = ApprovalMode::FullAccess;
    let result = runtime.execute("run_command", args);
    assert_eq!(result["status"], "passed", "{result}");
    assert_eq!(fs::read_to_string(destination).unwrap(), "confirmed");
}

#[cfg(target_os = "macos")]
#[test]
fn workspace_cleanup_stops_detached_writers_after_exit_and_cancellation() {
    use cognix::runtime::Runtime;

    let code = r#"
import os, sys, time
from pathlib import Path

writer = '''
import os, sys, time
from pathlib import Path

# Reparent before signaling readiness, keeping stdout and stderr open.
if os.fork() != 0:
    os._exit(0)
name = sys.argv[1]
while os.getppid() != 1:
    time.sleep(0.005)
assert os.getpgrp() != int(sys.argv[2])
with open(name + '.writes', 'wb', buffering=0) as output:
    output.write(b'x')
    Path(name).touch()
    for _ in range(300):
        time.sleep(0.01)
        output.write(b'x')
'''
for name, flags in [('session', {'setsid': True}), ('group', {'setpgroup': 0})]:
    pid = os.posix_spawn(sys.executable, [sys.executable, '-c', writer, name, str(os.getpgrp())], os.environ, **flags)
    assert os.waitpid(pid, 0)[1] == 0
deadline = time.monotonic() + 3
while not all(Path(name).is_file() for name in ('session', 'group')):
    assert time.monotonic() < deadline
    time.sleep(0.005)
if sys.argv[1] == 'wait':
    time.sleep(5)
print('parent stdout', flush=True)
print('x' * 8192, file=sys.stderr, flush=True)
sys.exit(23)
"#;
    for (action, status, exit_code) in
        [("exit", "failed", 23), ("wait", "canceled", -libc::SIGKILL)]
    {
        let project = tempfile::tempdir().unwrap();
        let cancel = Cancellation::default();
        let mut runtime =
            Runtime::new(project.path(), Box::new(|_, _| true), cancel.clone()).unwrap();
        runtime.timeout = Duration::from_secs(2);
        let root = project.path().to_owned();
        let timer = (action == "wait").then(|| {
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(2);
                let started = || {
                    ["session", "group"]
                        .iter()
                        .all(|name| root.join(name).is_file())
                };
                while !started() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                cancel.cancel();
                assert!(
                    started(),
                    "detached children did not start before cancellation"
                );
            })
        });
        let result = runtime.execute(
            "run_command",
            json!({"argv":[common::FRAMEWORK_PYTHON,"-c",code,action]}),
        );
        if let Some(timer) = timer {
            timer.join().unwrap();
        }
        assert_eq!(result["status"], status, "{result}");
        assert_eq!(result["executed"], true);
        assert_eq!(result["exit_code"], exit_code);
        if action == "exit" {
            assert_eq!(
                result["output"],
                format!("parent stdout\n{}\n", "x".repeat(8192))
            );
        }
        let lengths = || {
            ["session.writes", "group.writes"]
                .map(|name| fs::metadata(project.path().join(name)).unwrap().len())
        };
        let at_return = lengths();
        assert!(at_return.iter().all(|&len| len < 301));
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(lengths(), at_return);
    }
}
