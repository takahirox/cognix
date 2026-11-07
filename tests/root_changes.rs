use cognix::{Cancellation, runtime::Runtime};
use serde_json::json;
use std::fs;
#[cfg(target_os = "macos")]
#[test]
fn command_rejects_replaced_project_root() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("project");
    fs::create_dir(&path).unwrap();
    let moved = t.path().join("original");
    let root = path.clone();
    let mut r = Runtime::new(
        &path,
        Box::new(move |_, _| {
            fs::rename(&root, &moved).unwrap();
            fs::create_dir(&root).unwrap();
            true
        }),
        Cancellation::default(),
    )
    .unwrap();
    r.allowed_commands = vec![vec!["/usr/bin/true".into()]];
    let result = r.execute("run_command", json!({"argv":["/usr/bin/true"]}));
    assert_eq!(result["status"], "not_run");
    assert_eq!(result["executed"], false);
    assert!(result["error"].as_str().unwrap().contains("root changed"));
}
