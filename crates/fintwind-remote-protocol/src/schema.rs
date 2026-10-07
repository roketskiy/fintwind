//! JSON Schema export for TypeScript generation.
//!
//! The PWA consumes these definitions instead of hand-written types; CI
//! dumps and diffs this output so the two ends cannot drift. Run
//! `cargo run -p fintwind-remote-protocol --example dump_schema` to
//! print the current bundle.

use schemars::schema::RootSchema;
use schemars::JsonSchema;

use crate::{
    ControlFrame, Frame, MessageBody, OperationOutcome, OperationRequest, PermissionDecision,
    PermissionOffer, ResumeCursor, SealedMessage, SessionSummary, UserInputDecision,
    UserInputOffer,
};

/// Every exported root schema, keyed by type name. Keep entries sorted;
/// the dump is diffed in CI.
pub fn exported_schemas() -> Vec<(&'static str, RootSchema)> {
    fn root<T: JsonSchema>() -> RootSchema {
        schemars::schema_for!(T)
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
        let mut names: Vec<&str> = exported_schemas()
            .iter()
            .map(|(name, _)| *name)
            .collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "exported schema names must be unique");
        for (name, schema) in exported_schemas() {
            assert_eq!(schema.schema.metadata.as_ref().and_then(|m| m.title.clone()), Some(name.to_owned()), "schema title must match its export name: {name}");
        }
    }
}
