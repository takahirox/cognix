import json
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from cognix.provider import Cancelled, ContextLimit, Ollama, ProviderError


class Server:
    def __init__(self, responses):
        self.responses = iter(responses)
        self.requests = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                self.reply()

            def do_POST(self):
                self.reply()

            def reply(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                owner.requests.append((self.path, json.loads(body) if body else None))
                status, value = next(owner.responses)
                if callable(value):
                    value = value()
                data = value if isinstance(value, bytes) else json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                try:
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.http.server_port}"

    def close(self):
        self.http.shutdown()
        self.http.server_close()
        self.thread.join()


def completion(message=None, **extra):
    return {
        "done": True,
        "message": message or {"role": "assistant", "content": "done"},
        **extra,
    }


class ProviderTests(unittest.TestCase):
    def provider(self, responses, **kwargs):
        server = Server(responses)
        self.addCleanup(server.close)
        self.server = server
        return Ollama("custom:tag", server.url, **kwargs)

    def test_validate_and_round_trip_native_protocol(self):
        probe = {
            "role": "assistant",
            "content": "",
            "thinking": "probe",
            "tool_calls": [
                {
                    "id": "id-1",
                    "function": {"name": "cognix_probe", "arguments": {"value": "ok"}},
                }
            ],
        }
        provider = self.provider(
            [
                (200, {"version": "test-version"}),
                (
                    200,
                    {
                        "capabilities": ["completion", "tools"],
                        "model_info": {"arch.context_length": 32768},
                    },
                ),
                (200, completion(probe)),
                (200, completion()),
            ]
        )
        cancel = threading.Event()
        provider.validate(cancel)
        messages = [
            probe,
            {
                "role": "tool",
                "tool_name": "cognix_probe",
                "tool_call_id": "id-1",
                "content": "ok",
            },
        ]
        self.assertEqual(provider.chat(messages, [], cancel)["content"], "done")
        self.assertEqual(provider.version, "test-version")
        request = self.server.requests[-1][1]
        self.assertEqual(request["model"], "custom:tag")
        self.assertFalse(request["stream"])
        self.assertEqual(request["messages"], messages)

    def test_missing_model_unsupported_cloud_and_behavior(self):
        cases = [
            [(404, {"error": "model not found"})],
            [(200, {"capabilities": ["completion"]})],
            [(200, {"capabilities": None})],
            [(200, {"capabilities": ["completion", "tools"], "model_info": []})],
            [
                (
                    200,
                    {
                        "remote_host": "https://ollama.com",
                        "capabilities": ["completion", "tools"],
                    },
                )
            ],
            [(200, {"capabilities": ["completion", "tools"]}), (200, completion())],
        ]
        for responses in cases:
            with self.subTest(responses=responses):
                provider = self.provider([(200, {"version": "test"}), *responses])
                with self.assertRaises(ProviderError):
                    provider.validate(threading.Event())

    def test_limits_malformed_and_http_failures(self):
        for response, exception in [
            ((500, {"error": "context exceeded"}), ContextLimit),
            ((500, {"error": "internal failure"}), ProviderError),
            ((200, b"not JSON"), ProviderError),
            ((200, []), ProviderError),
            ((200, b"x" * (1024 * 1024 + 1)), ProviderError),
            ((200, {"done": False}), ProviderError),
            ((200, completion(done_reason="length")), ContextLimit),
        ]:
            with self.subTest(response=str(response)[:100]):
                provider = self.provider([response])
                with self.assertRaises(exception):
                    provider.chat([], [], threading.Event())

    def test_missing_server(self):
        server = Server([])
        url = server.url
        server.close()
        with self.assertRaisesRegex(ProviderError, "ollama serve"):
            Ollama("tag", url).request("/api/version", None, threading.Event())

    def test_cancel_and_timeout_during_http(self):
        for cancellation in [True, False]:

            def delayed():
                time.sleep(0.4)
                return completion()

            provider = self.provider(
                [(200, delayed)], timeout=2 if cancellation else 0.1
            )
            cancel = threading.Event()
            timer = threading.Timer(0.1, cancel.set)
            if cancellation:
                timer.start()
                self.addCleanup(timer.cancel)
            with self.assertRaises(Cancelled if cancellation else ProviderError):
                provider.chat([], [], cancel)

    def test_only_local_origin_and_local_tags(self):
        for url in [
            "https://example.com",
            "http://example.com",
            "http://localhost/path",
            "http://user:pass@localhost",
            "http://localhost?x=1",
        ]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                Ollama("tag", url)
        for tag in ["", "bad tag", "model:cloud"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                Ollama(tag)
