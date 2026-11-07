use super::*;
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
