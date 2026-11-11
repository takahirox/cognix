use super::*;
use crate::Cancellation;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

fn runtime(root: &Path) -> Runtime {
    Runtime::new(
        root,
        Box::new(|_, _| panic!("Preparation must never request approval")),
        Cancellation::default(),
    )
    .unwrap()
}
fn intent(changes: Value) -> PatchIntent {
    PatchIntent::parse(
        json!({"version":2,"changes":changes})
            .to_string()
            .as_bytes(),
    )
    .unwrap()
}
fn add(path: &str, content: &str) -> Value {
    json!({"op":"add","path":path,"content":content})
}
fn read(r: &mut Runtime, path: &str) -> String {
    let result = r.execute("read_file", json!({"path":path}));
    assert_eq!(result["status"], "ok", "{result}");
    result["sha256"].as_str().unwrap().into()
}
fn update(path: &str, digest: &str, edits: Value) -> Value {
    json!({"op":"update","path":path,"sha256":digest,"edits":edits})
}
fn edit(old: &str, new: &str) -> Value {
    json!({"old_text":old,"new_text":new})
}
fn write(root: &Path, path: &str, text: &str) {
    fs::write(root.join(path), text).unwrap();
    fs::set_permissions(root.join(path), fs::Permissions::from_mode(0o644)).unwrap();
}

