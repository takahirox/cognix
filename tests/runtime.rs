use cognix::{Cancellation, runtime::Runtime};
use serde_json::{Value, json};
use std::{cell::RefCell, fs, os::unix::fs::symlink, rc::Rc};

fn setup() -> (tempfile::TempDir, Runtime) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("main.py"),
        "# user note\ndef divide(a, b):\n    return a / b\n",
    )
    .unwrap();
    let runtime =
        Runtime::new(temp.path(), Box::new(|_, _| true), Cancellation::default()).unwrap();
    (temp, runtime)
}
fn edit(r: &mut Runtime, overrides: Value) -> Value {
    let mut a = json!({"path":"main.py","old_text":"return a / b","new_text":"return a / b if b else None"});
    for (k, v) in overrides.as_object().unwrap() {
        a[k] = v.clone();
    }
    r.execute("apply_patch", a)
}
#[test]
fn preview_and_unrelated_changes() {
    let (t, mut r) = setup();
    fs::write(t.path().join("unrelated"), "uncommitted").unwrap();
    let path = t.path().join("main.py");
    r.approve = Box::new(move |kind, preview| {
        assert_eq!(kind, "patch");
        assert!(preview.contains("-    return a / b"));
        assert!(!fs::read_to_string(&path).unwrap().contains("if b"));
        true
    });
    assert_eq!(edit(&mut r, json!({}))["status"], "ok");
    assert!(
        fs::read_to_string(t.path().join("main.py"))
            .unwrap()
            .starts_with("# user note")
    );
    assert_eq!(
        fs::read_to_string(t.path().join("unrelated")).unwrap(),
        "uncommitted"
    );
    assert!(r.modified.contains("main.py"));
}
#[test]
fn denial_and_cancellation_at_approval() {
    let (t, mut r) = setup();
    let original = fs::read(t.path().join("main.py")).unwrap();
    r.approve = Box::new(|_, _| false);
    assert_eq!(edit(&mut r, json!({}))["status"], "denied");
    let cancel = r.cancel.clone();
    r.approve = Box::new(move |_, _| {
        cancel.cancel();
        true
    });
    assert_eq!(edit(&mut r, json!({}))["status"], "canceled");
    assert_eq!(fs::read(t.path().join("main.py")).unwrap(), original);
    assert!(r.modified.is_empty());
}
#[test]
fn concurrent_edit_and_parent_swap_are_preserved() {
    let (t, mut r) = setup();
    let path = t.path().join("main.py");
    r.approve = Box::new(move |_, _| {
        fs::write(&path, "new user content").unwrap();
        true
    });
    assert_eq!(edit(&mut r, json!({}))["status"], "failed");
    assert_eq!(
        fs::read_to_string(t.path().join("main.py")).unwrap(),
        "new user content"
    );
    fs::create_dir(t.path().join("dir")).unwrap();
    fs::write(t.path().join("dir/file"), "before").unwrap();
    let root = t.path().to_owned();
    r.approve = Box::new(move |_, _| {
        fs::rename(root.join("dir"), root.join("old")).unwrap();
        symlink(root.join("old"), root.join("dir")).unwrap();
        true
    });
    assert_eq!(
        r.execute(
            "apply_patch",
            json!({"path":"dir/file","old_text":"before","new_text":"after"})
        )["status"],
        "failed"
    );
    assert_eq!(
        fs::read_to_string(t.path().join("old/file")).unwrap(),
        "before"
    );
}
#[test]
fn root_symlink_hardlink_and_git_boundaries() {
    let (t, mut r) = setup();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("secret");
    fs::write(&target, "outside").unwrap();
    symlink(&target, t.path().join("escape")).unwrap();
    symlink(outside.path(), t.path().join("linkdir")).unwrap();
    fs::hard_link(&target, t.path().join("hardlink")).unwrap();
    fs::create_dir(t.path().join(".git")).unwrap();
    fs::write(t.path().join(".git/config"), "outside").unwrap();
    fs::create_dir(t.path().join("nested")).unwrap();
    fs::write(t.path().join("nested/.GiT"), "outside").unwrap();
    let traversal = format!(
        "../{}/secret",
        outside.path().file_name().unwrap().to_str().unwrap()
    );
    r.approve = Box::new(|_, _| panic!("Unsafe path must not reach approval"));
    for p in [
        target.to_str().unwrap(),
        &traversal,
        "a/../../secret",
        "escape",
        "linkdir/secret",
        "hardlink",
        ".git/config",
        ".GIT/config",
        "nested/.GiT",
    ] {
        let result = r.execute(
            "apply_patch",
            json!({"path":p,"old_text":"outside","new_text":"bad"}),
        );
        assert_eq!(result["status"], "failed", "{p}: {result}");
    }
    assert_eq!(fs::read_to_string(target).unwrap(), "outside");
}
#[test]
fn invalid_schema_and_file_types() {
    let (t, mut r) = setup();
    for (n, a) in [
        ("unknown", json!({})),
        ("list_files", json!({"path":"."})),
        ("read_file", json!({"path":"main.py"})),
        ("create_file", json!({"path":"new","content":"text"})),
        (
            "apply_patch",
            json!({"path":2,"old_text":null,"new_text":"text"}),
        ),
        ("run_command", json!({"argv":"sh"})),
        ("run_command", json!({"argv":[]})),
        ("run_command", json!({"argv":["x\0y"]})),
        ("run_command", json!({"argv":vec!["x";65]})),
    ] {
        assert_ne!(r.execute(n, a)["status"], "ok");
    }
    for (name, data, detail) in [
        ("large", vec![b'x'; 65537], "exceeds 64 KiB"),
        ("binary", vec![0], "Binary files"),
        ("bad_utf8", vec![255], "invalid utf-8"),
    ] {
        fs::write(t.path().join(name), data).unwrap();
        let result = edit(&mut r, json!({"path":name}));
        assert_eq!(result["status"], "failed");
        assert!(
            result["error"].as_str().unwrap().contains(detail),
            "{result}"
        );
    }
    let fifo = std::ffi::CString::new(t.path().join("fifo").to_str().unwrap()).unwrap();
    unsafe {
        assert_eq!(libc::mkfifo(fifo.as_ptr(), 0o600), 0);
    }
    let result = edit(&mut r, json!({"path":"fifo"}));
    assert_eq!(result["status"], "failed");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("Only regular files")
    );
    assert!(
        edit(&mut r, json!({"path":"missing"}))["error"]
            .as_str()
            .unwrap()
            .contains("No such file")
    );
}
#[test]
fn patch_preconditions_staleness_and_bounds() {
    let (t, mut r) = setup();
    let previews = Rc::new(RefCell::new(0));
    let count = previews.clone();
    r.approve = Box::new(move |_, _| {
        *count.borrow_mut() += 1;
        true
    });
    for a in [
        json!({"old_text":""}),
        json!({"old_text":"missing"}),
        json!({"sha256":"bad"}),
        json!({"new_text":"x".repeat(65537)}),
        json!({"new_text":"\0"}),
    ] {
        assert_eq!(edit(&mut r, a)["status"], "failed");
    }
    fs::write(t.path().join("main.py"), "あああ").unwrap();
    assert_eq!(edit(&mut r, json!({"old_text":"ああ"}))["status"], "failed");
    assert_eq!(*previews.borrow(), 0);
    fs::write(t.path().join("main.py"), "").unwrap();
    let result = edit(&mut r, json!({"old_text":"","new_text":"first line\n"}));
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(
        fs::read_to_string(t.path().join("main.py")).unwrap(),
        "first line\n"
    );
}
#[test]
fn exact_command_policy_and_denial() {
    let (_t, mut r) = setup();
    r.allowed_commands = vec![vec!["true".into()]];
    r.approve = Box::new(|_, _| panic!("Commands outside policy must not reach approval"));
    for argv in [json!(["true", "different"]), json!(["/usr/bin/true"])] {
        let result = r.execute("run_command", json!({"argv":argv}));
        assert_eq!(result["status"], "not_run");
        assert!(result["error"].as_str().unwrap().contains("allow-command"));
    }
    r.approve = Box::new(|_, _| false);
    let denied = r.execute("run_command", json!({"argv":["true"]}));
    assert_eq!(denied["executed"], false);
    assert_eq!(
        denied["status"],
        if cfg!(target_os = "macos") {
            "denied"
        } else {
            "not_run"
        }
    );
}

