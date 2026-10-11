//! JSON Schema export for TypeScript generation.
//!
//! The PWA consumes these definitions instead of hand-written types; CI
//! dumps and diffs this output so the two ends cannot drift. Run
//! `cargo run -p fintwind-remote-protocol --example dump_schema` to
//! print the current bundle.

use schemars::JsonSchema;
use schemars::schema::RootSchema;

use crate::{
    ControlFrame, Frame, MessageBody, OperationOutcome, OperationRequest, PermissionDecision,
    PermissionOffer, ResumeCursor, SealedMessage, SessionSummary, UserInputDecision,
    UserInputOffer,
};

/// Schemars 0.8 does not apply serde's `rename_all_fields` to fields of enum
/// struct variants. Our wire contract uses camelCase throughout, but without
/// this correction a generated schema says `operation_id` while serde accepts
/// only `operationId`. Normalize schema property/required names, not arbitrary
/// JSON payload maps or discriminator values. This is an export workaround;
/// serde remains the authoritative runtime parser.
pub fn wire_schema<T: JsonSchema>() -> RootSchema {
    fn camel_case(name: &str) -> String {
        let mut result = String::with_capacity(name.len());
        let mut uppercase = false;
        for character in name.chars() {
            if character == '_' {
                uppercase = true;
            } else if uppercase {
                result.extend(character.to_uppercase());
                uppercase = false;
            } else {
                result.push(character);
            }
        }
        result
    }
    fn normalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(serde_json::Value::Object(properties)) = object.get_mut("properties") {
                    let previous = std::mem::take(properties);
                    for (name, property) in previous {
                        properties.insert(camel_case(&name), property);
                    }
                }
                if let Some(serde_json::Value::Array(required)) = object.get_mut("required") {
                    for name in required {
                        if let Some(text) = name.as_str() {
                            *name = serde_json::Value::String(camel_case(text));
                        }
                    }
                }
                for child in object.values_mut() {
                    normalize(child);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    normalize(child);
                }
            }
            _ => {}
        }
    }
    let mut value =
        serde_json::to_value(schemars::schema_for!(T)).expect("generated schema serializes");
    normalize(&mut value);
    serde_json::from_value(value).expect("property normalization preserves schema structure")
}

/// Every exported root schema, keyed by type name. Keep entries sorted;
/// the dump is diffed in CI.
pub fn exported_schemas() -> Vec<(&'static str, RootSchema)> {
    fn root<T: JsonSchema>() -> RootSchema {
        wire_schema::<T>()
    }
    vec![
        ("Frame", root::<Frame>()),
        ("ControlFrame", root::<ControlFrame>()),
        ("SealedMessage", root::<SealedMessage>()),
        ("MessageBody", root::<MessageBody>()),
        ("ResumeCursor", root::<ResumeCursor>()),
        ("SessionSummary", root::<SessionSummary>()),
        ("PermissionOffer", root::<PermissionOffer>()),
        ("PermissionDecision", root::<PermissionDecision>()),
        ("UserInputOffer", root::<UserInputOffer>()),
        ("UserInputDecision", root::<UserInputDecision>()),
        ("OperationRequest", root::<OperationRequest>()),
        ("OperationOutcome", root::<OperationOutcome>()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The PWA consumes this bundle; a silently empty or misnamed export
    /// would starve its TypeScript generation.
    #[test]
    fn every_export_has_a_unique_name_and_a_title() {
        let mut names: Vec<&str> = exported_schemas().iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "exported schema names must be unique");
        for (name, schema) in exported_schemas() {
            assert_eq!(
                schema
                    .schema
                    .metadata
                    .as_ref()
                    .and_then(|m| m.title.clone()),
                Some(name.to_owned()),
                "schema title must match its export name: {name}"
            );
        }
    }
}