// Snapshot every entry, including hidden entries, directories, symlink targets,
// inode, permissions, uid/gid, links, length, mtime/ctime and file bytes. atime is
// deliberately excluded: ordinary kernel reads may update it on some mounts.
fn snapshot(root: &Path) -> Value {
    fn visit(root: &Path, at: &Path, entries: &mut BTreeMap<String, Value>) {
        let m = at.symlink_metadata().unwrap();
        let name = at.strip_prefix(root).unwrap().to_str().unwrap().to_owned();
        let data = if m.is_file() {
            json!(fs::read(at).unwrap())
        } else if m.file_type().is_symlink() {
            json!(fs::read_link(at).unwrap().to_str().unwrap())
        } else {
            Value::Null
        };
        let mut entry = json!({"metadata":Fingerprint::new(&m),"data":data});
        #[cfg(target_os = "macos")]
        if m.is_file() || m.is_dir() {
            use std::os::macos::fs::MetadataExt as _;
            let fd = File::open(at).unwrap();
            let size = unsafe { libc::flistxattr(fd.as_raw_fd(), std::ptr::null_mut(), 0, 0) };
            assert!(size >= 0);
            let mut names = vec![0u8; size as usize];
            assert_eq!(
                unsafe {
                    libc::flistxattr(fd.as_raw_fd(), names.as_mut_ptr().cast(), names.len(), 0)
                },
                size
            );
            let mut values = BTreeMap::new();
            for name in names.split(|b| *b == 0).filter(|s| !s.is_empty()) {
                let c = CString::new(name).unwrap();
                let n = unsafe {
                    libc::fgetxattr(fd.as_raw_fd(), c.as_ptr(), std::ptr::null_mut(), 0, 0, 0)
                };
                assert!(n >= 0);
                let mut value = vec![0u8; n as usize];
                assert_eq!(
                    unsafe {
                        libc::fgetxattr(
                            fd.as_raw_fd(),
                            c.as_ptr(),
                            value.as_mut_ptr().cast(),
                            value.len(),
                            0,
                            0,
                        )
                    },
                    n
                );
                values.insert(format!("{name:?}"), value);
            }
            entry["xattr_values"] = json!(values);
            entry["flags"] = json!(m.st_flags());
            entry["xattr_names"] = json!(names);
            let mut attrs = libc::attrlist {
                bitmapcount: libc::ATTR_BIT_MAP_COUNT,
                reserved: 0,
                commonattr: libc::ATTR_CMN_EXTENDED_SECURITY,
                volattr: 0,
                dirattr: 0,
                fileattr: 0,
                forkattr: 0,
            };
            let mut acl = vec![0u8; 16384];
            assert_eq!(
                unsafe {
                    libc::fgetattrlist(
                        fd.as_raw_fd(),
                        (&mut attrs as *mut libc::attrlist).cast(),
                        acl.as_mut_ptr().cast(),
                        acl.len(),
                        0,
                    )
                },
                0
            );
            let bytes = u32::from_ne_bytes(acl[..4].try_into().unwrap()) as usize;
            assert!(bytes <= acl.len());
            acl.truncate(bytes);
            entry["acl"] = json!(acl);
        }
        entries.insert(name, entry);
        if m.is_dir() {
            for child in fs::read_dir(at).unwrap() {
                visit(root, &child.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    json!(entries)
}

#[test]
fn strict_parser_rejects_malformed_closed_contracts() {
    let digest = "a".repeat(64);
    let valid = [
        add("a", ""),
        update("a", &digest, json!([edit("x", "y")])),
        json!({"op":"delete","path":"a","sha256":digest}),
        json!({"op":"move","path":"a","to":"b","sha256":digest}),
    ];
    for change in valid {
        assert!(
            PatchIntent::parse(
                json!({"version":2,"changes":[change.clone()]})
                    .to_string()
                    .as_bytes()
            )
            .is_ok()
        );
        for key in change.as_object().unwrap().keys() {
            let mut missing = change.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert_eq!(
                PatchIntent::parse(
                    json!({"version":2,"changes":[missing]})
                        .to_string()
                        .as_bytes()
                )
                .unwrap_err()
                .reason,
                Reason::Shape,
                "missing {key}"
            );
            let mut wrong = change.clone();
            wrong[key] = Value::Null;
            assert_eq!(
                PatchIntent::parse(
                    json!({"version":2,"changes":[wrong]})
                        .to_string()
                        .as_bytes()
                )
                .unwrap_err()
                .reason,
                Reason::Shape,
                "wrong {key}"
            );
        }
        let mut extra = change;
        extra["surprise"] = json!(1);
        assert_eq!(
            PatchIntent::parse(
                json!({"version":2,"changes":[extra]})
                    .to_string()
                    .as_bytes()
            )
            .unwrap_err()
            .reason,
            Reason::Shape
        );
    }
    for edits in [json!([]), json!([edit("x", "y")]), Value::Null] {
        let raw = json!({"version":2,"changes":[{"op":"move","path":"a","to":"b","sha256":digest,"edits":edits}]});
        assert_eq!(
            PatchIntent::parse(raw.to_string().as_bytes())
                .unwrap_err()
                .reason,
            Reason::Shape
        );
    }
    for (value, reason) in [
        (json!({}), Reason::Shape),
        (
            json!({"version":2,"changes":[],"extra":true}),
            Reason::Shape,
        ),
        (
            json!({"version":1,"changes":[add("a", "")]}),
            Reason::Version,
        ),
        (json!({"version":2,"changes":[]}), Reason::Limit),
        (
            json!({"version":2,"changes":[{"op":"copy","path":"a"}]}),
            Reason::Shape,
        ),
        (
            json!({"version":2,"changes":[update("a", &digest, json!([{"old_text":"x","new_text":"y","extra":0}]))]}),
            Reason::Shape,
        ),
        (
            json!({"version":2,"changes":[update("a", &digest, json!([{"old_text":"x"}]))]}),
            Reason::Shape,
        ),
        (
            json!({"version":2,"changes":[update("a", &digest, json!([{"old_text":3,"new_text":"y"}]))]}),
            Reason::Shape,
        ),
    ] {
        assert_eq!(
            PatchIntent::parse(value.to_string().as_bytes())
                .unwrap_err()
                .reason,
            reason,
            "{value}"
        );
    }
    for raw in [
        b"not json".as_slice(),
        br#"{"version":2,"version":2,"changes":[]}"#,
        br#"{"version":2,"changes":[{"op":"add","path":"a","path":"b","content":""}]}"#,
        br#"{"version":2,"changes":[{"op":"add","path":"a","content":"\ud800"}]}"#,
    ] {
        assert!(PatchIntent::parse(raw).is_err());
    }
    assert_eq!(
        PatchIntent::parse(&[0xff]).unwrap_err().reason,
        Reason::Text
    );
    for h in [
        "".into(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        let raw = json!({"version":2,"changes":[update("a", &h, json!([edit("x", "y")]))]});
        assert_eq!(
            PatchIntent::parse(raw.to_string().as_bytes())
                .unwrap_err()
                .reason,
            Reason::Hash
        );
    }
}

#[test]
fn parser_limits_paths_text_and_edits() {
    let parse = |changes| {
        PatchIntent::parse(
            json!({"version":2,"changes":changes})
                .to_string()
                .as_bytes(),
        )
    };
    for path in [
        "", "/a", "a/", "a//b", ".", "a/./b", "../a", "a/../b", ".git/x", "a/.GiT/x", "a\0b",
    ] {
        assert_eq!(
            parse(json!([add(path, "")])).unwrap_err().reason,
            Reason::Path,
            "{path:?}"
        );
    }
    let exact_path = format!(
        "{}/{}",
        vec!["a".repeat(127); 31].join("/"),
        "b".repeat(128)
    );
    assert_eq!(exact_path.len(), 4096);
    assert!(parse(json!([add(&exact_path, "")])).is_ok());
    assert_eq!(
        parse(json!([add(&(exact_path + "x"), "")]))
            .unwrap_err()
            .reason,
        Reason::Limit
    );
    assert!(parse(json!([add(&vec!["a"; 32].join("/"), "")])).is_ok());
    assert_eq!(
        parse(json!([add(&vec!["a"; 33].join("/"), "")]))
            .unwrap_err()
            .reason,
        Reason::Limit
    );
    assert!(
        parse(json!(
            (0..8)
                .map(|i| add(&format!("f{i}"), ""))
                .collect::<Vec<_>>()
        ))
        .is_ok()
    );
    assert_eq!(
        parse(json!(
            (0..9)
                .map(|i| add(&format!("f{i}"), ""))
                .collect::<Vec<_>>()
        ))
        .unwrap_err()
        .reason,
        Reason::Limit
    );
    assert!(parse(json!([add("a", &"é".repeat(FILE_BYTES / 2))])).is_ok());
    assert_eq!(
        parse(json!([add("a", &"é".repeat(FILE_BYTES / 2 + 1))]))
            .unwrap_err()
            .reason,
        Reason::Limit
    );
    assert_eq!(
        parse(json!([add("a", "a\0b")])).unwrap_err().reason,
        Reason::Text
    );
    for edits in [
        json!([]),
        json!([edit("", "x")]),
        json!([edit("x", "x")]),
        json!([edit("x", "\0")]),
        json!([edit("\0", "x")]),
    ] {
        assert!(parse(json!([update("a", &"0".repeat(64), edits)])).is_err());
    }
    for n in [16, 17] {
        let edits: Vec<_> = (0..n).map(|i| edit(&format!("x{i}"), "y")).collect();
        assert_eq!(
            parse(json!([update("a", &"0".repeat(64), json!(edits))])).is_ok(),
            n == 16
        );
    }
    let mut raw = json!({"version":2,"changes":[add("a", "")]})
        .to_string()
        .into_bytes();
    raw.resize(ARG_BYTES, b' ');
    assert!(PatchIntent::parse(&raw).is_ok());
    raw.push(b' ');
    assert_eq!(PatchIntent::parse(&raw).unwrap_err().reason, Reason::Limit);
    for n in [4, 5] {
        let changes: Vec<_> = (0..n)
            .map(|i| add(&format!("a{i}"), &"x".repeat(FILE_BYTES)))
            .collect();
        assert_eq!(parse(json!(changes)).is_ok(), n == 4);
    }
}

#[test]
fn exact_original_bytes_handle_overlaps_ambiguity_and_noops() {
    let apply = |before: &str, es: Value| {
        let edits: Vec<Edit> = serde_json::from_value(es).unwrap();
        edited(before, &edits, "test")
    };
    assert_eq!(
        apply(
            "a\r\nb c",
            json!([edit("c", "a"), edit("a", "c"), edit("b", "β")])
        )
        .unwrap(),
        "c\r\nβ a"
    );
    assert_eq!(
        apply("abc", json!([edit("a", "b"), edit("b", "c")])).unwrap(),
        "bcc"
    );
    for (before, es) in [
        ("aaa", json!([edit("aa", "b")])),
        ("ééé", json!([edit("éé", "b")])),
        ("abc", json!([edit("ab", "x"), edit("bc", "y")])),
        ("abc", json!([edit("a", "x"), edit("a", "y")])),
        ("abc", json!([edit("z", "x")])),
        ("ab", json!([edit("a", "ab"), edit("b", "")])),
        ("a\r\n", json!([edit("a\n", "b\n")])),
        ("", json!([edit("x", "y")])),
    ] {
        assert_eq!(apply(before, es).unwrap_err().reason, Reason::Edit);
    }
    assert_eq!(
        apply("a", json!([edit("a", &"x".repeat(FILE_BYTES))]))
            .unwrap()
            .len(),
        FILE_BYTES
    );
    // Individually legal replacements can exceed the resulting-file limit.
    assert_eq!(
        apply("ab", json!([edit("a", &"x".repeat(FILE_BYTES))]))
            .unwrap_err()
            .reason,
        Reason::Limit
    );
}

#[test]
fn mixed_plan_and_preview_are_golden_deterministic_and_read_only() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "update", "mod old;\n");
    write(t.path(), "delete", "gone\n");
    write(t.path(), "old", "before\n");
    fs::create_dir(t.path().join(".hidden")).unwrap();
    write(t.path(), ".hidden/keep", "unchanged");
    let mut r = runtime(t.path());
    let u = read(&mut r, "update");
    let d = read(&mut r, "delete");
    let m = read(&mut r, "old");
    let request = intent(json!([
        add("new/shared/add", "new\n"), update("update", &u, json!([edit("old", "moved")])),
        {"op":"delete","path":"delete","sha256":d},
        {"op":"move","path":"old","to":"new/shared/moved","sha256":m}
    ]));
    let before = snapshot(t.path());
    let events = r.events.clone();
    let seen = r.seen.clone();
    let plan = r.prepare_v2(&request).unwrap();
    let second = r.prepare_v2(&request).unwrap();
    assert_eq!(plan.inspect(), second.inspect());
    assert_eq!(plan.digest(), second.digest());
    assert_eq!(plan.preview(), second.preview());
    assert_eq!(snapshot(t.path()), before);
    assert_eq!(r.events, events);
    assert_eq!(r.seen, seen);
    assert!(r.modified.is_empty());
    let mut projected = plan.inspect();
    assert_eq!(
        projected["root"]["identity"],
        json!(Identity::new(&t.path().metadata().unwrap()))
    );
    for op in projected["operations"].as_array_mut().unwrap() {
        if let Some(before) = op["before"].as_object_mut() {
            before.remove("state");
            let observed = before.remove("metadata").unwrap();
            assert_eq!(observed["xattrs"]["status"], "observed");
            assert_eq!(observed["acl"]["status"], "observed");
            assert_eq!(observed["flags"]["status"], "observed");
        }
    }
    projected.as_object_mut().unwrap().remove("root");
    assert_eq!(
        projected,
        json!({
        "version":2,"parents":{}, "new_directories":[{"path":"new","first_change":0,"mode":448},{"path":"new/shared","first_change":0,"mode":448}],
            "operations":[
                {"op":"add","path":"new/shared/add","to":null,"before":null,"after":"new\n","after_sha256":hash(b"new\n"),"before_bytes":0,"after_bytes":4,"destination_absent":true,"mode":384,"edits":[],"constraints":["establish destination metadata/inheritance support before writes; create 0600 subject to umask"]},
                {"op":"update","path":"update","to":null,"before":{"content":"mod old;\n","sha256":u},"after":"mod moved;\n","after_sha256":hash(b"mod moved;\n"),"before_bytes":9,"after_bytes":11,"destination_absent":false,"mode":420,"edits":[edit("old","moved")],"constraints":["establish metadata support before writes; safely preserve source mode, flags, ACLs and xattrs or refuse execution"]},
                {"op":"delete","path":"delete","to":null,"before":{"content":"gone\n","sha256":d},"after":null,"after_sha256":null,"before_bytes":5,"after_bytes":0,"destination_absent":false,"mode":null,"edits":[],"constraints":["establish metadata support before unlink; revalidate source mode, flags, ACLs and xattrs or refuse execution"]},
                {"op":"move","path":"old","to":"new/shared/moved","before":{"content":"before\n","sha256":m},"after":"before\n","after_sha256":m,"before_bytes":7,"after_bytes":7,"destination_absent":true,"mode":420,"edits":[],"constraints":["establish metadata support before destination writes; safely preserve source mode, flags, ACLs and xattrs or refuse execution; revalidate before source unlink"]}
            ],"total_result_bytes":22,"constraints":[
                "not authorization; explicit consent to exact digest required", "revalidate whole plan after consent and each operation before mutation",
                "recheck anchored root, parent/source identities, metadata and hashes, and every absence", "exclusive parent creation (0700) and destination publication; no clobber",
                "sequential commits; record plan-owned directories and each committed substep; no rollback", "metadata support required before any write; safely preserve source attributes or refuse execution; add mode 0600 subject to umask"]
        })
    );
    let mut preview = plan
        .preview()
        .replace(&plan.digest(), "DIGEST")
        .replace(&m, "MOVE_HASH");
    for op in &plan.operations {
        if let Some(source) = &op.before {
            preview = preview.replace(&source.metadata.summary(), "METADATA");
        }
    }
    assert_eq!(
        preview,
        concat!(
            "v2 patch plan DIGEST (not authorization)\n",
            "mkdir \"new\" (0700 subject to umask; change 0)\n",
            "mkdir \"new/shared\" (0700 subject to umask; change 0)\n",
            "0: add \"new/shared/add\" (0 -> 4 bytes)\n",
            "  constraint: establish destination metadata/inheritance support before writes; create 0600 subject to umask\n",
            "1: update \"update\" (9 -> 11 bytes)\n",
            "  metadata: METADATA\n",
            "  constraint: establish metadata support before writes; safely preserve source mode, flags, ACLs and xattrs or refuse execution\n",
            "2: delete \"delete\" (5 -> 0 bytes)\n",
            "  metadata: METADATA\n",
            "  constraint: establish metadata support before unlink; revalidate source mode, flags, ACLs and xattrs or refuse execution\n",
            "3: move \"old\" -> \"new/shared/moved\" (7 -> 7 bytes)\n",
            "  unchanged content: sha256 MOVE_HASH (7 bytes)\n",
            "  metadata: METADATA\n",
            "  constraint: establish metadata support before destination writes; safely preserve source mode, flags, ACLs and xattrs or refuse execution; revalidate before source unlink\n",
            "Content diffs (8192 escaped-character budget):\n",
            "--- /dev/null\n+++ new/shared/add\n@@ -0,0 +1 @@\n+new\n",
            "--- update\n+++ update\n@@ -1 +1 @@\n-mod old;\n+mod moved;\n",
            "--- delete\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n",
        )
    );
    let mut copy = plan.inspect();
    copy["operations"][0]["after"] = json!("tampered");
    assert_eq!(plan.digest(), second.digest());
}

#[test]
fn preview_has_all_paths_and_directories_when_content_is_truncated() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "old", "unchanged\n");
    let mut r = runtime(t.path());
    let h = read(&mut r, "old");
    let before = snapshot(t.path());
    let weird = "esc\x1b[31m\n\u{202e}file";
    let p = r
        .prepare_v2(&intent(json!([
            add("nested/big", &"x\n".repeat(FILE_BYTES / 2)),
            add(weird, "\x1b\r\t\u{202e}"),
            {"op":"move","path":"old","to":"nested/moved","sha256":h}
        ])))
        .unwrap();
    let preview = p.preview();
    assert_eq!(snapshot(t.path()), before);
    assert!(preview.contains("truncated"));
    assert!(preview.contains("move \"old\" -> \"nested/moved\""));
    assert!(preview.contains(&format!("unchanged content: sha256 {h} (10 bytes)")));
    assert!(preview.contains("metadata:"));
    assert!(
        preview.contains("safely preserve source mode, flags, ACLs and xattrs or refuse execution")
    );
    assert!(preview.contains(&quoted(weird)));
    assert!(preview.contains("mkdir \"nested\""));
    assert!(!preview.contains('\x1b'));
    assert!(!preview.contains('\r'));
    assert!(!preview.contains('\u{202e}'));
    assert!(preview.chars().count() < PREVIEW_CHARS + 2500);
    let complete = p.complete_escaped();
    assert!(!complete.contains('\x1b'));
    assert!(!complete.contains('\n'));
    assert!(!complete.contains('\u{202e}'));
    assert_eq!(p.inspect()["operations"][1]["after"], "\x1b\r\t\u{202e}");
}

#[test]
fn successful_native_add_plan_keeps_full_tree_and_metadata_unchanged() {
    let t = tempfile::tempdir().unwrap();
    fs::create_dir(t.path().join("existing")).unwrap();
    fs::create_dir(t.path().join(".hidden")).unwrap();
    write(t.path(), ".hidden/keep", "unchanged");
    let r = runtime(t.path());
    let request = intent(json!([
        add("existing/shared/a", "a\n"),
        add("existing/shared/b", ""),
        add(".new/empty", "")
    ]));
    let before = snapshot(t.path());
    let plan = r.prepare_v2(&request).unwrap();
    let repeat = r.prepare_v2(&request).unwrap();
    assert_eq!(plan.inspect(), repeat.inspect());
    assert_eq!(plan.preview(), repeat.preview());
    assert_eq!(
        plan.inspect()["new_directories"],
        json!([
            {"path":"existing/shared","first_change":0,"mode":448},
            {"path":".new","first_change":2,"mode":448}
        ])
    );
    assert_eq!(
        plan.inspect()["parents"]["existing"],
        json!(Fingerprint::new(
            &t.path().join("existing").metadata().unwrap()
        ))
    );
    assert_eq!(snapshot(t.path()), before);
    assert!(r.events.is_empty());
    assert!(r.modified.is_empty());
}

#[test]
fn source_substitution_and_special_parent_modes_are_refused_without_effects() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "source", "old");
    let mut r = runtime(t.path());
    let h = read(&mut r, "source");
    fs::rename(t.path().join("source"), t.path().join("held-source")).unwrap();
    symlink("held-source", t.path().join("source")).unwrap();
    let before = snapshot(t.path());
    assert!(
        r.prepare_v2(&intent(json!([update(
            "source",
            &h,
            json!([edit("old", "new")])
        )])))
        .is_err()
    );
    assert_eq!(snapshot(t.path()), before);
    fs::create_dir(t.path().join("special")).unwrap();
    fs::set_permissions(t.path().join("special"), fs::Permissions::from_mode(0o2700)).unwrap();
    let before = snapshot(t.path());
    assert_eq!(
        r.prepare_v2(&intent(json!([add("special/new", "")])))
            .unwrap_err()
            .reason,
        Reason::UnsupportedMetadata
    );
    assert_eq!(snapshot(t.path()), before);
}

