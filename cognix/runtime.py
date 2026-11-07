"""Root-confined file tools and approval-controlled verification."""

import difflib
import hashlib
import os
import secrets
import stat
from contextlib import ExitStack
from pathlib import Path

from . import commands
from .cancellation import defer_sigint
from .provider import Cancelled
from .schema import validate


class Runtime:
    def __init__(
        self, root, approve, cancel, allowed_commands=(), timeout=30, output_limit=16384
    ):
        self.root = Path(root).resolve(strict=True)
        self.root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.approve = approve
        self.cancel = cancel
        self.allowed_commands = tuple(allowed_commands)
        self.timeout = timeout
        self.output_limit = output_limit
        self.file_limit = 65536
        self.seen = {}
        self.modified = set()
        self.events = []

    def close(self):
        if self.root_fd is not None:
            os.close(self.root_fd)
            self.root_fd = None

    def _parts(self, path, directory=False):
        if not path or "\0" in path or Path(path).is_absolute():
            raise ValueError("Use a relative project path")
        parts = path.split("/")
        # Deny case aliases even on case-sensitive hosts so the policy is portable.
        if any(p == ".." or p.casefold() == ".git" for p in parts):
            raise ValueError("Parent traversal and .git access are denied")
        parts = [p for p in parts if p not in {"", "."}]
        if not directory and not parts:
            raise ValueError("Expected a file path")
        return parts

    def _directory(self, parts):
        fd = os.dup(self.root_fd)
        try:
            for part in parts:
                child = os.open(
                    part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd
                )
                os.close(fd)
                fd = child
            return fd
        except BaseException:
            os.close(fd)
            raise

    def _read(self, parent, name):
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                raise ValueError("Only regular files with one hard link are supported")
            if info.st_size > self.file_limit:
                raise ValueError("File exceeds 64 KiB; narrow the task")
            raw = os.read(fd, self.file_limit + 1)
            if len(raw) > self.file_limit:
                raise ValueError("File exceeds 64 KiB")
            return raw, info
        finally:
            os.close(fd)

    def list_files(self, path):
        fd = self._directory(self._parts(path, directory=True))
        try:
            # scandir is lazy, keeping huge directories bounded too.
            entries = []
            with os.scandir(fd) as iterator:
                for entry in iterator:
                    if self.cancel.is_set():
                        raise Cancelled()
                    if entry.name.casefold() == ".git" or entry.is_symlink():
                        continue
                    entries.append(
                        entry.name
                        + ("/" if entry.is_dir(follow_symlinks=False) else "")
                    )
                    if (
                        len(entries) > 500
                        or sum(len(s.encode()) for s in entries) > self.output_limit
                    ):
                        raise ValueError(
                            "Directory listing exceeds limit; choose a narrower directory"
                        )
            return {"entries": sorted(entries)}
        finally:
            os.close(fd)

    def read_file(self, path):
        parts = self._parts(path)
        parent = self._directory(parts[:-1])
        try:
            raw, _ = self._read(parent, parts[-1])
            content = raw.decode("utf-8")
            if "\0" in content:
                raise ValueError("Binary files are unsupported")
            digest = hashlib.sha256(raw).hexdigest()
            self.seen["/".join(parts)] = digest
            return {"content": content, "sha256": digest}
        finally:
            os.close(parent)

    def apply_patch(self, path, sha256, old_text, new_text, *, interrupts=None):
        parts = self._parts(path)
        normalized = "/".join(parts)
        if self.seen.get(normalized) != sha256:
            raise ValueError("Read the file first and use its current sha256")
        if not old_text or old_text == new_text or "\0" in old_text + new_text:
            raise ValueError("Patch needs a nonempty changed text span, without NULs")
        parent = self._directory(parts[:-1])
        temporary = None
        try:
            raw, info = self._read(parent, parts[-1])
            if hashlib.sha256(raw).hexdigest() != sha256:
                raise ValueError(
                    "File changed since read; read again before proposing a patch"
                )
            before = raw.decode("utf-8")
            if before.count(old_text) != 1:
                raise ValueError("old_text must match exactly once")
            after = before.replace(old_text, new_text, 1)
            encoded = after.encode("utf-8")
            if len(encoded) > self.file_limit:
                raise ValueError("Patched file exceeds 64 KiB")
            diff = "".join(
                difflib.unified_diff(
                    before.splitlines(keepends=True),
                    after.splitlines(keepends=True),
                    fromfile=normalized,
                    tofile=normalized,
                )
            )
            # Include exact strings for no-newline-at-EOF changes and invisible characters.
            preview = diff + f"\nExact replacement: {old_text!r} -> {new_text!r}"
            if not self.approve("patch", preview):
                return {"status": "denied", "path": normalized, "executed": False}
            if self.cancel.is_set():
                raise Cancelled()
            # Re-open from the anchored root after approval; reject renamed/symlink parents.
            fresh_parent = self._directory(parts[:-1])
            try:
                if (
                    os.fstat(fresh_parent).st_ino != os.fstat(parent).st_ino
                    or os.fstat(fresh_parent).st_dev != os.fstat(parent).st_dev
                ):
                    raise ValueError("Parent directory changed during approval")
            finally:
                os.close(fresh_parent)
            current, current_info = self._read(parent, parts[-1])
            if current != raw or (current_info.st_ino, current_info.st_dev) != (
                info.st_ino,
                info.st_dev,
            ):
                raise ValueError("File changed during approval; patch not applied")
            temporary = ".cognix-" + secrets.token_hex(12)
            fd = os.open(
                temporary,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                info.st_mode & 0o777,
                dir_fd=parent,
            )
            with os.fdopen(fd, "wb") as target:
                os.fchmod(target.fileno(), info.st_mode & 0o777)
                target.write(encoded)
                target.flush()
                os.fsync(target.fileno())
            if self.cancel.is_set():
                raise Cancelled()
            # SIGINT requests cancellation here, but cannot split the committed
            # rename from its evidence. Approved bytes may already be visible.
            with defer_sigint(self.cancel, until=interrupts) as canceled:
                if canceled():
                    raise Cancelled()
                os.rename(temporary, parts[-1], src_dir_fd=parent, dst_dir_fd=parent)
                temporary = None
                self.modified.add(normalized)
                self.seen[normalized] = hashlib.sha256(encoded).hexdigest()
                return {
                    "path": normalized,
                    "sha256": self.seen[normalized],
                    "executed": True,
                }
        finally:
            if temporary:
                try:
                    os.unlink(temporary, dir_fd=parent)
                except FileNotFoundError:
                    pass
            os.close(parent)

    def run_command(self, argv, *, interrupts=None):
        if tuple(argv) not in self.allowed_commands:
            return {
                "status": "not_run",
                "executed": False,
                "error": "Command is outside the exact --allow-command policy",
            }
        # Detect absent isolation before prompting, without running a command.
        commands.sandbox_argv(self.root, self.root, argv)
        if not self.approve(
            "command",
            "argv = "
            + repr(argv)
            + "\nRead-only project; no network; scratch writes only.",
        ):
            return {"status": "denied", "executed": False}
        if self.cancel.is_set():
            raise Cancelled()
        # Verify root still denotes the directory held by the file tools.
        info = self.root.stat()
        anchored = os.fstat(self.root_fd)
        if (info.st_ino, info.st_dev) != (anchored.st_ino, anchored.st_dev):
            raise ValueError("Project root changed during session")
        return commands.run(
            self.root,
            argv,
            self.cancel,
            self.timeout,
            self.output_limit,
            interrupts=interrupts,
        )

    def execute(self, name, arguments):
        result = {"status": "failed", "executed": False}
        try:
            with ExitStack() as interrupts:
                try:
                    if self.cancel.is_set():
                        raise Cancelled()
                    validate(name, arguments)
                    options = (
                        {"interrupts": interrupts}
                        if name in {"apply_patch", "run_command"}
                        else {}
                    )
                    result = {
                        "status": "ok",
                        **getattr(self, name)(**arguments, **options),
                    }
                except (Cancelled, KeyboardInterrupt):
                    self.cancel.set()
                    result = {"status": "canceled", "executed": False}
                except (ValueError, OSError, UnicodeError) as exc:
                    result = {
                        "status": "not_run" if name == "run_command" else "failed",
                        "executed": False,
                        "error": str(exc)[:1000],
                    }
                self.events.append(
                    {"tool": name, "arguments": arguments, "result": result}
                )
        except KeyboardInterrupt:
            # The normal handler can fire as ExitStack restores it. The result
            # and ledger already own the evidence; cancellation must retain it.
            self.cancel.set()
        if self.cancel.is_set() and result["status"] in {"ok", "passed"}:
            result["status"] = "canceled"
        return result