#[cfg(target_os = "macos")]
#[test]
fn unrestricted_proposals_reject_project_programs_and_unknown_names() {
    use std::os::unix::fs::PermissionsExt;

    let (t, mut r) = setup();
    let program = t.path().join("cognix-test-project-program");
    fs::copy("/usr/bin/true", &program).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    r.approve = Box::new(|_, _| panic!("Invalid executable must not reach approval"));
    for executable in [
        "cognix-test-project-program",
        "./cognix-test-project-program",
        "/usr/bin",
        program.to_str().unwrap(),
    ] {
        let result = r.execute("run_command", json!({"argv":[executable]}));
        assert_eq!(result["status"], "not_run");
        assert_eq!(result["executed"], false);
        assert!(result["error"].is_string(), "{result}");
    }

    // Even an executable that shadows a valid bare name must be ignored.
    fs::copy("/usr/bin/false", t.path().join("true")).unwrap();
    fs::set_permissions(t.path().join("true"), fs::Permissions::from_mode(0o755)).unwrap();
    r.approve = Box::new(|_, preview| {
        assert!(preview.contains("Resolved executable = \"/usr/bin/true\""));
        true
    });
    let result = r.execute("run_command", json!({"argv":["true"]}));
    assert_eq!(result["status"], "passed", "{result}");
}