#[test]
fn conflicts_dependencies_cycles_prefixes_and_aliases_are_rejected() {
    let h = "a".repeat(64);
    for changes in [
        json!([add("a", ""), add("a", "")]),
        json!([add("a", ""), add("A", "")]),
        json!([add("a", ""), add("a/b", "")]),
        json!([add("a/b", ""), add("A", "")]),
        json!([add("é", ""), add("e\u{301}", "")]),
        json!([{"op":"move","path":"a","to":"b","sha256":h},{"op":"move","path":"b","to":"a","sha256":h}]),
        json!([{"op":"move","path":"a","to":"A","sha256":h}]),
        json!([{"op":"move","path":"a","to":"b","sha256":h}, update("a", &h, json!([edit("x", "y")]))]),
        json!([{"op":"move","path":"a","to":"b","sha256":h}, update("b", &h, json!([edit("x", "y")]))]),
        json!([add("a", ""), update("a", &h, json!([edit("x", "y")]))]),
        json!([{"op":"delete","path":"a","sha256":h},add("a", "")]),
    ] {
        assert_eq!(
            PatchIntent::parse(
                json!({"version":2,"changes":changes})
                    .to_string()
                    .as_bytes()
            )
            .unwrap_err()
            .reason,
            Reason::Conflict
        );
    }
    let t = tempfile::tempdir().unwrap();
    let r = runtime(t.path());
    // Missing shared parents cannot use ambiguous spellings either.
    for changes in [
        json!([add("New/a", ""), add("new/b", "")]),
        json!([add("é/a", ""), add("e\u{301}/b", "")]),
    ] {
        let before = snapshot(t.path());
        assert_eq!(
            r.prepare_v2(&intent(changes)).unwrap_err().reason,
            Reason::Conflict
        );
        assert_eq!(snapshot(t.path()), before);
    }
    // Establish real volume behavior rather than assuming string equality.
    write(t.path(), "Case", "x");
    write(t.path(), "é", "x");
    let case_aliases = fs::metadata(t.path().join("case")).is_ok();
    let unicode_aliases = fs::metadata(t.path().join("e\u{301}")).is_ok();
    eprintln!(
        "Actual test volume aliases: case={case_aliases}, canonical_unicode={unicode_aliases}"
    );
    for dest in ["case", "e\u{301}"] {
        assert_eq!(
            r.prepare_v2(&intent(json!([add(dest, "")])))
                .unwrap_err()
                .reason,
            Reason::Conflict
        );
    }
    if case_aliases {
        assert_eq!(
            Identity::new(&t.path().join("case").metadata().unwrap()),
            Identity::new(&t.path().join("Case").metadata().unwrap())
        );
    }
    if unicode_aliases {
        assert_eq!(
            Identity::new(&t.path().join("é").metadata().unwrap()),
            Identity::new(&t.path().join("e\u{301}").metadata().unwrap())
        );
    }
}

