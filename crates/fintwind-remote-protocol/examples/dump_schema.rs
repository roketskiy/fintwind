//! Dump the remote protocol's JSON Schema bundle as JSON.
//!
//! The PWA build (and CI drift checks) consume this output; TypeScript
//! definitions are generated from it rather than written by hand.

fn main() {
    let bundle: serde_json::Map<String, serde_json::Value> = fintwind_remote_protocol::schema::exported_schemas()
        .into_iter()
        .map(|(name, schema)| (name.to_owned(), serde_json::to_value(schema).expect("schema serialization cannot fail")))
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&bundle).expect("schema serialization cannot fail")
    );
}
