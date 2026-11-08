//! Terminal adapter helpers. Untrusted text never emits terminal controls.
use crate::Cancellation;
use serde_json::Value;
use std::io::Write;

pub fn text(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            // escape_debug uses Unicode printability, including invisible/bidi
            // format characters. Keep ordinary quotes/backslashes unchanged.
            if matches!(c, '\n' | '\t' | '\\' | '\'' | '"')
                || c.escape_debug().to_string() == c.to_string()
            {
                c.to_string()
            } else {
                format!("\\x{:02x}", c as u32)
            }
        })
        .collect()
}
pub fn show(value: &str) {
    println!("{}", text(value));
}
pub fn input(prompt: &str, cancel: &Cancellation) -> Result<Option<String>, String> {
    read_input(prompt, cancel, true)
}
/// Read a newline-submitted line, discarding any buffered text at EOF.
pub fn submitted_input(prompt: &str, cancel: &Cancellation) -> Result<Option<String>, String> {
    read_input(prompt, cancel, false)
}
fn read_input(
    prompt: &str,
    cancel: &Cancellation,
    accept_eof_text: bool,
) -> Result<Option<String>, String> {
    print!("{prompt}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = Vec::new();
    loop {
        if cancel.is_canceled() {
            return Err("Canceled".into());
        }
        let mut poll = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // Reading one byte avoids buffering past the next approval or prompt.
        if unsafe { libc::poll(&mut poll, 1, 20) } <= 0 {
            continue;
        }
        let mut byte = 0u8;
        let n = unsafe { libc::read(0, (&mut byte as *mut u8).cast(), 1) };
        if n == 0 {
            return if line.is_empty() || !accept_eof_text {
                Ok(None)
            } else {
                String::from_utf8(line).map(Some).map_err(|e| e.to_string())
            };
        }
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.to_string());
        }
        if byte == b'\n' {
            return String::from_utf8(line).map(Some).map_err(|e| e.to_string());
        }
        line.push(byte);
        if line.len() > 65536 {
            return Err("Input exceeds 64 KiB".into());
        }
    }
}
pub fn render(report: &Value) -> String {
    let mut out = format!(
        "\nSession: {}\n",
        report["status"].as_str().unwrap_or("error")
    );
    if let Some(e) = report["error"].as_str() {
        out.push_str(&format!("{e}\n"));
    }
    if let Some(reply) = report["model_reply"].as_str().filter(|s| !s.is_empty()) {
        out.push_str(&format!(
            "\nModel response (not verification evidence):\n{reply}\n"
        ));
    }
    out.push_str(&format!(
        "\nRecorded execution evidence:\nModified files: {}\n",
        report["modified_files"]
    ));
    let checks = report["checks"].as_array().unwrap();
    if checks.is_empty() {
        out.push_str("Verification: not run (no command executed)\n");
    }
    for event in checks {
        let r = &event["result"];
        out.push_str(&format!(
            "Check argv: {}\n  {}; executed={}; exit={}\n",
            event["arguments"]["argv"], r["status"], r["executed"], r["exit_code"]
        ));
        for key in ["output", "error"] {
            if let Some(s) = r[key].as_str() {
                out.push_str(s);
                out.push('\n');
            }
        }
    }
    for e in report["events"].as_array().unwrap() {
        if e["tool"] != "run_command" && e["result"]["status"] != "ok" {
            out.push_str(&format!("{}: {}\n", e["tool"], e["result"]));
        }
    }
    out.push_str("Recorded 'passed' means exit code 0.\n");
    text(&out)
}