#[test]
fn real_filesystem_refusals_have_no_effects() {
    let t = tempfile::tempdir().unwrap();
    let mut r = runtime(t.path());
    write(t.path(), "source", "x");
    fs::create_dir(t.path().join("dir")).unwrap();
    symlink("absent", t.path().join("dangling")).unwrap();
    symlink("dir", t.path().join("linked")).unwrap();
    fs::hard_link(t.path().join("source"), t.path().join("hard")).unwrap();
    let c = CString::new(t.path().join("fifo").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let digest = hash(b"x");
    for path in ["source", "hard", "fifo", "dir", "dangling"] {
        assert_ne!(r.execute("read_file", json!({"path":path}))["status"], "ok");
    }
    for path in [
        "source",
        "dir",
        "dangling",
        "linked/file",
        "fifo",
        "source/file",
    ] {
        let before = snapshot(t.path());
        assert!(
            r.prepare_v2(&intent(json!([add(path, "x")]))).is_err(),
            "{path}"
        );
        assert_eq!(snapshot(t.path()), before, "{path}");
    }
    assert_eq!(
        r.prepare_v2(&intent(json!([update(
            "source",
            &digest,
            json!([edit("x", "y")])
        )])))
        .unwrap_err()
        .reason,
        Reason::ReadRequired
    );
    write(t.path(), "ordinary", "x");
    let h = read(&mut r, "ordinary");
    fs::hard_link(t.path().join("ordinary"), t.path().join("new-hard")).unwrap();
    let before = snapshot(t.path());
    assert!(
        r.prepare_v2(&intent(json!([update(
            "ordinary",
            &h,
            json!([edit("x", "y")])
        )])))
        .is_err()
    );
    assert_eq!(snapshot(t.path()), before);
}

#[test]
fn stale_hash_identity_read_evidence_and_root_are_rejected() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("root");
    fs::create_dir(&root).unwrap();
    write(&root, "a", "old");
    let mut r = runtime(&root);
    let digest = read(&mut r, "a");
    let request = intent(json!([update("a", &digest, json!([edit("old", "new")]))]));
    write(&root, "a", "changed");
    let before = snapshot(&root);
    assert_eq!(
        r.prepare_v2(&request).unwrap_err().reason,
        Reason::StaleState
    );
    assert_eq!(snapshot(&root), before);
    write(&root, "a", "old");
    let digest = read(&mut r, "a");
    fs::rename(root.join("a"), root.join("held-inode")).unwrap();
    write(&root, "a", "old");
    assert_eq!(
        r.prepare_v2(&request).unwrap_err().reason,
        Reason::StaleState
    );
    read(&mut r, "a");
    r.seen.remove("a");
    assert_eq!(
        r.prepare_v2(&request).unwrap_err().reason,
        Reason::ReadRequired
    );
    // A public legacy hash map entry must not forge v2 identity evidence.
    r.seen.insert("never-read".into(), digest);
    write(&root, "never-read", "old");
    assert_eq!(
        r.prepare_v2(&intent(json!([update(
            "never-read",
            &hash(b"old"),
            json!([edit("old", "new")])
        )])))
        .unwrap_err()
        .reason,
        Reason::ReadRequired
    );
    fs::rename(&root, t.path().join("moved-root")).unwrap();
    fs::create_dir(&root).unwrap();
    assert_eq!(
        r.prepare_v2(&intent(json!([add("new", "")])))
            .unwrap_err()
            .reason,
        Reason::StaleState
    );
}

