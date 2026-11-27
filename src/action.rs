//! Shared approval plans and result conventions, not filesystem/sandbox policy.
use crate::Cancellation;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalMode {
    #[default]
    Manual,
    Auto,
    FullAccess,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
}

pub type Approval = Box<dyn FnMut(&str, &str) -> bool>;

/// Pure diff construction shared by patch approval and internal v2 preparation.
pub(crate) fn patch_diff(from: &str, to: &str, before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .header(from, to)
        .to_string()
}

/// An immutable preview built only after the backend's side-effect-free checks.
/// The terminal callback still sanitizes the complete display and reads explicit
/// default-No consent. Consuming a plan never authorizes a later action.
pub(crate) struct ActionPlan {
    pub preview: String,
    pub kind: &'static str,
    pub path: Option<String>,
    pub evidence: Value,
}

impl ActionPlan {
    pub fn patch(path: &str, before: &str, after: &str, old: &str, new: &str) -> Self {
        let header = path.escape_debug().to_string();
        let diff = patch_diff(&header, &header, before, after);
        Self {
            kind: "patch",
            preview: format!("{diff}\nExact replacement: {old:?} -> {new:?}"),
            evidence: json!({"path":path,"diff":diff,"before_sha256":crate::runtime::hash(before.as_bytes())}),
            path: Some(path.into()),
        }
    }

    pub fn creation(path: &str, content: &str) -> Self {
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
            kind: "patch",
            preview,
            evidence: json!({"path":path,"pre_edit_state":"absent","addition":content}),
            path: Some(path.into()),
        }
    }

    pub fn stopped(&self, status: &str) -> Value {
        let mut result = unexecuted(status);
        if let Some(path) = &self.path {
            result["path"] = json!(path);
        }
        result
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
