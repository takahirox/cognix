import copy
import hashlib
import json
import shutil
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from test_commands import verification_python

from cognix.agent import Agent
from cognix.provider import ContextLimit, ProviderError
from cognix.runtime import Runtime


def call(name, arguments, id="call-1"):
    return {
        "role": "assistant",
        "content": "",
        "tool_calls": [{"id": id, "function": {"name": name, "arguments": arguments}}],
    }


def final(content="Done"):
    return {"role": "assistant", "content": content}


class MockProvider:
    def __init__(self, responses):
        self.responses = iter(responses)
        self.requests = []

    def chat(self, messages, tools, cancel):
        self.requests.append(copy.deepcopy(messages))
        response = next(self.responses)
        if isinstance(response, Exception):
            raise response
        if callable(response):
            response = response(messages)
        return response


class AgentTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cancel = threading.Event()
        self.runtime = Runtime(self.root, lambda *_: True, self.cancel)
        self.addCleanup(self.runtime.close)
        (self.root / "source").write_text("user content")

    def agent(self, responses, **kwargs):
        self.provider = MockProvider(responses)
        return Agent(self.provider, self.runtime, self.cancel, **kwargs)

    def test_order_ids_and_results_return_to_model(self):
        first = call("read_file", {"path": "source"}, "read-id")
        first["tool_calls"].append(
            call("list_files", {"path": "."}, "list-id")["tool_calls"][0]
        )
        agent = self.agent([first, final()])
        self.assertEqual(agent.run("inspect")["status"], "final_answer")
        second = self.provider.requests[1]
        self.assertEqual(
            [m["role"] for m in second], ["system", "user", "assistant", "tool", "tool"]
        )
        self.assertEqual(
            [m["tool_call_id"] for m in second[-2:]], ["read-id", "list-id"]
        )
        self.assertEqual(json.loads(second[-2]["content"])["content"], "user content")
        agent.provider = MockProvider([final()])
        agent.run("follow-up")
        self.assertEqual(
            [m["content"] for m in agent.messages if m["role"] == "user"],
            ["inspect", "follow-up"],
        )

    def test_missing_id_generated_and_invalid_arguments_return_structured_error(self):
        response = call("read_file", {"path": 1})
        del response["tool_calls"][0]["id"]
        agent = self.agent([response, final()])
        self.assertEqual(agent.run("inspect")["status"], "final_answer")
        result = agent.messages[-2]
        self.assertEqual(result["tool_call_id"], "cognix-1-0")
        self.assertEqual(json.loads(result["content"])["status"], "failed")

    def test_malformed_batch_and_duplicate_ids_execute_nothing(self):
        cases = [
            {"role": "assistant", "tool_calls": "bad"},
            {"role": "assistant", "tool_calls": [{}]},
            {
                "role": "assistant",
                "tool_calls": [{"id": [], "function": {"name": "read_file"}}],
            },
        ]
        repeated = call("read_file", {"path": "source"})
        repeated["tool_calls"] *= 2
        cases.append(repeated)
        for response in cases:
            with self.subTest(response=response):
                self.runtime.events.clear()
                self.assertEqual(
                    self.agent([response]).run("inspect")["status"], "error"
                )
                self.assertEqual(self.runtime.events, [])

    def test_step_context_and_provider_failure_stops(self):
        agent = self.agent([call("read_file", {"path": "missing"})], max_steps=1)
        self.assertEqual(agent.run("inspect")["status"], "limit_reached")
        self.assertEqual(len(self.provider.requests), 1)
        agent = self.agent([], context_budget=1)
        self.assertEqual(agent.run("inspect")["status"], "context_limit")
        self.assertEqual(len(self.provider.requests), 0)
        for exception, status in [
            (ContextLimit("full"), "context_limit"),
            (ProviderError("offline"), "error"),
        ]:
            self.assertEqual(self.agent([exception]).run("inspect")["status"], status)

    def test_cancellation_before_and_during_model_prevents_tools(self):
        self.cancel.set()
        self.assertEqual(self.agent([]).run("inspect")["status"], "canceled")
        self.cancel.clear()

        def cancel(messages):
            self.cancel.set()
            return call("read_file", {"path": "source"})

        agent = self.agent([cancel])
        self.assertEqual(agent.run("inspect")["status"], "canceled")
        self.assertEqual(self.runtime.events, [])

    def test_denied_patch_returns_result_and_unverified_model_claim_is_not_evidence(
        self,
    ):
        digest = hashlib.sha256(b"user content").hexdigest()
        self.runtime.approve = lambda *_: False
        agent = self.agent(
            [
                call("read_file", {"path": "source"}, "r"),
                call(
                    "apply_patch",
                    {
                        "path": "source",
                        "sha256": digest,
                        "old_text": "content",
                        "new_text": "edit",
                    },
                    "p",
                ),
                final("All tests passed"),
            ]
        )
        report = agent.run("fix")
        self.assertEqual(report["modified_files"], [])
        self.assertEqual(report["checks"], [])
        self.assertEqual(report["events"][-1]["result"]["status"], "denied")
        self.assertEqual((self.root / "source").read_text(), "user content")

    def test_cancel_during_approval_records_queued_checks_as_unrun(self):
        argv = ["/usr/bin/true"]
        self.runtime.allowed_commands = (tuple(argv),)

        def approval(*_):
            self.cancel.set()
            return True

        self.runtime.approve = approval
        batch = call("run_command", {"argv": argv}, "first")
        batch["tool_calls"].append(
            call("run_command", {"argv": argv}, "queued")["tool_calls"][0]
        )
        with (
            patch("cognix.commands.sandbox_argv"),
            patch("cognix.commands.run") as execute,
        ):
            report = self.agent([batch]).run("check")
        execute.assert_not_called()
        self.assertEqual(report["status"], "canceled")
        self.assertEqual(
            [e["result"]["status"] for e in report["checks"]], ["canceled", "not_run"]
        )

    def test_e2e_fixture_failed_check_then_correction(self):
        from test_commands import CommandTests

        CommandTests.setUpClass()  # Skip only if actual OS sandbox is unavailable.
        fixture = Path(__file__).parent / "fixtures/division"
        shutil.copytree(
            fixture,
            self.root,
            dirs_exist_ok=True,
            ignore=shutil.ignore_patterns("__pycache__"),
        )
        (self.root / "unrelated").write_text("pre-existing edit")
        original = (self.root / "main.py").read_text()
        argv = [
            verification_python(),
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
        self.runtime.allowed_commands = (tuple(argv),)
        approvals = []
        self.runtime.approve = lambda kind, preview: (
            approvals.append((kind, preview)) or True
        )
        digest = hashlib.sha256(original.encode()).hexdigest()
        fixed = original.replace(
            "return numerator / denominator",
            "return numerator / denominator if denominator else None",
        )
        agent = self.agent(
            [
                call("list_files", {"path": "."}, "list"),
                call("read_file", {"path": "main.py"}, "read"),
                call("run_command", {"argv": argv}, "fail"),
                call(
                    "apply_patch",
                    {
                        "path": "main.py",
                        "sha256": digest,
                        "old_text": "return numerator / denominator",
                        "new_text": "return numerator / denominator if denominator else None",
                    },
                    "patch",
                ),
                call("run_command", {"argv": argv}, "pass"),
                final(),
            ],
            context_budget=32768,
        )
        report = agent.run("Fix division by zero; return None for zero and verify")
        self.assertEqual(report["status"], "final_answer", report)
        self.assertEqual(
            [c["result"]["status"] for c in report["checks"]],
            ["failed", "passed"],
            report,
        )
        self.assertIn("ZeroDivisionError", report["checks"][0]["result"]["output"])
        self.assertEqual([a[0] for a in approvals], ["command", "patch", "command"])
        self.assertEqual((self.root / "main.py").read_text(), fixed)
        self.assertEqual((self.root / "unrelated").read_text(), "pre-existing edit")
        self.assertEqual(report["modified_files"], ["main.py"])
        # Failed check was sent to model before the correcting patch was requested.
        self.assertEqual(
            json.loads(self.provider.requests[3][-1]["content"])["status"], "failed"
        )