#[test]
fn changed_state_between_inspections_never_becomes_a_plan() {
    for race in [
        "source-bytes",
        "source-inode",
        "parent",
        "root",
        "new-parent",
        "destination",
        "metadata",
    ] {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        write(&root, "dir/source", "old");
        let mut r = runtime(&root);
        let h = read(&mut r, "dir/source");
        let mut changes = vec![add("new/child", "new"), add("dir/add", "")];
        if race.starts_with("source-") || race == "metadata" {
            changes.push(update("dir/source", &h, json!([edit("old", "new")])));
            let before = snapshot(&root);
            r.prepare_v2(&intent(json!(changes.clone()))).unwrap();
            assert_eq!(snapshot(&root), before);
        }
        let request = intent(json!(changes));
        let mut called = false;
        let mut after_race = None;
        let result = r.prepare_v2_checked(&request, || {
            called = true;
            match race {
                "source-bytes" => write(&root, "dir/source", "modified"),
                "source-inode" => {
                    fs::rename(root.join("dir/source"), root.join("dir/held")).unwrap();
                    write(&root, "dir/source", "old");
                }
                "parent" => {
                    fs::rename(root.join("dir"), root.join("old-dir")).unwrap();
                    fs::create_dir(root.join("dir")).unwrap();
                    write(&root, "dir/source", "old");
                }
                "root" => {
                    fs::rename(&root, t.path().join("held-root")).unwrap();
                    fs::create_dir(&root).unwrap();
                }
                "new-parent" => fs::create_dir(root.join("new")).unwrap(),
                "destination" => write(&root, "dir/add", "user"),
                "metadata" => {
                    fs::set_permissions(root.join("dir/source"), fs::Permissions::from_mode(0o600))
                        .unwrap()
                }
                _ => unreachable!(),
            }
            after_race = Some(snapshot(&root));
        });
        assert!(called, "race hook must be reached: {race}");
        assert!(result.is_err(), "{race}");
        assert_eq!(snapshot(&root), after_race.unwrap(), "{race}");
        if race.starts_with("source-") || race == "metadata" {
            assert_eq!(result.unwrap_err().reason, Reason::StaleState, "{race}");
        }
        eprintln!("PASS native between-inspection race: {race}");
        assert!(r.modified.is_empty());
        assert!(!root.join("new/child").exists());
    }
}

