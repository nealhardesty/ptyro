//! Typed step outputs (`output = { schema = … }`): the agent writes JSON to `$PTYRO_STEP_OUT`
//! and the engine checks it. An invalid answer gets one repair prompt before the step fails.

use serde_json::Value;

/// What the agent is told to write, appended to the step's prompt.
pub fn instructions(schema: &str, path: &str) -> String {
    let shape = match schema {
        "builtin:verdict" => {
            r#"{"decision": "approve" | "changes", "issues": ["one line per issue", …], "summary": "…"}"#.to_owned()
        }
        "builtin:subtasks" => {
            r#"{"subtasks": [{"title": "…", "prompt": "what a worker should do"}, …]}"#.to_owned()
        }
        other => other.to_owned(),
    };
    format!(
        "\n\nWhen you are done, write your answer as JSON to the file {path} (and nothing else to that file), shaped like:\n{shape}"
    )
}

/// Check `v` against `schema`: a builtin, or inline JSON `{"required": ["key", …]}`.
pub fn check(schema: &str, v: &Value) -> Result<(), String> {
    let obj = v.as_object().ok_or("the answer is not a JSON object")?;
    match schema {
        "builtin:verdict" => {
            match obj.get("decision").and_then(Value::as_str) {
                Some("approve" | "changes") => {}
                Some(d) => {
                    return Err(format!("decision is {d:?}; use \"approve\" or \"changes\""));
                }
                None => return Err("missing \"decision\"".into()),
            }
            if let Some(i) = obj.get("issues")
                && !i.is_array()
            {
                return Err("\"issues\" must be a list".into());
            }
            Ok(())
        }
        "builtin:subtasks" => {
            let list =
                obj.get("subtasks").and_then(Value::as_array).ok_or("missing \"subtasks\" list")?;
            for (i, t) in list.iter().enumerate() {
                if t.get("prompt").and_then(Value::as_str).is_none_or(|p| p.trim().is_empty()) {
                    return Err(format!("subtask {} has no \"prompt\"", i + 1));
                }
            }
            Ok(())
        }
        inline => {
            let s: Value = serde_json::from_str(inline).map_err(|e| format!("schema: {e}"))?;
            for k in s.get("required").and_then(Value::as_array).into_iter().flatten() {
                if let Some(k) = k.as_str()
                    && !obj.contains_key(k)
                {
                    return Err(format!("missing {k:?}"));
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn outputs_are_checked() {
        assert!(check("builtin:verdict", &json!({"decision": "approve"})).is_ok());
        assert!(
            check("builtin:verdict", &json!({"decision": "maybe"})).unwrap_err().contains("maybe")
        );
        assert!(check("builtin:verdict", &json!([1])).is_err());
        assert!(
            check("builtin:subtasks", &json!({"subtasks": [{"prompt": "x"}, {"title": "y"}]}))
                .unwrap_err()
                .contains("subtask 2")
        );
        assert!(check(r#"{"required": ["score"]}"#, &json!({"score": 3})).is_ok());
        assert!(check(r#"{"required": ["score"]}"#, &json!({})).is_err());
        assert!(instructions("builtin:verdict", "/x/out.json").contains("/x/out.json"));
    }
}
