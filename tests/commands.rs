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
        "[\"python\"]",
        "[\"/missing\"]",
        "[\"/bin/sh\",\"\"]",
    ] {
        assert!(parse_command(s).is_err(), "{s}");
    }
    assert_eq!(
        parse_command("[\"/bin/sh\",\"-c\",\"echo x\"]").unwrap(),
        vec!["/bin/sh", "-c", "echo x"]
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
