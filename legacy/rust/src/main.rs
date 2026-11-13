// A GUI build must not open a console window behind the app. Debug builds
// keep the console subsystem so `cargo run` and the dev watcher still see
// stdout and panics.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

/// The one flag that routes into the isolated browser PoC host instead of the
/// application. It must be explicit: nothing about the PoC's other flags can
/// imply it, and a build without the `browser-poc` feature rejects it rather
/// than silently starting the normal app.
const BROWSER_POC_FLAG: &str = "--browser-poc";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|arg| arg == BROWSER_POC_FLAG) {
        // No PoC flag: the ordinary application, daemon and all. The PoC
        // flags mean nothing to it.
        fintwind::run();
        return;
    }

    // The flag carries the PoC, and only the PoC; hand it everything else.
    let poc_args: Vec<String> = args
        .into_iter()
        .filter(|arg| arg != BROWSER_POC_FLAG)
        .collect();

    #[cfg(all(target_os = "windows", feature = "browser-poc"))]
    {
        if let Err(error) = fintwind::try_run_browser_poc(&poc_args) {
            eprintln!("browser-poc failed: {error:#}");
            std::process::exit(1);
        }
    }

    // A build without the feature must not fall through to the application:
    // starting the daemon for a flag it does not support is worse than the
    // hard stop here.
    #[cfg(not(all(target_os = "windows", feature = "browser-poc")))]
    {
        let _ = poc_args;
        eprintln!(
            "error: {BROWSER_POC_FLAG} is not supported by this build; \
             rebuild with --features browser-poc"
        );
        std::process::exit(2);
    }
}
