use sha2::{Digest, Sha256};

// Test-only SourceKey parity fixture for the Go usagecheck E2E. Mirrors
// crates/fintwind-core/src/usage_index.rs `source_key` byte-for-byte:
// canonicalize the binary (verbatim paths on Windows), then for each of the
// six provider-storage env names append NUL + name + value. Values arrive as
// positional args so the Go E2E can fix them without touching real env.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    const NAMES: [&str; 6] = [
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
    ];
    if args.len() != 8 {
        eprintln!(
            "usage: sourcekey <binary> <XDG_DATA_HOME> <XDG_CONFIG_HOME> <HOME> <USERPROFILE> <APPDATA> <LOCALAPPDATA>"
        );
        std::process::exit(2);
    }
    let binary = std::path::Path::new(&args[1]);
    let mut digest = Sha256::new();
    digest.update(
        binary
            .canonicalize()
            .unwrap_or_else(|_| binary.to_owned())
            .to_string_lossy()
            .as_bytes(),
    );
    for (name, value) in NAMES.iter().zip(args[2..].iter()) {
        digest.update([0u8]);
        digest.update(name.as_bytes());
        digest.update(value.as_bytes());
    }
    println!("{:x}", digest.finalize());
    Ok(())
}
