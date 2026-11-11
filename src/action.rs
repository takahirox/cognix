//! Shared approval plans and result conventions, not filesystem/sandbox policy.
use crate::Cancellation;
use serde_json::{Value, json};

pub type Approval = Box<dyn FnMut(&str, &str) -> bool>;

/// Pure diff construction shared by legacy approval and internal v2 preparation.
pub(crate) fn patch_diff(from: &str, to: &str, before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .header(from, to)
        .to_string()
}

/// An immutable preview built only after the backend's side-effect-free checks.
/// The terminal callback still sanitizes the complete display and reads explicit
/// default-No consent. Consuming a plan never authorizes a later action.
pub(crate) struct ActionPlan<'a> {
    kind: &'static str,
    preview: String,
    denied_path: Option<&'a str>,
    canceled_path: Option<&'a str>,
}

impl<'a> ActionPlan<'a> {
    pub fn patch(path: &'a str, before: &str, after: &str, old: &str, new: &str) -> Self {
        let diff = patch_diff(path, path, before, after);
        Self {
            kind: "patch",
            preview: format!("{diff}\nExact replacement: {old:?} -> {new:?}"),
            denied_path: Some(path),
            canceled_path: None,
        }
    }

    pub fn creation(path: &'a str, content: &str) -> Self {
        // Bound escaped characters, since terminal controls expand on display.
        // The full content remains in the runtime's event ledger.
        const PREVIEW_CHARS: usize = 8192;
        let display = crate::terminal::text(content);
        let excerpt: String = display.chars().take(PREVIEW_CHARS).collect();
        let mut preview = format!(
            "Create {path:?} ({} UTF-8 bytes)\n--- /dev/null\n+++ {path:?}\n",
            content.len()
        );
        for line in excerpt.split_inclusive('\n') {
            preview.push('+');
            preview.push_str(line);
        }
        if !excerpt.ends_with('\n') && !excerpt.is_empty() {
            preview.push_str("\n\\ No newline at end of displayed content\n");
        }
        if display.chars().count() > PREVIEW_CHARS {
            preview.push_str("[Creation preview truncated; full content is larger]\n");
        } else if content.is_empty() {
            preview.push_str("[Empty file: 0 additions]\n");
        }
        Self {
            kind: "create_file",
            preview,
            denied_path: Some(path),
            canceled_path: Some(path),
        }
    }

    pub fn command(argv: &[String]) -> Self {
        Self {
            kind: "command",
            preview: format!(
                "argv = {}\nRead-only project; no network; scratch writes only.",
                json!(argv)
            ),
            denied_path: None,
            canceled_path: None,
        }
    }

    /// Cancellation takes precedence over either callback answer. On approval,
    /// the caller must still revalidate and execute in its dedicated backend.
    pub fn approve(self, approve: &mut Approval, cancel: &Cancellation) -> Result<(), Value> {
        let approved = approve(self.kind, &self.preview);
        let (status, path) = if cancel.is_canceled() {
            ("canceled", self.canceled_path)
        } else if !approved {
            ("denied", self.denied_path)
        } else {
            return Ok(());
        };
        let mut result = unexecuted(status);
        if let Some(path) = path {
            result["path"] = json!(path);
        }
        Err(result)
    }
}

pub(crate) fn unexecuted(status: &str) -> Value {
    json!({"status":status,"executed":false})
}

/// Normalize only reporting fields; never discard evidence of committed work.
pub(crate) fn finish_result(
    name: &str,
    result: Result<Value, String>,
    cancel: &Cancellation,
) -> Value {
    let mut result = result.unwrap_or_else(|error| {
        let mut result = unexecuted(if name == "run_command" {
            "not_run"
        } else {
            "failed"
        });
        result["error"] = json!(error.chars().take(1000).collect::<String>());
        result
    });
    if result.get("status").is_none() {
        result["status"] = json!("ok");
    }
    if cancel.is_canceled() && matches!(result["status"].as_str(), Some("ok" | "passed")) {
        result["status"] = json!("canceled");
    }
    result
}
