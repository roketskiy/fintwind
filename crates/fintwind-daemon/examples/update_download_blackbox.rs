//! Real curl downloads against a loopback fixture; never executes an installer.
//! Run through scripts/update-download-e2e.py for an isolated Windows Job Object.

use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(PartialEq)]
enum BuildChannel {
    Stable,
}
const BUILD_CHANNEL: BuildChannel = BuildChannel::Stable;

#[allow(dead_code)]
#[path = "../../../src/update.rs"]
mod update;

fn download(
    root: &Path,
    name: &str,
    redirect: bool,
    known_size: bool,
    truncated: bool,
    rejected: bool,
) -> Result<()> {
    let payload = vec![0x5a; 512 * 1024];
    let destination = root.join(format!("{name}.exe"));
    let partial = root.join(format!("{name}.exe.part"));
    // A prior attempt must not appear as this attempt's progress.
    std::fs::write(&partial, vec![0x01; payload.len()])?;
    if truncated || rejected {
        std::fs::write(&destination, b"previous verified download")?;
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let url = format!("http://{}/installer", listener.local_addr()?);
    let source = update::DownloadSource {
        url: url.clone(),
        mirror: None,
    };
    let asset = update::ReleaseAsset {
        name: format!("{name}.exe"),
        url,
        size: if known_size { payload.len() as u64 } else { 0 },
        sha256: None,
    };
    let sent_payload = payload.clone();
    let server = std::thread::spawn(move || -> Result<()> {
        for request in 0..if redirect { 2 } else { 1 } {
            let deadline = Instant::now() + Duration::from_secs(5);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        ensure!(Instant::now() < deadline, "download request missing");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            stream.set_write_timeout(Some(Duration::from_secs(3)))?;
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line)?;
            ensure!(line.starts_with(if request == 1 {
                "GET /payload "
            } else {
                "GET /installer "
            }));
            loop {
                line.clear();
                ensure!(stream.read_line(&mut line)? > 0);
                if line == "\r\n" {
                    break;
                }
            }
            let stream = stream.get_mut();
            if redirect && request == 0 {
                write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: /payload\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )?;
                continue;
            }
            if rejected {
                write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )?;
                continue;
            }
            write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\n")?;
            if known_size {
                write!(stream, "Content-Length: {}\r\n", sent_payload.len())?;
            }
            write!(stream, "\r\n")?;
            let length = if truncated {
                sent_payload.len() / 2
            } else {
                sent_payload.len()
            };
            for chunk in sent_payload[..length].chunks(64 * 1024) {
                stream.write_all(chunk)?;
                stream.flush()?;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    });
    let mut samples = Vec::new();
    let result =
        update::download_installer(&asset, &source, &destination, |bytes| samples.push(bytes));
    server.join().expect("fixture panicked")?;
    ensure!(
        samples.first() == Some(&0),
        "stale partial reported as progress"
    );
    ensure!(
        samples.windows(2).all(|pair| pair[0] <= pair[1]),
        "progress went backwards"
    );
    ensure!(!partial.exists(), "partial file was not retired");
    if truncated || rejected {
        ensure!(result.is_err(), "bad response accepted");
        ensure!(std::fs::read(&destination)? == b"previous verified download");
        ensure!(
            samples.last().copied().unwrap_or(0) < payload.len() as u64,
            "failed download reported complete"
        );
    } else {
        result?;
        ensure!(
            samples.last() == Some(&(payload.len() as u64)),
            "final byte count missing"
        );
        ensure!(
            samples
                .iter()
                .any(|bytes| *bytes > 0 && *bytes < payload.len() as u64),
            "no progress before completion"
        );
        ensure!(std::fs::read(&destination)? == payload);
    }
    Ok(())
}

fn main() -> Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("pass an isolated output directory")?,
    );
    std::fs::create_dir_all(&root)?;
    let result = (|| -> Result<Vec<&str>> {
        let mut checks = Vec::new();
        for (name, redirect, known, truncated, rejected) in [
            ("redirected-download", true, true, false, false),
            ("unknown-total", false, false, false, false),
            ("truncated-download", false, true, true, false),
            ("http-error", false, true, false, true),
        ] {
            download(&root, name, redirect, known, truncated, rejected)?;
            checks.push(name);
        }
        Ok(checks)
    })();
    let report = match &result {
        Ok(checks) => {
            json!({"finished":true,"passed":true,"checks":checks,"installer_executed":false})
        }
        Err(error) => json!({"finished":true,"passed":false,"error":format!("{error:#}")}),
    };
    std::fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    result?;
    Ok(())
}
