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

#[cfg(target_os = "macos")]
#[test]
fn command_checks_entire_tree_after_approval() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret");
    fs::write(&secret, "outside").unwrap();
    fs::create_dir(project.path().join("nested")).unwrap();
    let alias = project.path().join("nested/alias");
    let mut runtime = Runtime::new(
        project.path(),
        Box::new(move |_, _| {
            fs::hard_link(&secret, &alias).unwrap();
            true
        }),
        Cancellation::default(),
    )
    .unwrap();
    runtime.allowed_commands = vec![vec!["/usr/bin/true".into()]];
    // The alias is introduced during approval and never appears in argv.
    let result = runtime.execute("run_command", json!({"argv":["/usr/bin/true"]}));
    assert_eq!(result["status"], "not_run", "{result}");
    assert_eq!(result["executed"], false, "{result}");
    assert!(
        result["error"].as_str().unwrap().contains("hard link"),
        "{result}"
    );
    assert_eq!(runtime.events.last().unwrap()["result"], result);
}
