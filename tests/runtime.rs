use cognix::{
    Cancellation,
    runtime::{Runtime, hash},
};
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
    let read = r.execute("read_file", json!({"path":"main.py"}));
    let mut a = json!({"path":"main.py","sha256":read["sha256"],"old_text":"return a / b","new_text":"return a / b if b else None"});
    for (k, v) in overrides.as_object().unwrap() {
        a[k] = v.clone();
    }
    r.execute("apply_patch", a)
}
#[test]
fn preview_and_unrelated_changes() {
    let (t, mut r) = setup();
    fs::write(t.path().join("unrelated"), "uncommitted").unwrap();
    assert!(
        r.execute("list_files", json!({"path":"."}))["entries"]
            .as_array()
            .unwrap()
            .contains(&json!("main.py"))
    );
    // Repeated root listing must not share a consumed directory offset.
    assert!(
        r.execute("list_files", json!({"path":"."}))["entries"]
            .as_array()
            .unwrap()
            .contains(&json!("main.py"))
    );
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
    let read = r.execute("read_file", json!({"path":"dir/file"}));
    let root = t.path().to_owned();
    r.approve = Box::new(move |_, _| {
        fs::rename(root.join("dir"), root.join("old")).unwrap();
        symlink(root.join("old"), root.join("dir")).unwrap();
        true
    });
    assert_eq!(r.execute("apply_patch",json!({"path":"dir/file","sha256":read["sha256"],"old_text":"before","new_text":"after"}))["status"],"failed");
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
    fs::write(t.path().join(".git/config"), "git config").unwrap();
    fs::create_dir(t.path().join("nested")).unwrap();
    fs::write(t.path().join("nested/.GiT"), "gitdir: elsewhere").unwrap();
    for p in [
        target.to_str().unwrap(),
        "../secret",
        "a/../../secret",
        "escape",
        "linkdir/secret",
        "hardlink",
        ".git/config",
        ".GIT/config",
        "nested/.GiT",
    ] {
        assert_eq!(
            r.execute("read_file", json!({"path":p}))["status"],
            "failed",
            "{p}"
        );
    }
    for p in [".git/config", ".GIT/config", "nested/.GiT"] {
        r.seen.insert(p.into(), hash(b"git config"));
        assert_eq!(r.execute("apply_patch",json!({"path":p,"sha256":hash(b"git config"),"old_text":"git config","new_text":"bad"}))["status"],"failed");
        assert_eq!(
            r.execute("list_files", json!({"path":p}))["status"],
            "failed"
        );
    }
    assert_eq!(
        r.execute("list_files", json!({"path":"linkdir"}))["status"],
        "failed"
    );
    let list = r.execute("list_files", json!({"path":"."}));
    let entries = list["entries"].as_array().unwrap();
    assert!(!entries.contains(&json!("escape")));
    assert!(!entries.contains(&json!(".git/")));
    assert_eq!(fs::read_to_string(target).unwrap(), "outside");
}
#[test]
fn invalid_schema_and_file_types() {
    let (t, mut r) = setup();
    for (n, a) in [
        ("unknown", json!({})),
        ("read_file", json!({})),
        ("read_file", json!({"path":2})),
        ("list_files", json!({"path":".","extra":true})),
        ("run_command", json!({"argv":"sh"})),
        ("run_command", json!({"argv":[]})),
        ("run_command", json!({"argv":["x\0y"]})),
        ("run_command", json!({"argv":vec!["x";65]})),
    ] {
        assert_ne!(r.execute(n, a)["status"], "ok");
    }
    for (name, data) in [
        ("large", vec![b'x'; 65537]),
        ("binary", vec![0]),
        ("bad_utf8", vec![255]),
    ] {
        fs::write(t.path().join(name), data).unwrap();
        assert_eq!(
            r.execute("read_file", json!({"path":name}))["status"],
            "failed"
        );
    }
    let fifo = std::ffi::CString::new(t.path().join("fifo").to_str().unwrap()).unwrap();
    unsafe {
        assert_eq!(libc::mkfifo(fifo.as_ptr(), 0o600), 0);
    }
    assert_eq!(
        r.execute("read_file", json!({"path":"fifo"}))["status"],
        "failed"
    );
    assert!(
        r.execute("read_file", json!({"path":"missing"}))["error"]
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
    fs::write(t.path().join("main.py"), "repeat repeat").unwrap();
    assert_eq!(
        edit(&mut r, json!({"old_text":"repeat"}))["status"],
        "failed"
    );
    assert_eq!(*previews.borrow(), 0);
    let read = r.execute("read_file", json!({"path":"main.py"}));
    fs::write(t.path().join("main.py"), "user edit").unwrap();
    assert_eq!(r.execute("apply_patch",json!({"path":"main.py","sha256":read["sha256"],"old_text":"user edit","new_text":"lost"}))["status"],"failed");
}
#[test]
fn directory_limits() {
    let (t, mut r) = setup();
    for i in 0..501 {
        fs::write(t.path().join(i.to_string()), "").unwrap();
    }
    assert_eq!(
        r.execute("list_files", json!({"path":"."}))["status"],
        "failed"
    );
}
#[test]
fn exact_command_policy_and_denial() {
    let (_t, mut r) = setup();
    r.allowed_commands = vec![vec!["/usr/bin/true".into()]];
    r.approve = Box::new(|_, _| panic!("Commands outside policy must not reach approval"));
    assert_eq!(
        r.execute("run_command", json!({"argv":["/bin/sh","-c","true"]}))["status"],
        "not_run"
    );
    r.approve = Box::new(|_, _| false);
    let denied = r.execute("run_command", json!({"argv":["/usr/bin/true"]}));
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

#[test]
fn shared_approval_preserves_previews_and_stopped_result_shapes() {
    let mut tools = vec!["apply_patch", "create_file"];
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
            let read = r.execute("read_file", json!({"path":"file"}));
            r.allowed_commands = vec![vec!["/usr/bin/true".into()]];
            let (args, expected_kind, expected_preview) = match tool {
                "apply_patch" => (
                    json!({"path":"./file","sha256":read["sha256"],"old_text":"before","new_text":"after"}),
                    "patch",
                    "--- file\n+++ file\n@@ -1 +1 @@\n-before\n+after\n\nExact replacement: \"before\" -> \"after\"",
                ),
                "create_file" => (
                    json!({"path":"./new","content":"after\n"}),
                    "create_file",
                    "Create \"new\" (6 UTF-8 bytes)\n--- /dev/null\n+++ \"new\"\n+after\n",
                ),
                _ => (
                    json!({"argv":["/usr/bin/true"]}),
                    "command",
                    "argv = [\"/usr/bin/true\"]\nRead-only project; no network; scratch writes only.",
                ),
            };
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
            let result = r.execute(tool, args.clone());
            let mut expected =
                json!({"status":if interrupt {"canceled"} else {"denied"},"executed":false});
            if tool == "create_file" || (tool == "apply_patch" && !interrupt) {
                expected["path"] = json!(if tool == "create_file" { "new" } else { "file" });
            }
            assert_eq!(result, expected, "{tool}");
            assert_eq!(*count.borrow(), 1);
            assert!(r.modified.is_empty());
            assert_eq!(r.seen["file"], hash(b"before\n"));
            assert_eq!(fs::read_dir(t.path()).unwrap().count(), 1);
            assert_eq!(r.events.len(), 2);
            assert_eq!(
                r.events[1],
                json!({"tool":tool,"arguments":args,"result":expected})
            );
        }
    }
}
