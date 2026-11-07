//! No Python tooling is needed to validate the repository documentation.
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    process::Command,
};
fn anchors(text: &str) -> HashSet<String> {
    let heading = Regex::new(r"^#{1,6} (.*)$").unwrap();
    let punctuation = Regex::new(r"[^\w\- ]").unwrap();
    let mut result = HashSet::new();
    let mut counts = HashMap::<String, usize>::new();
    let mut code = false;
    for line in text.lines() {
        if line.starts_with("```") {
            code = !code;
        }
        if code {
            continue;
        }
        if let Some(c) = heading.captures(line) {
            let slug = punctuation
                .replace_all(&c[1].trim().to_lowercase(), "")
                .replace(' ', "-");
            let count = counts.entry(slug.clone()).or_default();
            result.insert(if *count == 0 {
                slug
            } else {
                format!("{slug}-{count}")
            });
            *count += 1;
        }
    }
    result
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err("git ls-files failed".into());
    }
    let files = String::from_utf8(output.stdout)?;
    let documents: HashSet<_> = files
        .split('\0')
        .filter(|p| p.ends_with(".md"))
        .map(|p| root.join(p))
        .filter(|p| p.exists())
        .collect();
    let links = Regex::new(r"!?\[[^\]]*\]\(([^)]+)\)")?;
    let scheme = Regex::new(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")?;
    let mut errors = Vec::new();
    for path in &documents {
        let text = fs::read_to_string(path)?;
        if text.lines().filter(|l| l.starts_with("```")).count() % 2 != 0 {
            errors.push(format!("{}: unclosed code fence", path.display()));
        }
        for capture in links.captures_iter(&text) {
            let target = &capture[1];
            if target.starts_with("//") || scheme.is_match(target) {
                continue;
            }
            let (file, fragment) = target.split_once('#').unwrap_or((target, ""));
            let file = file.split('?').next().unwrap();
            let decoded = percent_encoding::percent_decode_str(file).decode_utf8()?;
            let destination = if file.is_empty() {
                path.clone()
            } else {
                path.parent().unwrap().join(decoded.as_ref())
            };
            if !destination.exists() {
                errors.push(format!("{}: missing local link {target}", path.display()));
            } else if !fragment.is_empty()
                && destination.extension().is_some_and(|s| s == "md")
                && !anchors(&fs::read_to_string(destination)?).contains(
                    percent_encoding::percent_decode_str(fragment)
                        .decode_utf8()?
                        .as_ref(),
                )
            {
                errors.push(format!("{}: missing anchor {target}", path.display()));
            }
        }
    }
    let template = fs::read_to_string(root.join(".github/ISSUE_TEMPLATE/issue.md"))?;
    if !template.starts_with("---\n") || !template[4..].contains("\n---\n") {
        errors.push("Issue template needs YAML front matter".into());
    } else {
        let metadata = template.split("---").nth(1).unwrap();
        for key in ["name", "about", "title", "labels", "assignees"] {
            if !metadata.lines().any(|l| l.starts_with(&format!("{key}:"))) {
                errors.push(format!("Issue template missing {key}"));
            }
        }
    }
    for e in &errors {
        eprintln!("{e}");
    }
    println!(
        "Checked {} Markdown documents: {} errors",
        documents.len(),
        errors.len()
    );
    if !errors.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
