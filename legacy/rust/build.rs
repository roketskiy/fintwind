//! Windows executable resources.
//!
//! Explorer, the taskbar, and the Programs list all read the icon and version
//! block out of the PE image itself — there is no bundle or desktop entry to
//! carry them.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(target_os = "windows")]
    {
        // GPUI's Taffy layout and text shaping recurse deeply enough to
        // overflow the 1 MiB the MSVC linker defaults to.
        println!("cargo:rustc-link-arg-bins=/stack:{}", 8 * 1024 * 1024);
        embed_windows_resources();
    }
}

#[cfg(target_os = "windows")]
fn embed_windows_resources() {
    const ICON: &str = "resources/windows/AppIcon.ico";

    println!("cargo:rerun-if-changed={ICON}");

    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ICON);
    // The resource compiler reads `.rc` as C source, so a Windows path
    // separator has to survive as a literal backslash.
    let icon = icon.to_string_lossy().replace('\\', "\\\\");

    let package_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // VERSIONINFO wants four numeric fields; fintwind's version has three.
    let mut fields = package_version
        .split(['.', '-', '+'])
        .map(|field| field.parse::<u16>().unwrap_or(0))
        .chain(std::iter::repeat(0));
    let file_version = format!(
        "{},{},{},{}",
        fields.next().unwrap_or(0),
        fields.next().unwrap_or(0),
        fields.next().unwrap_or(0),
        fields.next().unwrap_or(0),
    );
    let description = std::env::var("CARGO_PKG_DESCRIPTION").unwrap_or_default();

    // Match src/lib.rs's APP_DISPLAY_NAME: a debug build is the Dev channel,
    // a release build with the `beta` feature is Beta, anything else is
    // stable. Cargo hands both facts to build scripts as environment
    // variables, so this reads them instead of cfg! (which here would
    // describe the build script's own compilation, not the crate's).
    let channel_suffix = if std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some() {
        " Dev"
    } else if std::env::var_os("CARGO_FEATURE_BETA").is_some() {
        " Beta"
    } else {
        ""
    };
    let product = format!("fintwind{channel_suffix}");

    let resources = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {file_version}
PRODUCTVERSION {file_version}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "fintwind\0"
            VALUE "FileDescription", "{description} ({product})\0"
            VALUE "FileVersion", "{package_version}\0"
            VALUE "InternalName", "fintwind\0"
            VALUE "OriginalFilename", "fintwind.exe\0"
            VALUE "ProductName", "{product}\0"
            VALUE "ProductVersion", "{package_version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    );

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let script = out_dir.join("fintwind.rc");
    std::fs::write(&script, resources).expect("write the resource script");

    // GPUI embeds the application manifest through its own resource script,
    // so this one only claims the icon and version block.
    embed_resource::compile(&script, embed_resource::NONE)
        .manifest_optional()
        .expect("compile Windows resources");
}
