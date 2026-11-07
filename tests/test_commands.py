import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from cognix.commands import SandboxUnavailable, parse_command, run, sandbox_argv
from cognix.runtime import Runtime


def verification_python():
    if sys.platform == "darwin":
        path = Path("/opt/homebrew/bin/python3")
        if path.exists():
            return str(path)
        return "/usr/bin/python3"
    return sys.executable


class CommandTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.python = verification_python()
        with tempfile.TemporaryDirectory() as directory:
            try:
                result = run(
                    Path(directory).resolve(),
                    [cls.python, "-c", "print('ready')"],
                    threading.Event(),
                )
            except SandboxUnavailable as exc:
                raise unittest.SkipTest(str(exc))
            if result["status"] != "passed":
                raise unittest.SkipTest(f"OS sandbox/runtime unavailable: {result}")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cancel = threading.Event()

    def execute(self, code, **limits):
        return run(self.root, [self.python, "-c", code], self.cancel, **limits)

    def test_exit_output_and_failure(self):
        self.assertEqual(self.execute("print('yes')")["output"], "yes\n")
        failed = self.execute("raise SystemExit(3)")
        self.assertEqual((failed["status"], failed["exit_code"]), ("failed", 3))

    def test_sandbox_blocks_outside_reads_root_writes_network_and_symlinks(self):
        with tempfile.TemporaryDirectory() as outside:
            secret = Path(outside).resolve() / "secret"
            secret.write_text("private")
            (self.root / "link").symlink_to(secret)
            probes = [
                f"open({str(secret)!r}).read()",
                "open('link').read()",
                "open('created', 'w').write('bad')",
                f"open({str(secret)!r}, 'w').write('bad')",
                "import socket; socket.create_connection(('127.0.0.1', 11434), timeout=1)",
            ]
            for code in probes:
                with self.subTest(code=code):
                    self.assertEqual(self.execute(code)["status"], "failed")
            self.assertEqual(secret.read_text(), "private")
            self.assertFalse((self.root / "created").exists())
        (self.root / ".git").mkdir()
        (self.root / ".git/config").write_text("private metadata")
        self.assertEqual(self.execute("open('.git/config').read()")["status"], "failed")

    def test_scratch_works_and_parent_environment_is_not_inherited(self):
        import os
        from unittest.mock import patch

        with patch.dict(os.environ, {"COGNIX_TEST_SECRET": "must not inherit"}):
            result = self.execute(
                "import os, tempfile; assert 'COGNIX_TEST_SECRET' not in os.environ; f=tempfile.TemporaryFile(); f.write(b'ok'); print('scratch')"
            )
        self.assertEqual(result["status"], "passed", result)

    def test_time_output_and_closed_pipe_limits(self):
        for code in [
            "import time; time.sleep(20)",
            "import os,time; os.close(1); os.close(2); time.sleep(20)",
        ]:
            started = time.monotonic()
            result = self.execute(code, timeout=0.2)
            self.assertEqual(result["status"], "limit_reached")
            self.assertLess(time.monotonic() - started, 3)
        result = self.execute("print('x'*100000)", output_limit=100)
        self.assertEqual(result["status"], "limit_reached")
        self.assertLessEqual(len(result["output"].encode()), 100)

    def test_cancel_kills_execution_and_stops_wait(self):
        timer = threading.Timer(0.2, self.cancel.set)
        timer.start()
        self.addCleanup(timer.cancel)
        result = self.execute("import time; time.sleep(20)")
        self.assertEqual(result["status"], "canceled")
        self.assertIsNotNone(result["exit_code"])
        self.assertTrue(result["executed"])

    def test_sigint_after_launch_records_execution_and_reaps_process(self):
        popen = subprocess.Popen
        killpg = os.killpg
        launched = []
        handler = signal.getsignal(signal.SIGINT)

        def launch(*args, **kwargs):
            process = popen(*args, **kwargs)
            launched.append(process)
            self.addCleanup(lambda: process.poll() is None and process.kill())
            # Reproduce SIGINT before Popen's caller can take ownership.
            os.kill(os.getpid(), signal.SIGINT)
            return process

        def kill_group(pid, sig):
            # A second interruption must not abandon cleanup either.
            os.kill(os.getpid(), signal.SIGINT)
            killpg(pid, sig)

        argv = [self.python, "-c", "import time; time.sleep(20)"]
        runtime = Runtime(self.root, lambda *_: True, self.cancel, [tuple(argv)])
        self.addCleanup(runtime.close)
        started = time.monotonic()
        with (
            patch("cognix.commands.subprocess.Popen", side_effect=launch),
            patch("cognix.commands.os.killpg", side_effect=kill_group),
        ):
            result = runtime.execute("run_command", {"argv": argv})
        self.assertLess(time.monotonic() - started, 3)
        self.assertEqual(len(launched), 1)
        self.assertEqual(result["status"], "canceled")
        self.assertTrue(result["executed"])
        self.assertEqual(result["exit_code"], -signal.SIGKILL)
        self.assertIsNotNone(launched[0].poll())
        self.assertTrue(launched[0].stdout.closed)
        with self.assertRaises(ProcessLookupError):
            os.killpg(launched[0].pid, 0)
        self.assertEqual(runtime.events[-1]["result"], result)
        self.assertTrue(self.cancel.is_set())
        self.assertEqual(signal.getsignal(signal.SIGINT), handler)

    @unittest.skipUnless(
        sys.platform == "darwin", "macOS single-process sandbox policy"
    )
    def test_fork_and_spawn_cannot_escape_cancellation(self):
        self.assertEqual(self.execute("import os; os.fork()")["status"], "failed")
        self.assertEqual(
            self.execute(
                "import subprocess; subprocess.run(['/usr/bin/true'], check=True)"
            )["status"],
            "failed",
        )

    def test_already_canceled_does_not_launch(self):
        self.cancel.set()
        result = self.execute("raise SystemExit('should not run')")
        self.assertEqual(result, {"status": "canceled", "executed": False})


class PolicyTests(unittest.TestCase):
    def test_invalid_policy(self):
        for value in [
            "[]",
            "{}",
            '["python", "-c", "x"]',
            '["/missing"]',
            "[2]",
            '["/bin/sh", ""]',
        ]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                parse_command(value)
        self.assertEqual(
            parse_command('["/bin/sh", "-c", "echo x"]'), ("/bin/sh", "-c", "echo x")
        )

    def test_unsupported_platform_fails_closed(self):
        from unittest.mock import patch

        with (
            patch("cognix.commands.sys.platform", "unsupported"),
            self.assertRaises(SandboxUnavailable),
        ):
            sandbox_argv(Path("/project"), Path("/scratch"), ["/bin/true"])
