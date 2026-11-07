"""Check repository Markdown structure, local links/anchors and template metadata."""

import re
import subprocess
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]


def anchors(text):
    result = set()
    counts = {}
    in_code = False
    for line in text.splitlines():
        if line.startswith("```"):
            in_code = not in_code
        if in_code:
            continue
        if re.match(r"^#{1,6} ", line):
            title = re.sub(r"^#+ ", "", line).strip().lower()
            slug = re.sub(r"[^\w\- ]", "", title).replace(" ", "-")
            count = counts.get(slug, 0)
            counts[slug] = count + 1
            result.add(slug if count == 0 else f"{slug}-{count}")
    return result


def main():
    files = (
        subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            cwd=ROOT,
        )
        .decode()
        .split("\0")
    )
    documents = sorted({ROOT / path for path in files if path.endswith(".md")})
    errors = []
    for path in documents:
        text = path.read_text()
        if sum(line.startswith("```") for line in text.splitlines()) % 2:
            errors.append(f"{path.relative_to(ROOT)}: unclosed code fence")
        for target in re.findall(r"(?<!!)\[[^\]]*\]\(([^)]+)\)", text):
            parsed = urlsplit(target)
            if parsed.scheme or target.startswith("//"):
                continue
            destination = (
                (path.parent / unquote(parsed.path)).resolve() if parsed.path else path
            )
            if not destination.exists():
                errors.append(f"{path.relative_to(ROOT)}: missing local link {target}")
            elif (
                parsed.fragment
                and destination.suffix == ".md"
                and unquote(parsed.fragment) not in anchors(destination.read_text())
            ):
                errors.append(f"{path.relative_to(ROOT)}: missing anchor {target}")
    template = (ROOT / ".github/ISSUE_TEMPLATE/issue.md").read_text()
    if not template.startswith("---\n") or "\n---\n" not in template[4:]:
        errors.append("Issue template needs YAML front matter")
    else:
        metadata = template.split("---", 2)[1]
        for key in ("name", "about", "title", "labels", "assignees"):
            if not re.search(rf"^{key}:", metadata, re.MULTILINE):
                errors.append(f"Issue template missing {key}")
    for error in errors:
        print(error)
    print(f"Checked {len(documents)} Markdown documents: {len(errors)} errors")
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())
