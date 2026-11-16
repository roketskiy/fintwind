#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

const BROWSER_POC_FLAG: &str = "--browser-poc";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|arg| arg == BROWSER_POC_FLAG) {
        fintwind::run();
        return;
    }

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
