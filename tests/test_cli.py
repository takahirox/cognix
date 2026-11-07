import io
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

from test_provider import Server, completion

from cognix.cli import main, render, terminal_text

PROBE = completion(
    {
        "role": "assistant",
        "content": "",
        "tool_calls": [
            {"function": {"name": "cognix_probe", "arguments": {"value": "ok"}}}
        ],
    }
)


class CLITests(unittest.TestCase):
    def server(self, more):
        server = Server(
            [
                (200, {"version": "fixture"}),
                (200, {"capabilities": ["completion", "tools"]}),
                (200, PROBE),
                *more,
            ]
        )
        self.addCleanup(server.close)
        return server

    def test_interactive_start_task_approval_and_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "file").write_text("before")
            import hashlib

            digest = hashlib.sha256(b"before").hexdigest()
            server = self.server(
                [
                    (
                        200,
                        completion(
                            {
                                "role": "assistant",
                                "tool_calls": [
                                    {
                                        "id": "r",
                                        "function": {
                                            "name": "read_file",
                                            "arguments": {"path": "file"},
                                        },
                                    }
                                ],
                            }
                        ),
                    ),
                    (
                        200,
                        completion(
                            {
                                "role": "assistant",
                                "tool_calls": [
                                    {
                                        "id": "p",
                                        "function": {
                                            "name": "apply_patch",
                                            "arguments": {
                                                "path": "file",
                                                "sha256": digest,
                                                "old_text": "before",
                                                "new_text": "after",
                                            },
                                        },
                                    }
                                ],
                            }
                        ),
                    ),
                    (200, completion()),
                ]
            )
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "cognix",
                    "--root",
                    directory,
                    "--ollama-url",
                    server.url,
                    "--model",
                    "chosen:tag",
                ],
                input="Edit file\ny\n/exit\n",
                text=True,
                capture_output=True,
                timeout=15,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("Approve this action?", result.stdout)
            self.assertIn("Modified files: file", result.stdout)
            self.assertEqual((root / "file").read_text(), "after")
            self.assertEqual(server.requests[2][1]["model"], "chosen:tag")

    def test_eof_denies_approval_and_invalid_inputs(self):
        from cognix.cli import approve

        with (
            patch("builtins.input", side_effect=EOFError),
            redirect_stdout(io.StringIO()),
        ):
            self.assertFalse(approve("command", "preview"))
        for args in [
            ["--task", ""],
            ["--root", "/missing-cognix-project"],
            ["--num-ctx", "100"],
            ["--allow-command", "bad json"],
        ]:
            with self.subTest(args=args), redirect_stdout(io.StringIO()):
                self.assertEqual(main(args), 1)
        result = subprocess.run(
            [sys.executable, "-m", "cognix", "--max-steps", "0"],
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)

    def test_configurable_default(self):
        from cognix.cli import parser

        with patch.dict(os.environ, {"COGNIX_MODEL": "another:tag"}):
            self.assertEqual(parser().parse_args([]).model, "another:tag")
            self.assertEqual(
                parser().parse_args(["--model", "override:tag"]).model, "override:tag"
            )

    def test_sigint_during_model_and_input(self):
        def delay():
            time.sleep(1)
            return completion()

        for at_input in [False, True]:
            server = self.server([] if at_input else [(200, delay)])
            argv = [sys.executable, "-u", "-m", "cognix", "--ollama-url", server.url]
            if not at_input:
                argv += ["--task", "inspect"]
            process = subprocess.Popen(
                argv,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if len(server.requests) >= (3 if at_input else 4):
                        break
                    time.sleep(0.02)
                else:
                    self.fail("CLI did not reach cancellation target")
                process.send_signal(signal.SIGINT)
                output, errors = process.communicate(timeout=3)
                self.assertEqual(process.returncode, 130, output + errors)
                self.assertNotIn("Traceback", output + errors)
                self.assertIn("anceled", output)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()

    def test_final_answers_and_blocker_explanations_are_visible(self):
        for reply in [
            "The divide function returns None when the denominator is zero.",
            "Cannot verify: pytest is missing. Install pytest before retrying.\x1b[2J",
        ]:
            with self.subTest(reply=reply), tempfile.TemporaryDirectory() as directory:
                server = self.server(
                    [(200, completion({"role": "assistant", "content": reply}))]
                )
                output = io.StringIO()
                with redirect_stdout(output):
                    result = main(
                        [
                            "--root",
                            directory,
                            "--ollama-url",
                            server.url,
                            "--task",
                            "Inspect the function and explain any verification blocker",
                        ]
                    )
                self.assertEqual(result, 0)
                model_response, evidence = output.getvalue().split(
                    "\nRecorded execution evidence:\n"
                )
                self.assertIn(
                    "Model response (not verification evidence):", model_response
                )
                self.assertIn(terminal_text(reply), model_response)
                self.assertNotIn("\x1b", output.getvalue())
                self.assertNotIn(terminal_text(reply), evidence)
                self.assertIn("Verification: not run (no command executed)", evidence)
                self.assertNotIn("Check argv:", evidence)

    def test_reporting_separates_model_claims_from_evidence_and_is_terminal_safe(self):
        checks = []
        for status in [
            "passed",
            "failed",
            "denied",
            "canceled",
            "not_run",
            "limit_reached",
        ]:
            checks.append(
                {
                    "tool": "run_command",
                    "arguments": {"argv": ["/bin/true"]},
                    "result": {
                        "status": status,
                        "executed": status in {"passed", "failed"},
                    },
                }
            )
        checks.append(
            {"tool": "run_command", "arguments": None, "result": {"status": "not_run"}}
        )
        output = io.StringIO()
        with redirect_stdout(output):
            render(
                {
                    "status": "final_answer",
                    "error": None,
                    "model_reply": "All tests passed, trust me",
                    "modified_files": [],
                    "checks": checks,
                    "events": checks,
                }
            )
        model_response, evidence = output.getvalue().split(
            "\nRecorded execution evidence:\n"
        )
        self.assertIn("All tests passed, trust me", model_response)
        self.assertIn("Model response (not verification evidence):", model_response)
        self.assertNotIn("trust me", evidence)
        for status in [
            "passed",
            "failed",
            "denied",
            "canceled",
            "not_run",
            "limit_reached",
        ]:
            self.assertIn(status + ";", evidence)
        self.assertEqual(terminal_text("x\x1b[2J\ry"), "x\\x1b[2J\\x0dy")

    def test_sigint_stops_approved_running_command_and_records_evidence(self):
        from test_commands import CommandTests, verification_python

        CommandTests.setUpClass()
        command = [verification_python(), "-c", "import time; time.sleep(20)"]
        server = self.server(
            [
                (
                    200,
                    completion(
                        {
                            "role": "assistant",
                            "tool_calls": [
                                {
                                    "id": "check",
                                    "function": {
                                        "name": "run_command",
                                        "arguments": {"argv": command},
                                    },
                                }
                            ],
                        }
                    ),
                )
            ]
        )
        with tempfile.TemporaryDirectory() as directory:
            process = subprocess.Popen(
                [
                    sys.executable,
                    "-u",
                    "-m",
                    "cognix",
                    "--root",
                    directory,
                    "--ollama-url",
                    server.url,
                    "--task",
                    "verify",
                    "--allow-command",
                    json.dumps(command),
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                process.stdin.write("y\n")
                process.stdin.flush()
                deadline = time.monotonic() + 5
                while len(server.requests) < 4 and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertEqual(len(server.requests), 4)
                time.sleep(0.3)
                process.send_signal(signal.SIGINT)
                output, errors = process.communicate(timeout=3)
                self.assertEqual(process.returncode, 130, output + errors)
                self.assertIn("canceled; executed=True; exit=-9", output)
                self.assertEqual(len(server.requests), 4)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
