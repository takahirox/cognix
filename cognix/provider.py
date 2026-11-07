"""Ollama's native local chat protocol behind a replaceable interface."""

import copy
import http.client
import json
import threading
import time
from typing import Protocol
from urllib.parse import urlsplit


class ProviderError(Exception):
    pass


class ContextLimit(ProviderError):
    pass


class Cancelled(Exception):
    pass


class Provider(Protocol):
    def chat(self, messages, tools, cancel): ...


def normalize_message(message):
    if not isinstance(message, dict) or message.get("role") != "assistant":
        raise ProviderError("Malformed assistant message from model")
    result = copy.deepcopy(message)
    if not isinstance(result.get("content", ""), str):
        raise ProviderError("Assistant content must be text")
    result.setdefault("content", "")
    calls = result.get("tool_calls", [])
    if not isinstance(calls, list) or len(calls) > 8:
        raise ProviderError(
            "Malformed or excessive tool_calls (maximum 8 per response)"
        )
    for call in calls:
        if (
            not isinstance(call, dict)
            or not isinstance(call.get("function"), dict)
            or not isinstance(call["function"].get("name"), str)
            or ("id" in call and (not isinstance(call["id"], str) or not call["id"]))
        ):
            raise ProviderError("Malformed tool call envelope")
    return result


class Ollama:
    def __init__(self, model, url="http://127.0.0.1:11434", timeout=120, num_ctx=16384):
        parsed = urlsplit(url)
        if (
            parsed.scheme != "http"
            or parsed.hostname not in {"localhost", "127.0.0.1", "::1"}
            or parsed.username
            or parsed.password
            or parsed.path not in {"", "/"}
            or parsed.query
            or parsed.fragment
        ):
            raise ValueError(
                "Ollama URL must be a local HTTP origin (localhost/loopback)"
            )
        if not model.strip() or any(c.isspace() for c in model) or ":cloud" in model:
            raise ValueError(
                "Select a nonempty local Ollama model tag, without whitespace or :cloud"
            )
        self.model = model
        self.host = parsed.hostname
        self.port = parsed.port or 11434
        self.timeout = timeout
        self.num_ctx = num_ctx
        self.version = "unknown"

    def request(self, path, data, cancel):
        """Bound response bytes and waiting; close transport on timeout or cancellation."""
        if cancel.is_set():
            raise Cancelled()
        conn = http.client.HTTPConnection(self.host, self.port, timeout=self.timeout)
        finished = threading.Event()
        box = {}

        def work():
            try:
                body = None if data is None else json.dumps(data).encode()
                conn.request(
                    "GET" if data is None else "POST",
                    path,
                    body,
                    {"Content-Type": "application/json"},
                )
                response = conn.getresponse()
                raw = response.read(1024 * 1024 + 1)
                if len(raw) > 1024 * 1024:
                    raise ProviderError("Ollama response exceeds 1 MiB")
                try:
                    value = json.loads(raw)
                except (ValueError, UnicodeError) as exc:
                    raise ProviderError("Ollama returned invalid JSON") from exc
                if not isinstance(value, dict):
                    raise ProviderError("Ollama returned a non-object response")
                if response.status >= 400 or "error" in value:
                    detail = str(value.get("error", f"HTTP {response.status}"))[:500]
                    if "context" in detail.lower():
                        raise ContextLimit(detail)
                    raise ProviderError(f"Ollama: {detail}")
                box["value"] = value
            except (OSError, http.client.HTTPException) as exc:
                box["error"] = ProviderError(
                    f"Cannot reach Ollama at {self.host}:{self.port}: {exc}. Start 'ollama serve'."
                )
            except Exception as exc:  # noqa: BLE001 -- relay worker failures to the caller
                box["error"] = exc
            finally:
                conn.close()
                finished.set()

        worker = threading.Thread(target=work, daemon=True)
        worker.start()
        deadline = time.monotonic() + self.timeout
        try:
            while not finished.wait(0.05):
                if cancel.is_set():
                    raise Cancelled()
                if time.monotonic() >= deadline:
                    raise ProviderError(
                        "Ollama request timed out; try a smaller local model or increase --model-timeout"
                    )
            if cancel.is_set():
                raise Cancelled()
            if "error" in box:
                raise box["error"]
            return box["value"]
        finally:
            if not finished.is_set() and conn.sock:
                try:
                    conn.sock.shutdown(2)
                except OSError:
                    pass
            conn.close()

    def validate(self, cancel):
        self.version = self.request("/api/version", None, cancel).get(
            "version", "unknown"
        )
        try:
            info = self.request("/api/show", {"model": self.model}, cancel)
        except ProviderError as exc:
            raise ProviderError(
                f"Cannot load {self.model!r}. Install a local tag with 'ollama pull'. {exc}"
            ) from exc
        if info.get("remote_host") or info.get("remote_model"):
            raise ProviderError(
                "Cloud-backed Ollama tags are unsupported; select a local model"
            )
        capabilities = info.get("capabilities", [])
        if (
            not isinstance(capabilities, list)
            or "completion" not in capabilities
            or "tools" not in capabilities
        ):
            raise ProviderError(
                f"{self.model!r} does not advertise completion and tools; choose a tool-capable local model"
            )
        model_info = info.get("model_info", {})
        if not isinstance(model_info, dict):
            raise ProviderError("Ollama returned malformed model_info")
        lengths = [
            v
            for k, v in model_info.items()
            if k.endswith(".context_length") and type(v) is int
        ]
        if lengths and self.num_ctx > min(lengths):
            raise ContextLimit(
                f"--num-ctx {self.num_ctx} exceeds model context {min(lengths)}"
            )
        probe = [
            {
                "type": "function",
                "function": {
                    "name": "cognix_probe",
                    "description": "Compatibility probe; call with value='ok'.",
                    "parameters": {
                        "type": "object",
                        "properties": {"value": {"type": "string"}},
                        "required": ["value"],
                    },
                },
            }
        ]
        message = self.chat(
            [
                {
                    "role": "user",
                    "content": "Call cognix_probe with value ok now. Do not answer with prose.",
                }
            ],
            probe,
            cancel,
        )
        calls = message.get("tool_calls", [])
        if (
            len(calls) != 1
            or calls[0]["function"].get("name") != "cognix_probe"
            or calls[0]["function"].get("arguments") != {"value": "ok"}
        ):
            raise ProviderError(
                "Tool-calling compatibility probe failed; select another local model/template"
            )

    def chat(self, messages, tools, cancel):
        response = self.request(
            "/api/chat",
            {
                "model": self.model,
                "messages": messages,
                "tools": tools,
                "stream": False,
                "options": {
                    "num_ctx": self.num_ctx,
                    "num_predict": 2048,
                    "temperature": 0,
                },
            },
            cancel,
        )
        if response.get("done") is not True:
            raise ProviderError("Incomplete non-streaming Ollama response")
        if response.get("done_reason") == "length":
            raise ContextLimit(
                "Ollama exhausted its generation/context budget; no partial calls executed"
            )
        return normalize_message(response.get("message"))
