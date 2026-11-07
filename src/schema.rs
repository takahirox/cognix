use serde_json::{Value, json};

pub fn tools() -> Value {
    let string = json!({"type":"string"});
    let defs = [
        (
            "list_files",
            "List one project directory without symlinks; '.' is root.",
            json!({"path":string}),
        ),
        (
            "read_file",
            "Read a bounded UTF-8 file and its SHA-256.",
            json!({"path":string}),
        ),
        (
            "apply_patch",
            "Replace exactly one unique nonempty span in a previously read existing file, using its sha256. User approves a diff; no new files or deletions.",
            json!({"path":string,"sha256":string,"old_text":string,"new_text":string}),
        ),
        (
            "run_command",
            "Run an exact allowed verification argv after approval, in a read-only sandbox.",
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
        } else if !value.is_string() {
            return Err(format!("{key} must be a string"));
        }
    }
    Ok(())
}
