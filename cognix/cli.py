"""Terminal adapter. All execution evidence comes from the runtime ledger."""

import argparse
import json
import os
import signal
import threading
from pathlib import Path

from .agent import Agent
from .commands import parse_command
from .provider import Cancelled, Ollama, ProviderError
from .runtime import Runtime


def terminal_text(value):
    """Prevent untrusted filenames/output from injecting terminal escape sequences."""
    return "".join(
        c if c in "\n\t" or c.isprintable() else f"\\x{ord(c):02x}" for c in str(value)
    )


def show(value):
    print(terminal_text(value), flush=True)


def approve(kind, preview):
    show(f"\nProposed {kind}:\n{preview}")
    try:
        return input("Approve this action? [y/N] ").strip().lower() == "y"
    except EOFError:
        return False


def render(report):
    show(f"\nSession: {report['status']}")
    if report["error"]:
        show(report["error"])
    show("Modified files: " + (", ".join(report["modified_files"]) or "none"))
    if not report["checks"]:
        show("Verification: not run (no command executed)")
    for event in report["checks"]:
        result = event["result"]
        arguments = event["arguments"]
        show(
            "Check argv: "
            + json.dumps(
                arguments.get("argv") if isinstance(arguments, dict) else arguments,
                ensure_ascii=False,
            )
        )
        show(
            f"  {result['status']}; executed={result.get('executed', False)}; exit={result.get('exit_code', 'n/a')}"
        )
        if result.get("output"):
            show(result["output"])
        if result.get("error"):
            show(result["error"])
    for event in report["events"]:
        if event["tool"] != "run_command" and event["result"]["status"] not in {
            "ok",
            "passed",
        }:
            show(
                f"{event['tool']}: {event['result']['status']}: {event['result'].get('error', '')}"
            )
    # Free-form model prose is retained in memory, but cannot override or invent
    # evidence in terminal results (including false 'tests passed' claims).
    show("Results above are recorded execution evidence. 'passed' means exit code 0.")


def positive(value):
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return number


def parser():
    result = argparse.ArgumentParser(
        description="Cognix: bounded local edit-and-verify agent. Ctrl-C cancels; /exit quits."
    )
    result.add_argument(
        "--root",
        type=Path,
        default=Path.cwd(),
        help="project directory (default: current directory)",
    )
    result.add_argument(
        "--model", default=os.environ.get("COGNIX_MODEL", "qwen3-coder:30b")
    )
    result.add_argument(
        "--ollama-url",
        default=os.environ.get("COGNIX_OLLAMA_URL", "http://127.0.0.1:11434"),
    )
    result.add_argument(
        "--task",
        help="run one task instead of the interactive prompt; approvals still required",
    )
    result.add_argument(
        "--allow-command",
        action="append",
        default=[],
        metavar="JSON_ARGV",
        help="exact verification argv, repeatable; still sandboxed and prompted",
    )
    result.add_argument("--max-steps", type=positive, default=16)
    result.add_argument(
        "--num-ctx",
        type=positive,
        default=16384,
        help="Ollama context and conservative session budget",
    )
    result.add_argument(
        "--model-timeout",
        type=positive,
        default=120,
        help="seconds per provider request",
    )
    result.add_argument("--command-timeout", type=positive, default=30)
    result.add_argument(
        "--output-limit", type=positive, default=16384, help="bytes per command"
    )
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    cancel = threading.Event()
    runtime = None
    agent = None
    original_handler = signal.getsignal(signal.SIGINT)

    def interrupt(signum, frame):
        cancel.set()
        raise KeyboardInterrupt()

    signal.signal(signal.SIGINT, interrupt)
    try:
        if args.task is not None and not args.task.strip():
            raise ValueError("--task must contain a request")
        if args.num_ctx < 8192:
            raise ValueError(
                "--num-ctx must be at least 8192 for tools and output reserve"
            )
        allowed = [parse_command(value) for value in args.allow_command]
        runtime = Runtime(
            args.root, approve, cancel, allowed, args.command_timeout, args.output_limit
        )
        provider = Ollama(args.model, args.ollama_url, args.model_timeout, args.num_ctx)
        show(
            f"Project: {runtime.root}\nChecking local model {args.model} and tool calling..."
        )
        provider.validate(cancel)
        show(
            f"Ollama {provider.version}; model {args.model}. Ctrl-C cancels. /exit quits."
        )
        if not allowed:
            show(
                "No verification commands allowed. Restart with --allow-command JSON_ARGV to enable checks."
            )
        agent = Agent(provider, runtime, cancel, args.max_steps, args.num_ctx, show)
        while not cancel.is_set():
            if args.task is not None:
                task = args.task
            else:
                try:
                    task = input("\n> ").strip()
                except EOFError:
                    return 0
                if task == "/exit":
                    return 0
                if not task:
                    continue
            report = agent.run(task)
            render(report)
            if report["status"] != "final_answer":
                return 130 if report["status"] == "canceled" else 1
            if args.task is not None:
                failures = any(
                    e["result"]["status"] not in {"ok", "passed"}
                    for e in runtime.events
                )
                return 1 if failures else 0
        return 130
    except (KeyboardInterrupt, Cancelled):
        cancel.set()
        if agent:
            render(
                agent.report("canceled", "Interrupted; subsequent actions were not run")
            )
        else:
            show("Canceled; no project actions executed.")
        return 130
    except (ValueError, OSError, ProviderError) as exc:
        show(f"Error: {exc}")
        return 1
    finally:
        if runtime:
            runtime.close()
        signal.signal(signal.SIGINT, original_handler)
