//! Black-box check for the process-wide HTTP proxy. A fake proxy listens on
//! an ephemeral local port, `http_get` is pointed at it through
//! [`fintwind_protocol::http::set_active_proxy`], and the run passes only when
//! the request line visibly arrived through the proxy and the proxy's answer
//! came back as the fetch result. Run with:
//!
//! ```sh
//! cargo run -p fintwind-protocol --example proxy_fetch
//! ```

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use fintwind_protocol::http::{active_proxy, http_get, normalize_proxy_url, set_active_proxy};

fn main() -> anyhow::Result<()> {
    check_normalization();

    // The proxy answers every connection with a fixed 200 and reports the
    // request line it saw, which is the whole point: only a request that rode
    // through this port can carry an absolute-URI target of a host that does
    // not resolve.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let proxy_port = listener.local_addr()?.port();
    let proxy = thread::spawn(move || -> std::io::Result<String> {
        let (stream, _) = listener.accept()?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            if line.trim().is_empty() {
                break;
            }
        }
        let mut stream = stream;
        stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nPONG!")?;
        stream.flush()?;
        Ok(request_line)
    });

    // A bare host:port is accepted for the settings field's sake and
    // normalized on the way in.
    set_active_proxy(Some(format!("127.0.0.1:{proxy_port}")));
    assert_eq!(
        active_proxy().as_deref(),
        Some(format!("http://127.0.0.1:{proxy_port}").as_str())
    );

    let (status, body) = http_get("http://proxy-target.invalid/ping", &[], 10)?;
    let seen = proxy.join().unwrap()?;
    assert!(
        seen.starts_with("GET http://proxy-target.invalid/ping"),
        "the proxy saw {seen:?} instead of the absolute-URI request"
    );
    assert_eq!(status, 200, "the proxy's status did not come back");
    assert_eq!(body, "PONG!", "the proxy's body did not come back");

    set_active_proxy(None);
    assert_eq!(active_proxy(), None);

    println!("[OK] the request rode through 127.0.0.1:{proxy_port} and the answer returned");
    Ok(())
}

/// Every branch of the settings field's validation, including the forms the
/// General page is expected to receive from a hand-edited settings file.
fn check_normalization() {
    let cases: &[(&str, Option<&str>)] = &[
        ("", None),
        ("   ", None),
        ("127.0.0.1:7897", Some("http://127.0.0.1:7897")),
        (" http://127.0.0.1:7897 ", Some("http://127.0.0.1:7897")),
        ("https://proxy.local:8443", Some("https://proxy.local:8443")),
        ("127.0.0.1", None),
        ("127.0.0.1:port", None),
        ("127.0.0.1:", None),
        ("ftp://proxy.local:21", None),
        ("bad host:7897", None),
        ("\"quoted\":7897", None),
        ("http://proxy.local/path", None),
    ];
    for (input, expected) in cases {
        let actual = normalize_proxy_url(input);
        assert_eq!(
            actual.as_deref(),
            *expected,
            "normalizing {input:?} did not produce {expected:?}"
        );
    }
}
