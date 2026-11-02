use ring::digest;
use serde_json::{Value, json};

pub(crate) fn valid_revision(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn conditional_response(mut value: Value, previous: Option<&str>) -> Value {
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return value;
    };
    let revision = digest::digest(&digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if previous == Some(revision.as_str()) {
        return json!({"notModified": true, "revision": revision});
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("revision".into(), Value::String(revision));
    }
    value
}

/// Deltas are bounded by the already bounded desktop projection. Strings grow
/// by suffix when possible; array rows keep their indices and unchanged values.
pub(crate) fn delta(previous: &Value, next: &Value) -> Vec<Value> {
    fn walk(previous: &Value, next: &Value, path: &mut Vec<String>, edits: &mut Vec<Value>) {
        if previous == next {
            return;
        }
        match (previous, next) {
            (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => {
                for (key, value) in b {
                    path.push(key.clone());
                    walk(&a[key], value, path, edits);
                    path.pop();
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                for (index, value) in b.iter().enumerate() {
                    path.push(index.to_string());
                    if let Some(old) = a.get(index) {
                        walk(old, value, path, edits);
                    } else {
                        edits.push(json!({"path": path, "set": value}));
                    }
                    path.pop();
                }
                if b.len() < a.len() {
                    edits.push(json!({"path": path, "truncate": b.len()}));
                }
            }
            (Value::String(a), Value::String(b))
                if path.last().is_some_and(|key| key == "text") && b.starts_with(a.as_str()) =>
            {
                edits.push(json!({"path": path, "append": &b[a.len()..]}));
            }
            _ => edits.push(json!({"path": path, "set": next})),
        }
    }
    let mut edits = Vec::new();
    walk(previous, next, &mut Vec::new(), &mut edits);
    edits
}
