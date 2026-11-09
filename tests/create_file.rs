use cognix::{
    Cancellation,
    runtime::{Runtime, hash},
    schema,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
};

fn setup() -> (tempfile::TempDir, Runtime) {
    let t = tempfile::tempdir().unwrap();
    let r = Runtime::new(t.path(), Box::new(|_, _| true), Cancellation::default()).unwrap();
    (t, r)
}
fn create(r: &mut Runtime, path: &str, content: &str) -> Value {
    r.execute("create_file", json!({"path":path,"content":content}))
}
fn no_staging_files(path: &std::path::Path) {
    assert!(fs::read_dir(path).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".cognix-")
    }));
}

#[test]
fn creation_preview_content_and_evidence() {
    let (t, mut r) = setup();
    fs::create_dir(t.path().join("src")).unwrap();
    fs::write(t.path().join("user-edit"), "preserve").unwrap();
    let target = t.path().join("src/module.rs");
    r.approve = Box::new(move |kind, preview| {
        assert_eq!(kind, "create_file");
        assert!(!target.exists());
        assert!(preview.contains("src/module.rs"));
        assert!(preview.contains("--- /dev/null"));
        assert!(preview.contains("+// 日本語 🦀\n"));
        assert!(preview.contains("18 UTF-8 bytes"));
        true
    });
    let content = "// 日本語 🦀\n";
    let result = create(&mut r, "./src/module.rs", content);
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["executed"], true);
    assert_eq!(result["path"], "src/module.rs");
    assert_eq!(result["bytes"], content.len());
    assert_eq!(result["sha256"], hash(content.as_bytes()));
    assert_eq!(
        fs::read(t.path().join("src/module.rs")).unwrap(),
        content.as_bytes()
    );
    let info = fs::metadata(t.path().join("src/module.rs")).unwrap();
    assert!(info.is_file());
    assert_eq!(info.nlink(), 1);
    assert_eq!(info.mode() & 0o777, 0o600);
    assert!(r.modified.contains("src/module.rs"));
    assert_eq!(r.events.last().unwrap()["tool"], "create_file");
    assert_eq!(r.events.last().unwrap()["arguments"]["content"], content);
    assert_eq!(r.events.last().unwrap()["result"], result);
    assert_eq!(
        fs::read_to_string(t.path().join("user-edit")).unwrap(),
        "preserve"
    );
    no_staging_files(&t.path().join("src"));
    // Creation must not relax apply_patch's read-first contract.
    let patch =
        json!({"path":"src/module.rs","sha256":result["sha256"],"old_text":"//", "new_text":"///"});
    assert_eq!(r.execute("apply_patch", patch)["status"], "failed");
    let read = r.execute("read_file", json!({"path":"src/module.rs"}));
    assert_eq!(read["content"], content);
}

#[test]
fn empty_and_maximum_content_and_bounded_escaped_preview() {
    let (t, mut r) = setup();
    r.approve = Box::new(|_, preview| {
        assert!(preview.contains("0 UTF-8 bytes"));
        assert!(preview.contains("Empty file"));
        true
    });
    assert_eq!(create(&mut r, "empty", "")["status"], "ok");
    assert_eq!(fs::read(t.path().join("empty")).unwrap(), b"");
    r.approve = Box::new(|_, preview| {
        assert!(preview.len() < 10000, "{}", preview.len());
        assert!(preview.contains("preview truncated"));
        assert!(!preview.contains('\u{1b}'));
        assert!(preview.contains("+\\x1b"));
        true
    });
    // The byte limit is inclusive and measured before terminal escaping.
    let content = "\u{1b}".repeat(schema::MAX_CREATE_BYTES);
    assert_eq!(create(&mut r, "maximum", &content)["status"], "ok");
    assert_eq!(
        fs::read_to_string(t.path().join("maximum")).unwrap(),
        content
    );
    no_staging_files(t.path());
}

#[test]
fn schema_and_content_reject_before_approval() {
    let (t, mut r) = setup();
    let tools = schema::tools();
    let def = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "create_file")
        .unwrap();
    assert_eq!(def["function"]["parameters"]["additionalProperties"], false);
    assert_eq!(
        def["function"]["parameters"]["required"],
        json!(["content", "path"])
    );
    r.approve = Box::new(|_, _| panic!("Invalid call must not ask for approval"));
    for args in [
        json!({"path":"new"}),
        json!({"content":""}),
        json!({"path":"new","content":"", "extra":true}),
        json!({"path":1,"content":""}),
        json!({"path":"new","content":null}),
        json!({"path":"new","content":"é".repeat(32769)}),
        json!({"path":"new","content":"x\0y"}),
        json!({"path":"x".repeat(4097),"content":""}),
        json!({"path":"new","content":"x".repeat(1024 * 1024)}),
    ] {
        let result = r.execute("create_file", args);
        assert_eq!(result["status"], "failed");
        assert_eq!(result["executed"], false);
        assert!(result["error"].is_string());
        assert_eq!(r.events.last().unwrap()["result"], result);
    }
    assert_eq!(fs::read_dir(t.path()).unwrap().count(), 0);
    assert!(r.modified.is_empty());
}

