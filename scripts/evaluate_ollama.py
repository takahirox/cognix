"""Reproducible local acceptance trials on disposable, repository-owned fixtures.

Run from repository root: python3 scripts/evaluate_ollama.py --help
This fixture-only harness approves main.py diffs and one exact sandboxed check.
It is not a noninteractive approval mode for the product CLI.
"""

import argparse
import datetime
import json
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from cognix.agent import Agent
from cognix.commands import parse_command
from cognix.provider import Ollama, ProviderError
from cognix.runtime import Runtime


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True)
    parser.add_argument(
        "--python",
        required=True,
        help="absolute system-installed Python for sandboxed unittest",
    )
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    check = list(
        parse_command(
            json.dumps(
                [
                    args.python,
                    "-I",
                    "-B",
                    "-m",
                    "unittest",
                    "discover",
                    "-s",
                    ".",
                    "-p",
                    "test_*.py",
                ]
            )
        )
    )
    project = Path(__file__).resolve().parents[1]
    evidence = {
        "date_utc": datetime.datetime.now(datetime.UTC).isoformat(),
        "model": args.model,
        "platform": platform.platform(),
        "python": sys.version,
        "fixture": "tests/fixtures/division",
        "command": check,
        "attempts": [],
    }
    if sys.platform == "darwin":
        for key in ["machdep.cpu.brand_string", "hw.memsize"]:
            evidence[key] = subprocess.check_output(
                ["/usr/sbin/sysctl", "-n", key], text=True
            ).strip()
    for attempt in range(args.runs):
        with tempfile.TemporaryDirectory(prefix="cognix-trial-") as temporary:
            root = Path(temporary).resolve()
            shutil.copytree(
                project / "tests/fixtures/division",
                root,
                dirs_exist_ok=True,
                ignore=shutil.ignore_patterns("__pycache__"),
            )
            approvals = []

            def approve(kind, preview, approvals=approvals):
                decision = kind == "command" or (
                    kind == "patch" and preview.startswith("--- main.py\n+++ main.py\n")
                )
                approvals.append(
                    {"kind": kind, "preview": preview, "approved": decision}
                )
                return decision

            cancel = threading.Event()
            runtime = Runtime(root, approve, cancel, [tuple(check)])
            started = time.monotonic()
            try:
                provider = Ollama(args.model, num_ctx=32768)
                provider.validate(cancel)
                evidence["ollama_version"] = provider.version
                agent = Agent(
                    provider,
                    runtime,
                    cancel,
                    max_steps=16,
                    context_budget=32768,
                    activity=lambda s, attempt=attempt: print(
                        f"Trial {attempt + 1}: {s}", flush=True
                    ),
                )
                report = agent.run(
                    "Fix the division-by-zero bug in main.py: divide(a, 0) must return None. Inspect relevant files, run the allowed unittest check, correct any failures, and verify the fix. Preserve the tests."
                )
                success = (
                    report["status"] == "final_answer"
                    and report["modified_files"] == ["main.py"]
                    and any(e["tool"] == "read_file" for e in report["events"])
                    and report["checks"]
                    and report["checks"][-1]["result"]["status"] == "passed"
                    and (root / "test_main.py").read_bytes()
                    == (project / "tests/fixtures/division/test_main.py").read_bytes()
                )
                record = {
                    "success": bool(success),
                    "report": report,
                    "messages": agent.messages,
                }
            except (ProviderError, ValueError, OSError) as exc:
                record = {"success": False, "error": str(exc)}
            finally:
                runtime.close()
            record.update(
                {
                    "attempt": attempt + 1,
                    "seconds": round(time.monotonic() - started, 3),
                    "approvals": approvals,
                }
            )
            evidence["attempts"].append(record)
            print(f"Trial {attempt + 1}: success={record['success']}", flush=True)
    evidence["successes"] = sum(a["success"] for a in evidence["attempts"])
    evidence["failures"] = len(evidence["attempts"]) - evidence["successes"]
    args.output.write_text(json.dumps(evidence, indent=2) + "\n")
    print(
        f"Saved {args.output}: {evidence['successes']}/{args.runs} successful",
        flush=True,
    )
    return 0 if evidence["failures"] == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
