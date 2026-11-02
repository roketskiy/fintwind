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
