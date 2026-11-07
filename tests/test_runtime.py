import os
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from cognix.runtime import Runtime


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cancel = threading.Event()
        self.previews = []
        self.runtime = Runtime(self.root, self.approve, self.cancel)
        self.addCleanup(self.runtime.close)
        (self.root / "main.py").write_text(
            "# user note\ndef divide(a, b):\n    return a / b\n"
        )

    def approve(self, kind, preview):
        self.previews.append((kind, preview))
        return True

    def call(self, name, **args):
        return self.runtime.execute(name, args)

    def edit(self, **overrides):
        read = self.call("read_file", path="main.py")
        args = {
            "path": "main.py",
            "sha256": read["sha256"],
            "old_text": "return a / b",
            "new_text": "return a / b if b else None",
        }
        args.update(overrides)
        return self.call("apply_patch", **args)

    def test_tools_preserve_user_changes_and_preview_before_write(self):
        (self.root / "unrelated.txt").write_text("uncommitted")
        self.assertIn("main.py", self.call("list_files", path=".")["entries"])

        def approval(kind, preview):
            self.assertEqual(kind, "patch")
            self.assertIn("-    return a / b", preview)
            self.assertNotIn("if b", (self.root / "main.py").read_text())
            return True

        self.runtime.approve = approval
        self.assertEqual(self.edit()["status"], "ok")
        self.assertTrue((self.root / "main.py").read_text().startswith("# user note"))
        self.assertEqual((self.root / "unrelated.txt").read_text(), "uncommitted")
        self.assertEqual(self.runtime.modified, {"main.py"})

    def test_denial_and_cancel_at_approval_write_nothing(self):
        original = (self.root / "main.py").read_bytes()
        self.runtime.approve = lambda *_: False
        self.assertEqual(self.edit()["status"], "denied")

        def cancel(*_):
            self.cancel.set()
            return True

        self.runtime.approve = cancel
        self.assertEqual(self.edit()["status"], "canceled")
        self.assertEqual((self.root / "main.py").read_bytes(), original)
        self.assertEqual(list(self.root.glob(".cognix-*")), [])

    def test_edit_during_approval_is_preserved(self):
        def concurrent(*_):
            (self.root / "main.py").write_text("new user content")
            return True

        self.runtime.approve = concurrent
        self.assertEqual(self.edit()["status"], "failed")
        self.assertEqual((self.root / "main.py").read_text(), "new user content")

    def test_parent_swap_during_approval_is_denied(self):
        (self.root / "dir").mkdir()
        (self.root / "dir" / "file").write_text("before")
        read = self.call("read_file", path="dir/file")

        def concurrent(*_):
            (self.root / "dir").rename(self.root / "old")
            (self.root / "dir").symlink_to(self.root / "old", target_is_directory=True)
            return True

        self.runtime.approve = concurrent
        result = self.call(
            "apply_patch",
            path="dir/file",
            sha256=read["sha256"],
            old_text="before",
            new_text="after",
        )
        self.assertEqual(result["status"], "failed")
        self.assertEqual((self.root / "old/file").read_text(), "before")

    def test_outside_symlinks_hardlinks_and_git_are_denied(self):
        with tempfile.TemporaryDirectory() as outside:
            target = Path(outside) / "secret"
            target.write_text("outside")
            (self.root / "escape").symlink_to(target)
            (self.root / "linkdir").symlink_to(outside, target_is_directory=True)
            os.link(target, self.root / "hardlink")
            (self.root / ".git").mkdir()
            (self.root / ".git/config").write_text("git config")
            paths = [
                str(target),
                "../secret",
                "escape",
                "linkdir/secret",
                "hardlink",
                ".git/config",
                "a/../../secret",
            ]
            for path in paths:
                with self.subTest(path=path):
                    self.assertEqual(
                        self.call("read_file", path=path)["status"], "failed"
                    )
            self.assertEqual(
                self.call("list_files", path="linkdir")["status"], "failed"
            )
            self.assertNotIn("escape", self.call("list_files", path=".")["entries"])
            self.assertEqual(target.read_text(), "outside")

    def test_bad_arguments_and_precise_failures(self):
        for name, args in [
            ("unknown", {}),
            ("read_file", {}),
            ("read_file", {"path": 2}),
            ("list_files", {"path": ".", "extra": True}),
            ("run_command", {"argv": "sh"}),
            ("run_command", {"argv": []}),
            ("run_command", {"argv": ["x\0y"]}),
        ]:
            with self.subTest(name=name, args=args):
                self.assertNotIn(
                    self.runtime.execute(name, args)["status"], {"ok", "passed"}
                )
        self.assertIn("No such file", self.call("read_file", path="missing")["error"])
        (self.root / "large").write_bytes(b"x" * 65537)
        self.assertIn("64 KiB", self.call("read_file", path="large")["error"])
        (self.root / "binary").write_bytes(b"\0")
        self.assertEqual(self.call("read_file", path="binary")["status"], "failed")
        os.mkfifo(self.root / "fifo")
        self.assertEqual(self.call("read_file", path="fifo")["status"], "failed")

    def test_patch_preconditions(self):
        for overrides in [
            {"old_text": ""},
            {"old_text": "missing"},
            {"sha256": "bad"},
            {"new_text": "x" * 65537},
        ]:
            with self.subTest(overrides=overrides):
                self.assertEqual(self.edit(**overrides)["status"], "failed")
        (self.root / "main.py").write_text("repeat repeat")
        self.assertEqual(self.edit(old_text="repeat")["status"], "failed")
        self.assertEqual(self.previews, [])

    def test_stale_hash_rejected(self):
        read = self.call("read_file", path="main.py")
        (self.root / "main.py").write_text("user edit")
        self.assertEqual(
            self.call(
                "apply_patch",
                path="main.py",
                sha256=read["sha256"],
                old_text="user edit",
                new_text="lost",
            )["status"],
            "failed",
        )

    def test_command_policy_and_denial_do_not_execute(self):
        self.runtime.allowed_commands = (("/usr/bin/true",),)
        with patch("cognix.commands.run") as run, patch("cognix.commands.sandbox_argv"):
            self.assertEqual(
                self.call("run_command", argv=["/bin/sh", "-c", "true"])["status"],
                "not_run",
            )
            self.runtime.approve = lambda *_: False
            self.assertEqual(
                self.call("run_command", argv=["/usr/bin/true"])["status"], "denied"
            )
            run.assert_not_called()

    def test_listing_limit(self):
        for i in range(501):
            (self.root / str(i)).touch()
        self.assertEqual(self.call("list_files", path=".")["status"], "failed")