#[test]
fn all_planning_limits_and_shared_parent_order_are_enforced() {
    let t = tempfile::tempdir().unwrap();
    let r = runtime(t.path());
    let long = vec!["d"; 31].join("/") + "/f";
    let plan = r
        .prepare_v2(&intent(json!([add(&long, ""), add("z/f", "")])))
        .unwrap();
    assert_eq!(
        plan.inspect()["new_directories"].as_array().unwrap().len(),
        32
    );
    assert_eq!(plan.inspect()["new_directories"][31]["first_change"], 1);
    assert_eq!(
        r.prepare_v2(&intent(json!([
            add(&long, ""),
            add("z/f", ""),
            add("w/f", "")
        ])))
        .unwrap_err()
        .reason,
        Reason::Limit
    );
    assert!(
        r.prepare_v2(&intent(json!([add(
            &format!("missing/{}", "x".repeat(256)),
            ""
        )])))
        .is_err()
    );
    assert!(
        r.prepare_v2(&intent(json!([add(
            &format!("{}/child", "x".repeat(256)),
            ""
        )])))
        .is_err()
    );
    let mut r = runtime(t.path());
    let mut changes = Vec::new();
    for i in 0..5 {
        let path = format!("f{i}");
        write(t.path(), &path, &"x".repeat(FILE_BYTES));
        let h = read(&mut r, &path);
        changes.push(json!({"op":"move","path":path,"to":format!("dest{i}"),"sha256":h}));
    }
    let before = snapshot(t.path());
    let plan = r.prepare_v2(&intent(json!(changes[..4]))).unwrap();
    assert_eq!(plan.total_result_bytes, TOTAL_BYTES);
    assert!(
        plan.operations
            .iter()
            .all(|op| op.after_sha256.as_ref() == op.before.as_ref().map(|b| &b.sha256))
    );
    assert_eq!(
        r.prepare_v2(&intent(json!(changes))).unwrap_err().reason,
        Reason::Limit
    );
    eprintln!("PASS native unchanged-move aggregate boundary: 256 KiB accepted; 320 KiB refused");
    assert_eq!(snapshot(t.path()), before);
}