#[cfg(target_os = "macos")]
#[test]
fn approved_python_alias_uses_the_project_virtual_environment() {
    let (t, mut r) = setup();
    let venv = t.path().canonicalize().unwrap().join(".venv");
    fs::create_dir_all(venv.join("bin")).unwrap();
    fs::write(
        venv.join("pyvenv.cfg"),
        "home = /opt/homebrew/bin\ninclude-system-site-packages = false\n",
    )
    .unwrap();
    let python = venv.join("bin/python3");
    symlink("/opt/homebrew/bin/python3", &python).unwrap();
    let argv = vec![
        python.to_str().unwrap().into(),
        "-c".into(),
        "import sys; print(sys.prefix)".into(),
    ];
    r.allowed_commands = vec![argv.clone()];

    let result = r.execute("run_command", json!({"argv":argv}));
    assert_eq!(result["status"], "passed", "{result}");
    assert_eq!(result["output"], format!("{}\n", venv.display()));
}

#[cfg(target_os = "macos")]
#[test]
fn approval_is_bound_to_the_resolved_executable() {
    use std::os::unix::fs::symlink;

    let (t, mut r) = setup();
    let alias = t.path().join("command");
    symlink("/usr/bin/true", &alias).unwrap();
    let args = json!({"argv":[alias]});
    r.allowed_commands = vec![vec![alias.to_str().unwrap().into()]];
    r.approve = Box::new(|_, _| panic!("The resolved argv is outside the submitted allowlist"));
    let result = r.execute("run_command", json!({"argv":["/usr/bin/true"]}));
    assert_eq!(result["status"], "not_run");
    assert!(result["error"].as_str().unwrap().contains("allow-command"));

    let approve = |preview: &str| {
        assert!(
            preview.contains("Resolved executable = \"/usr/bin/true\""),
            "{preview}"
        );
        true
    };
    r.approve = Box::new(move |_, preview| approve(preview));
    let result = r.execute("run_command", args.clone());
    assert_eq!(result["status"], "passed", "{result}");

    r.approve = Box::new(move |_, preview| {
        assert!(approve(preview));
        fs::remove_file(&alias).unwrap();
        symlink("/usr/bin/false", &alias).unwrap();
        true
    });
    let result = r.execute("run_command", args);
    assert_eq!(result["status"], "not_run", "{result}");
    assert_eq!(result["executed"], false);
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("executable changed")
    );
}

#[test]
fn shared_approval_preserves_previews_and_stopped_result_shapes() {
    let mut tools = vec!["update", "create"];
    if cfg!(target_os = "macos") {
        tools.push("run_command");
    }
    for tool in tools {
        // A canceled callback must stop execution whether it answered yes or no.
        for (answer, interrupt) in [(false, false), (false, true), (true, true)] {
            let t = tempfile::tempdir().unwrap();
            fs::write(t.path().join("file"), "before\n").unwrap();
            let cancel = Cancellation::default();
            let mut r = Runtime::new(t.path(), Box::new(|_, _| true), cancel.clone()).unwrap();
            r.allowed_commands = vec![vec!["/usr/bin/true".into()]];
            let command_preview = format!(
                "argv = [\"/usr/bin/true\"]\nWorking directory = {:?}\nWrites to the live project and private scratch; no network or .git access. Child processes inherit the sandbox.",
                r.root
            );
            let (args, expected_kind, expected_preview) = match tool {
                "update" => (
                    json!({"path":"./file","old_text":"before","new_text":"after"}),
                    "patch",
                    "--- file\n+++ file\n@@ -1 +1 @@\n-before\n+after\n\nExact replacement: \"before\" -> \"after\"",
                ),
                "create" => (
                    json!({"path":"./new","old_text":null,"new_text":"after\n"}),
                    "patch",
                    "Create \"new\" (6 UTF-8 bytes)\n--- /dev/null\n+++ \"new\"\n+after\n",
                ),
                _ => (
                    json!({"argv":["/usr/bin/true"]}),
                    "command",
                    command_preview.as_str(),
                ),
            };
            let expected_preview = expected_preview.to_owned();
            let count = Rc::new(RefCell::new(0));
            let calls = count.clone();
            let root = t.path().to_owned();
            r.approve = Box::new(move |kind, preview| {
                *calls.borrow_mut() += 1;
                assert_eq!(kind, expected_kind);
                assert_eq!(preview, expected_preview);
                // Preparation must leave neither targets nor staging effects.
                assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
                assert_eq!(fs::read_to_string(root.join("file")).unwrap(), "before\n");
                if interrupt {
                    cancel.cancel();
                }
                answer
            });
            let name = if tool == "run_command" {
                tool
            } else {
                "apply_patch"
            };
            let result = r.execute(name, args.clone());
            let mut expected =
                json!({"status":if interrupt {"canceled"} else {"denied"},"executed":false});
            if tool != "run_command" {
                expected["path"] = json!(if tool == "create" { "new" } else { "file" });
            }
            assert_eq!(result, expected, "{tool}");
            assert_eq!(*count.borrow(), 1);
            assert!(r.modified.is_empty());
            assert_eq!(fs::read_dir(t.path()).unwrap().count(), 1);
            assert_eq!(r.events.len(), 1);
            assert_eq!(
                r.events[0],
                json!({"tool":name,"arguments":args,"result":expected})
            );
        }
    }
}
