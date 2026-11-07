"""Exact argv policy plus OS isolation. Never fall back to unsandboxed execution."""

import json
import os
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from .cancellation import defer_sigint


class SandboxUnavailable(ValueError):
    pass


RUNTIME_DIRS = (
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library/Apple",
    "/Library/Frameworks",
    "/opt/homebrew/Cellar",
    "/opt/homebrew/opt",
    "/opt/homebrew/lib",
    "/opt/homebrew/bin",
    "/lib",
    "/lib64",
)


def parse_command(value):
    argv = json.loads(value)
    if (
        not isinstance(argv, list)
        or not argv
        or len(argv) > 64
        or any(not isinstance(x, str) or not x or "\0" in x for x in argv)
    ):
        raise ValueError(
            "--allow-command must be a JSON array of nonempty argv strings"
        )
    executable = Path(argv[0])
    if (
        not executable.is_absolute()
        or not executable.is_file()
        or not os.access(executable, os.X_OK)
    ):
        raise ValueError(
            "Allowed commands require an existing absolute executable path"
        )
    executable = executable.resolve()
    if not any(
        executable.is_relative_to(Path(p).resolve())
        for p in RUNTIME_DIRS
        if Path(p).exists()
    ):
        raise ValueError(
            "Command executable must be installed in a documented system runtime directory"
        )
    # Preserve the exact argv the user selected, including argv[0]. No shell expansion.
    return tuple(argv)


def sandbox_argv(root, scratch, argv):
    if sys.platform == "darwin" and Path("/usr/bin/sandbox-exec").is_file():

        def quote(path):
            return json.dumps(str(path), ensure_ascii=True)

        readable = [root, scratch] + [
            Path(p).resolve() for p in RUNTIME_DIRS if Path(p).exists()
        ]
        reads = " ".join(f"(subpath {quote(p)})" for p in readable)
        profile = (
            "(version 1) (deny default) (allow process-exec) (allow sysctl-read) "
            "(allow file-read-metadata) "
            f'(allow file-read* {reads} (literal "/") (literal "/dev/null") (literal "/dev/urandom") '
            '(literal "/dev/random") (subpath "/private/var/db/dyld")) '
            f'(allow file-write* (subpath {quote(scratch)}) (literal "/dev/null")) '
            f"(deny file-read* (subpath {quote(root / '.git')}))"
        )
        return ["/usr/bin/sandbox-exec", "-p", profile, *argv]
    if sys.platform.startswith("linux") and shutil.which("bwrap"):
        command = [
            shutil.which("bwrap"),
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
        ]
        for path in ("/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc/ld.so.cache"):
            if Path(path).exists():
                command += ["--ro-bind", path, path]
        command += [
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--ro-bind",
            str(root),
            str(root),
            "--bind",
            str(scratch),
            str(scratch),
        ]
        if (root / ".git").exists():
            command += (
                ["--tmpfs", str(root / ".git")]
                if (root / ".git").is_dir()
                else ["--ro-bind", "/dev/null", str(root / ".git")]
            )
        return command + ["--chdir", str(root), "--", *argv]
    raise SandboxUnavailable(
        "Verification needs macOS sandbox-exec or Linux bubblewrap (bwrap); commands were not run"
    )


def _kill(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def run(root, argv, cancel, timeout=30, output_limit=16384):
    with (
        defer_sigint(cancel) as canceled,
        tempfile.TemporaryDirectory(prefix="cognix-check-") as temporary,
    ):
        scratch = Path(temporary).resolve()
        command = sandbox_argv(root, scratch, argv)
        env = {
            "PATH": "/usr/bin:/bin:/opt/homebrew/bin",
            "HOME": str(scratch),
            "TMPDIR": str(scratch),
            "TMP": str(scratch),
            "TEMP": str(scratch),
            "LANG": "en_US.UTF-8",
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONNOUSERSITE": "1",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_TERMINAL_PROMPT": "0",
        }
        if canceled():
            return {"status": "canceled", "executed": False}
        process = None
        output = bytearray()
        status = None
        try:
            process = subprocess.Popen(
                command,
                cwd=root,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                start_new_session=True,
                close_fds=True,
            )
            deadline = time.monotonic() + timeout
            with selectors.DefaultSelector() as selector:
                os.set_blocking(process.stdout.fileno(), False)
                selector.register(process.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    if canceled():
                        status = "canceled"
                        break
                    if time.monotonic() >= deadline:
                        status = "limit_reached"
                        break
                    for key, _ in selector.select(0.05):
                        chunk = os.read(
                            key.fd, min(4096, output_limit - len(output) + 1)
                        )
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        output.extend(chunk)
                        if len(output) > output_limit:
                            status = "limit_reached"
                            break
                    if status:
                        break
                # A process may close stdout and keep running; the same deadline still applies.
                while status is None and process.poll() is None:
                    if canceled():
                        status = "canceled"
                    elif time.monotonic() >= deadline:
                        status = "limit_reached"
                    else:
                        cancel.wait(0.02)
            if canceled():
                status = "canceled"
        except KeyboardInterrupt:
            cancel.set()
            status = "canceled"
        finally:
            # Also kills descendants retaining pipes or surviving after their parent exited.
            if process is not None:
                _kill(process)
                process.stdout.close()
        if process is None:
            return {"status": "canceled", "executed": False}
        if canceled():
            status = "canceled"
        return {
            "status": status or ("passed" if process.returncode == 0 else "failed"),
            "executed": True,
            "exit_code": process.returncode,
            "output": bytes(output[:output_limit]).decode("utf-8", errors="replace"),
            "limit": "time or output" if status == "limit_reached" else None,
        }