#[test]
fn source_special_mode_is_observed_and_invalid_text_fails_closed() {
    let t = tempfile::tempdir().unwrap();
    let mut r = runtime(t.path());
    write(t.path(), "mode", "x");
    fs::set_permissions(t.path().join("mode"), fs::Permissions::from_mode(0o4644)).unwrap();
    let h = read(&mut r, "mode");
    let before = snapshot(t.path());
    let plan = r
        .prepare_v2(&intent(json!([update(
            "mode",
            &h,
            json!([edit("x", "y")])
        )])))
        .unwrap();
    assert_eq!(
        plan.inspect()["operations"][0]["before"]["metadata"]["special_mode_bits"],
        0o4000
    );
    assert!(plan.preview().contains("safely preserve source mode"));
    assert_eq!(snapshot(t.path()), before);
    for (path, bytes) in [
        ("binary", vec![0xff]),
        ("nul", vec![0]),
        ("oversize", vec![b'x'; FILE_BYTES + 1]),
    ] {
        fs::write(t.path().join(path), &bytes).unwrap();
        let result = r.execute("read_file", json!({"path":path}));
        assert_eq!(result["status"], "failed");
        assert!(
            r.prepare_v2(&intent(
                json!([{"op":"delete","path":path,"sha256":hash(&bytes)}])
            ))
            .is_err()
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_multibyte_source_destination_and_parent_names_are_feasible() {
    for name in [
        "日".repeat(100),
        "é".repeat(200),
        "日".repeat(255),
        "é".repeat(255),
        "😀".repeat(127),
    ] {
        let t = tempfile::tempdir().unwrap();
        // Actual creation establishes this volume's behavior before preparation.
        fs::create_dir(t.path().join(&name)).unwrap();
        fs::create_dir(t.path().join("existing")).unwrap();
        let source = format!("existing/{name}");
        write(t.path(), &source, "source\n");
        let mut r = runtime(t.path());
        let digest = read(&mut r, &source);
        let events_before = r.events.len();
        let parent = File::open(t.path().join("existing")).unwrap();
        let reported = unsafe { libc::fpathconf(parent.as_raw_fd(), libc::_PC_NAME_MAX) };
        assert!(reported > 0 && name.len() > reported as usize);
        let before = snapshot(t.path());
        // Establish descriptor lookup feasibility as well as real source planning.
        name_feasible(&parent, &name, &source).unwrap();
        assert_eq!(
            component_identity(&parent, &name, &source).unwrap(),
            Some(Identity::new(&t.path().join(&source).metadata().unwrap()))
        );
        let paths = [
            format!("dest/{name}/{name}"), // missing multibyte parent and leaf
            format!("{name}/added"),       // existing multibyte parent
            format!("{name}/new/{name}"),  // leaf below another missing parent
        ];
        let mut changes = paths.iter().map(|p| add(p, "")).collect::<Vec<_>>();
        changes.push(json!({"op":"move","path":source,"to":"existing/moved","sha256":digest}));
        let plan = r.prepare_v2(&intent(json!(changes))).unwrap();
        assert_eq!(
            plan.inspect()["operations"]
                .as_array()
                .unwrap()
                .iter()
                .take(3)
                .map(|op| op["path"].as_str().unwrap())
                .collect::<Vec<_>>(),
            paths.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(
            plan.inspect()["new_directories"],
            json!([
                {"path":"dest","first_change":0,"mode":448},
                {"path":format!("dest/{name}"),"first_change":0,"mode":448},
                {"path":format!("{name}/new"),"first_change":2,"mode":448},
            ])
        );
        assert_eq!(snapshot(t.path()), before);
        assert_eq!(r.events.len(), events_before);
        assert!(r.modified.is_empty());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_overlong_components_are_refused_even_below_planned_parents() {
    let t = tempfile::tempdir().unwrap();
    let r = runtime(t.path());
    let before = snapshot(t.path());
    for name in [
        "a".repeat(256),
        "日".repeat(256),
        "é".repeat(256),
        "😀".repeat(128),
    ] {
        // Native creation must agree with the read-only feasibility result.
        let failure = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(t.path().join(&name))
            .unwrap_err();
        assert_eq!(failure.raw_os_error(), Some(libc::ENAMETOOLONG));
        for path in [
            name.clone(),
            format!("{name}/child"),
            format!("missing/{name}"),
            format!("missing/{name}/child"),
        ] {
            let failure = r.prepare_v2(&intent(json!([add(&path, "")]))).unwrap_err();
            assert_eq!(failure.reason, Reason::Filesystem);
            assert_eq!(failure.location, path);
            assert_eq!(snapshot(t.path()), before);
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn actual_macos_xattrs_acls_flags_and_parent_feasibility() {
    use std::process::Command;
    let t = tempfile::tempdir().unwrap();
    let mut r = runtime(t.path());
    for path in ["xattr", "acl", "flags"] {
        write(t.path(), path, "x");
    }
    let file = File::open(t.path().join("xattr")).unwrap();
    assert_eq!(
        unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                c"user.cognix-test".as_ptr(),
                b"v".as_ptr().cast(),
                1,
                0,
                0,
            )
        },
        0
    );
    let status = Command::new("/bin/chmod")
        .arg("+a")
        .arg("everyone allow read")
        .arg(t.path().join("acl"))
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        unsafe {
            libc::fchflags(
                File::open(t.path().join("flags")).unwrap().as_raw_fd(),
                libc::UF_NODUMP,
            )
        },
        0
    );
    for path in ["xattr", "acl", "flags"] {
        let h = read(&mut r, path);
        let before = snapshot(t.path());
        let plan = r
            .prepare_v2(&intent(json!([{"op":"delete","path":path,"sha256":h}])))
            .unwrap();
        let observed = &plan.operations[0].before.as_ref().unwrap().metadata;
        match path {
            "xattr" => assert!(
                matches!(&observed.xattrs, Observation::Observed(v) if v.iter().any(|x| x.name == b"user.cognix-test" && x.value == BlobState::new(b"v")))
            ),
            "acl" => assert!(matches!(&observed.acl, Observation::Observed(v) if v.bytes > 0)),
            "flags" => assert_eq!(observed.flags, Observation::Observed(libc::UF_NODUMP)),
            _ => unreachable!(),
        }
        assert!(
            plan.preview()
                .contains("establish metadata support before unlink")
        );
        assert_eq!(snapshot(t.path()), before);
    }
    fs::create_dir(t.path().join("restricted")).unwrap();
    fs::set_permissions(
        t.path().join("restricted"),
        fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    assert!(
        r.prepare_v2(&intent(json!([add("restricted/a", "")])))
            .is_err()
    );
    fs::set_permissions(
        t.path().join("restricted"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let status = Command::new("/bin/chmod")
        .arg("+a")
        .arg("everyone allow read,write,add_file,add_subdirectory,directory_inherit,file_inherit")
        .arg(t.path().join("restricted"))
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        r.prepare_v2(&intent(json!([add("restricted/a", "")])))
            .unwrap_err()
            .reason,
        Reason::UnsupportedMetadata
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_xattr_value_and_acl_changes_invalidate_observed_sources() {
    use std::process::Command;
    for race in ["xattr", "acl"] {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "source", "old");
        let file = File::open(t.path().join("source")).unwrap();
        let set_xattr = |value: &[u8]| {
            assert_eq!(
                unsafe {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        c"user.cognix-test".as_ptr(),
                        value.as_ptr().cast(),
                        value.len(),
                        0,
                        0,
                    )
                },
                0
            );
        };
        let set_acl = |rule: &str| {
            assert!(
                Command::new("/bin/chmod")
                    .arg("+a")
                    .arg(rule)
                    .arg(t.path().join("source"))
                    .status()
                    .unwrap()
                    .success()
            );
        };
        if race == "xattr" {
            set_xattr(b"a");
        } else {
            set_acl("everyone allow read");
        }
        let mut r = runtime(t.path());
        let h = read(&mut r, "source");
        let request = intent(json!([update("source", &h, json!([edit("old", "new")]))]));
        let before = snapshot(t.path());
        let plan = r.prepare_v2(&request).unwrap();
        assert_eq!(snapshot(t.path()), before);
        let prior = &plan.operations[0].before.as_ref().unwrap().metadata;
        let mut after_race = None;
        let mut called = false;
        let result = r.prepare_v2_checked(&request, || {
            called = true;
            if race == "xattr" {
                set_xattr(b"b");
            } else {
                set_acl("everyone allow write");
            }
            after_race = Some(snapshot(t.path()));
        });
        assert!(called);
        assert_eq!(result.unwrap_err().reason, Reason::StaleState);
        assert_eq!(snapshot(t.path()), after_race.unwrap());
        let fresh = inspect_metadata(&file, &file.metadata().unwrap());
        if race == "xattr" {
            assert_ne!(fresh.xattrs, prior.xattrs);
        } else {
            assert_ne!(fresh.acl, prior.acl);
        }
        // A later preparation must not silently refresh prior read evidence.
        let before = snapshot(t.path());
        assert_eq!(
            r.prepare_v2(&request).unwrap_err().reason,
            Reason::StaleState
        );
        assert_eq!(snapshot(t.path()), before);
        read(&mut r, "source");
        let refreshed = r.prepare_v2(&request).unwrap();
        assert_ne!(refreshed.digest(), plan.digest());
        assert_eq!(snapshot(t.path()), before);
        eprintln!("PASS native metadata observation/change: {race}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn unavailable_metadata_is_distinct_from_presence_and_blocks_execution() {
    assert!(matches!(inspect_xattrs(-1), Observation::Unavailable(_)));
    assert!(matches!(inspect_acl(-1), Observation::Unavailable(_)));
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "source", "old");
    let mut r = runtime(t.path());
    let digest = read(&mut r, "source");
    let request = intent(json!([update(
        "source",
        &digest,
        json!([edit("old", "new")])
    )]));
    let mut plan = r.prepare_v2(&request).unwrap();
    let mut source = plan.operations.pop().unwrap().before.unwrap();
    // Exercise the pure operation/preview builder's explicit failure handoff.
    source.metadata.xattrs = inspect_xattrs(-1);
    let operation = plan_operation(&request.changes[0], Some(source)).unwrap();
    assert!(operation.constraints.contains(
        &"metadata inspection incomplete; execution blocked until safety is established"
    ));
    plan.operations.push(operation);
    assert!(plan.preview().contains("xattrs inspection unavailable"));
    assert!(plan.complete_escaped().contains("unavailable"));
}