#[test]
fn denial_and_cancellation_leave_no_file() {
    for mode in ["denied", "cancel_before", "cancel_approval"] {
        let (t, mut r) = setup();
        let cancel = r.cancel.clone();
        r.approve = Box::new(move |_, _| {
            assert_ne!(mode, "cancel_before");
            if mode == "cancel_approval" {
                cancel.cancel();
            }
            mode != "denied"
        });
        if mode == "cancel_before" {
            r.cancel.cancel();
        }
        let result = create(&mut r, "new", "content");
        assert_eq!(
            result["status"],
            if mode == "denied" {
                "denied"
            } else {
                "canceled"
            }
        );
        assert_eq!(result["executed"], false);
        assert_eq!(fs::read_dir(t.path()).unwrap().count(), 0);
        assert!(r.modified.is_empty());
        assert_eq!(r.events.last().unwrap()["result"], result);
    }
}

#[test]
fn unsafe_paths_and_missing_or_invalid_parents() {
    let (t, mut r) = setup();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(t.path().join("nested")).unwrap();
    fs::write(t.path().join("plain"), "original").unwrap();
    symlink(outside.path(), t.path().join("symlink")).unwrap();
    r.approve = Box::new(|_, _| panic!("Unsafe destination must not reach approval"));
    for path in [
        "",
        ".",
        "./",
        "nested/",
        "nested/.",
        "/absolute",
        "../escape",
        "nested/../../escape",
        ".git/config",
        "nested/.GiT/config",
        "bad\0name",
        "symlink/new",
        "absent/new",
        "plain/new",
    ] {
        let result = create(&mut r, path, "new");
        assert_eq!(result["status"], "failed", "{path}: {result}");
        assert_eq!(result["executed"], false);
    }
    assert_eq!(
        fs::read_to_string(t.path().join("plain")).unwrap(),
        "original"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(t.path().join("nested")).unwrap().count(), 0);
    assert!(!t.path().join("absent").exists());
    assert!(r.modified.is_empty());
}

#[test]
fn all_existing_destination_types_and_duplicates_are_preserved() {
    let (t, mut r) = setup();
    let outside = tempfile::tempdir().unwrap();
    let source = outside.path().join("source");
    fs::write(&source, "outside").unwrap();
    fs::write(t.path().join("file"), "user edit").unwrap();
    fs::create_dir(t.path().join("dir")).unwrap();
    fs::hard_link(&source, t.path().join("hard")).unwrap();
    symlink(&source, t.path().join("link")).unwrap();
    symlink(outside.path().join("missing"), t.path().join("dangling")).unwrap();
    let fifo = std::ffi::CString::new(t.path().join("fifo").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert_eq!(create(&mut r, "created", "first")["status"], "ok");
    r.approve = Box::new(|_, _| panic!("Existing destination must not reach approval"));
    for path in ["file", "dir", "hard", "link", "dangling", "fifo", "created"] {
        let before = fs::symlink_metadata(t.path().join(path)).unwrap();
        assert_eq!(
            create(&mut r, path, "overwrite")["status"],
            "failed",
            "{path}"
        );
        let after = fs::symlink_metadata(t.path().join(path)).unwrap();
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.mode(), after.mode());
    }
    assert_eq!(
        fs::read_to_string(t.path().join("file")).unwrap(),
        "user edit"
    );
    assert_eq!(
        fs::read_to_string(t.path().join("created")).unwrap(),
        "first"
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "outside");
    no_staging_files(t.path());
}

#[test]
fn destination_and_parent_races_during_approval() {
    for race in ["file", "symlink", "parent", "parent_symlink", "root"] {
        let container = tempfile::tempdir().unwrap();
        let root = container.path().join("project");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        let path = root.clone();
        let moved = container.path().join("original");
        let mut r = Runtime::new(
            &root,
            Box::new(move |_, _| {
                match race {
                    "file" => fs::write(path.join("dir/new"), "racing edit").unwrap(),
                    "symlink" => symlink("missing", path.join("dir/new")).unwrap(),
                    "parent" | "parent_symlink" => {
                        fs::rename(path.join("dir"), path.join("old")).unwrap();
                        if race == "parent" {
                            fs::create_dir(path.join("dir")).unwrap();
                        } else {
                            symlink("old", path.join("dir")).unwrap();
                        }
                    }
                    "root" => {
                        fs::rename(&path, &moved).unwrap();
                        fs::create_dir(&path).unwrap();
                    }
                    _ => unreachable!(),
                }
                true
            }),
            Cancellation::default(),
        )
        .unwrap();
        let result = create(&mut r, "dir/new", "agent");
        assert_eq!(result["status"], "failed", "{race}: {result}");
        assert_eq!(result["executed"], false);
        assert!(r.modified.is_empty());
        match race {
            "file" => assert_eq!(
                fs::read_to_string(root.join("dir/new")).unwrap(),
                "racing edit"
            ),
            "symlink" => assert!(
                root.join("dir/new")
                    .symlink_metadata()
                    .unwrap()
                    .is_symlink()
            ),
            "parent" | "parent_symlink" => {
                assert_eq!(fs::read_dir(root.join("old")).unwrap().count(), 0)
            }
            "root" => assert_eq!(
                fs::read_dir(container.path().join("original/dir"))
                    .unwrap()
                    .count(),
                0
            ),
            _ => unreachable!(),
        }
    }
}
