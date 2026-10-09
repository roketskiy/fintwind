use std::{env, fs, path::PathBuf};

fn main() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../db/migrations");
    println!("cargo:rerun-if-changed={}", directory.display());
    let mut paths = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .collect::<Vec<_>>();
    paths.sort();
    let mut source = String::from("pub static MIGRATIONS: &[(&str, &str)] = &[\n");
    for path in paths {
        source.push_str(&format!(
            "({:?}, include_str!({:?})),\n",
            path.file_stem().unwrap().to_str().unwrap(),
            path.canonicalize().unwrap()
        ));
    }
    source.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("migrations.rs"),
        source,
    )
    .unwrap();
}
