use super::*;

#[test]
fn creation_interruptions_and_publish_races_clean_up_owned_staging() {
    for phase in [
        "before_commit",
        "after_commit",
        "destination_race",
        "parent_race",
        "root_race",
        "staging_race",
    ] {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        let cancel = Cancellation::default();
        let mut r = Runtime::new(&root, Box::new(|_, _| true), cancel.clone()).unwrap();
        let path = root.clone();
        r.hook = Some(Box::new(move |point| {
            if point == phase {
                cancel.cancel();
            }
            if point != "before_commit" {
                return;
            }
            match phase {
                "destination_race" => std::fs::write(path.join("dir/new"), "racing edit").unwrap(),
                "parent_race" => {
                    std::fs::rename(path.join("dir"), path.join("old")).unwrap();
                    std::fs::create_dir(path.join("dir")).unwrap();
                }
                "root_race" => {
                    std::fs::rename(&path, path.with_file_name("moved")).unwrap();
                    std::fs::create_dir(&path).unwrap();
                }
                "staging_race" => {
                    let staging = std::fs::read_dir(path.join("dir"))
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path();
                    // Retain the owned inode so replacement cannot reuse its identity.
                    std::fs::rename(&staging, path.join("owned-moved")).unwrap();
                    std::fs::write(&staging, "unrelated replacement").unwrap();
                }
                _ => (),
            }
        }));
        let result = r.execute("create_file", json!({"path":"dir/new","content":"agent"}));
        let committed = phase == "after_commit";
        assert_eq!(
            result["status"],
            if matches!(phase, "before_commit" | "after_commit") {
                "canceled"
            } else {
                "failed"
            },
            "{phase}: {result}"
        );
        assert_eq!(result["executed"], committed);
        assert_eq!(r.modified.contains("dir/new"), committed);
        assert_eq!(r.events.last().unwrap()["result"], result);
        if committed {
            assert_eq!(result["sha256"], hash(b"agent"));
            assert!(!r.seen.contains_key("dir/new"));
            assert_eq!(
                std::fs::read_to_string(root.join("dir/new")).unwrap(),
                "agent"
            );
        } else if phase == "destination_race" {
            assert_eq!(
                std::fs::read_to_string(root.join("dir/new")).unwrap(),
                "racing edit"
            );
        } else {
            assert!(!root.join("dir/new").exists());
        }
        let parent = match phase {
            "parent_race" => root.join("old"),
            "root_race" => root.with_file_name("moved").join("dir"),
            _ => root.join("dir"),
        };
        let staging: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".cognix-")
            })
            .collect();
        if phase == "staging_race" {
            // Cleanup must leave a substituted name alone.
            assert_eq!(staging.len(), 1);
            assert_eq!(
                std::fs::read_to_string(&staging[0]).unwrap(),
                "unrelated replacement"
            );
        } else {
            assert!(staging.is_empty(), "{phase}: {staging:?}");
        }
    }
}
#[cfg(target_os = "macos")]
#[test]
fn interrupt_at_command_ownership_and_cleanup_boundaries() {
    for phase in ["spawned", "cleanup", "reaped"] {
        let t = tempfile::tempdir().unwrap();
        let cancel = Cancellation::default();
        let mut r = Runtime::new(t.path(), Box::new(|_, _| true), cancel.clone()).unwrap();
        let argv = vec![
            "/opt/homebrew/bin/python3".to_string(),
            "-c".into(),
            "print('completed')".into(),
        ];
        r.allowed_commands = vec![argv.clone()];
        crate::commands::INTERRUPT_AT.with(|point| *point.borrow_mut() = Some((phase, cancel)));
        let result = r.execute("run_command", json!({"argv":argv}));
        crate::commands::INTERRUPT_AT.with(|point| *point.borrow_mut() = None);
        assert_eq!(result["status"], "canceled");
        assert_eq!(result["executed"], true);
        assert_eq!(r.events.last().unwrap()["result"], result);
        if phase == "reaped" {
            assert_eq!(result["exit_code"], 0);
            assert_eq!(result["output"], "completed\n");
        }
        assert_eq!(
            unsafe { libc::kill(result["pid"].as_i64().unwrap() as i32, 0) },
            -1
        );
    }
}
#[test]
fn interruption_before_and_after_commit_preserves_evidence() {
    for phase in ["before_commit", "after_commit"] {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("file"), "before").unwrap();
        let cancel = Cancellation::default();
        let mut r = Runtime::new(t.path(), Box::new(|_, _| true), cancel.clone()).unwrap();
        let read = r.execute("read_file", json!({"path":"file"}));
        r.hook = Some(Box::new(move |p| {
            if p == phase {
                cancel.cancel();
            }
        }));
        let result = r.execute(
            "apply_patch",
            json!({"path":"file","sha256":read["sha256"],"old_text":"before","new_text":"after"}),
        );
        assert_eq!(result["status"], "canceled");
        let committed = phase == "after_commit";
        assert_eq!(result["executed"], committed);
        assert_eq!(
            std::fs::read_to_string(t.path().join("file")).unwrap(),
            if committed { "after" } else { "before" }
        );
        assert_eq!(r.modified.contains("file"), committed);
        assert_eq!(r.events.last().unwrap()["result"], result);
        if committed {
            assert_eq!(result["sha256"], hash(b"after"));
            assert_eq!(r.seen["file"], hash(b"after"));
        }
        assert!(std::fs::read_dir(t.path()).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".cognix-")
        }));
    }
}
