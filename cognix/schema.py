"""Small, closed tool schemas shared by the model and runtime."""


def tool(name, description, properties):
    return {
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": list(properties),
                "additionalProperties": False,
            },
        },
    }


STRING = {"type": "string"}
TOOLS = [
    tool(
        "list_files",
        "List one project directory (no symlinks). Use '.' for root.",
        {"path": STRING},
    ),
    tool("read_file", "Read a bounded UTF-8 file and its SHA-256.", {"path": STRING}),
    tool(
        "apply_patch",
        "Replace exactly one nonempty text span in an existing, previously read file. "
        "Include its read_file sha256. A diff is shown for user approval. No new files/deletions.",
        {"path": STRING, "sha256": STRING, "old_text": STRING, "new_text": STRING},
    ),
    tool(
        "run_command",
        "Run an exact allowed verification argv, after approval, in a read-only sandbox.",
        {"argv": {"type": "array", "items": STRING, "minItems": 1}},
    ),
]
SCHEMAS = {t["function"]["name"]: t["function"]["parameters"] for t in TOOLS}


def validate(name, arguments):
    if not isinstance(name, str) or name not in SCHEMAS:
        raise ValueError("Unknown tool")
    if (
        not isinstance(arguments, dict)
        or arguments.keys() != SCHEMAS[name]["properties"].keys()
    ):
        raise ValueError("Arguments must contain exactly the documented fields")
    for key, schema in SCHEMAS[name]["properties"].items():
        value = arguments[key]
        if schema["type"] == "string" and not isinstance(value, str):
            raise ValueError(f"{key} must be a string")
        if schema["type"] == "array" and (
            not isinstance(value, list)
            or not value
            or len(value) > 64
            or any(not isinstance(x, str) or not x or "\0" in x for x in value)
        ):
            raise ValueError(
                f"{key} must be a nonempty array of at most 64 nonempty strings"
            )
