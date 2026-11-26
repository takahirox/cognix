use serde_json::{Value, json};

pub(crate) const PATCH_FILE_BYTES: usize = 65536;

pub fn tools() -> Value {
    let string = json!({"type":"string"});
    let defs = [
        (
            "apply_patch",
            "Preview and approve one project-relative UTF-8 file change (64 KiB maximum, no NULs). Use old_text: null to create a new file with complete new_text in an existing directory. To update, use an exact unique old_text span and replacement new_text. An empty old_text matches only an empty existing file. No file deletion, moves, or new directories.",
            json!({"path":{"type":"string","maxLength":4096},"old_text":{"type":["string","null"],"maxLength":PATCH_FILE_BYTES},"new_text":{"type":"string","maxLength":PATCH_FILE_BYTES}}),
        ),
        (
            "run_command",
            "Inspect, search, or verify the project with an approved read-only sandboxed command. Use a bare installed command name (e.g. ls, cat, rg, or grep) or an absolute executable path, plus literal arguments. Runs from the project root with no shell expansion, network, or project writes.",
            json!({"argv":{"type":"array","items":string,"minItems":1,"maxItems":64}}),
        ),
    ];
    Value::Array(defs.into_iter().map(|(name,description,properties)| {
        let required: Vec<_> = properties.as_object().unwrap().keys().cloned().collect();
        json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})
    }).collect())
}

pub fn argv(value: &Value) -> Result<Vec<String>, String> {
    let a = value.as_array().ok_or("argv must be an array")?;
    if a.is_empty() || a.len() > 64 {
        return Err("argv must have 1–64 nonempty strings".into());
    }
    a.iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && !s.contains('\0'))
                .map(str::to_owned)
                .ok_or("argv must have nonempty strings without NULs".into())
        })
        .collect()
}

pub fn validate(name: &str, args: &Value) -> Result<(), String> {
    let tools = tools();
    let def = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["function"]["name"] == name)
        .ok_or("Unknown tool")?;
    let props = def["function"]["parameters"]["properties"]
        .as_object()
        .unwrap();
    let obj = args.as_object().ok_or("Arguments must be an object")?;
    if obj.len() != props.len() || props.keys().any(|k| !obj.contains_key(k)) {
        return Err("Arguments must contain exactly the documented fields".into());
    }
    // A model response is bounded too; avoid oversized patches and command arguments.
    if args.to_string().len() > 1024 * 1024 {
        return Err("Arguments exceed 1 MiB".into());
    }
    for (key, value) in obj {
        if key == "argv" {
            argv(value)?;
        } else if !(value.is_string() || (key == "old_text" && value.is_null())) {
            if key == "old_text" {
                return Err("old_text must be a string or null".into());
            }
            return Err(format!("{key} must be a string"));
        }
    }
    if name == "apply_patch" {
        for field in ["old_text", "new_text"] {
            if let Some(text) = args[field].as_str() {
                if text.len() > PATCH_FILE_BYTES {
                    return Err(format!("{field} exceeds 64 KiB of UTF-8 content"));
                }
                if text.contains('\0') {
                    return Err(format!("{field} must be text without NULs"));
                }
            }
        }
        if args["path"].as_str().unwrap().len() > 4096 {
            return Err("File path exceeds 4096 bytes".into());
        }
    }
    Ok(())
}
