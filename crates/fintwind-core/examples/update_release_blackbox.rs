//! Run with `cargo run --locked -p fintwind-core --example update_release_blackbox`.
//! Exercises the application's update discovery over a loopback release API.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[derive(PartialEq)]
enum BuildChannel {
    Stable,
}
const BUILD_CHANNEL: BuildChannel = BuildChannel::Stable;

#[allow(dead_code)]
#[path = "../../../src/update.rs"]
mod update;

fn release(id: u64, tag: &str, published: &str) -> Value {
    let version = tag.trim_start_matches(['v', 'V']);
    json!({
        "id": id, "tag_name": tag, "published_at": published,
        "created_at": "2020-01-01T00:00:00Z", "draft": false, "prerelease": false,
        "body": "Publication order update", "html_url": format!("https://example.invalid/releases/tag/{tag}"),
        "assets": [{
            "name": format!("fintwind-{version}-{}-Setup.exe", std::env::consts::ARCH),
            "browser_download_url": "https://example.invalid/installer.exe", "size": 1234,
            "digest": format!("sha256:{}", "a".repeat(64))
        }]
    })
}

fn check(
    name: &str,
    current: &str,
    pages: Vec<(u16, Value)>,
    expected: Option<&str>,
) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}/releases", listener.local_addr()?);
    let server = std::thread::spawn(move || -> Result<()> {
        for (index, (status, page)) in pages.into_iter().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(5);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        ensure!(
                            Instant::now() < deadline,
                            "missing release page {}",
                            index + 1
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            stream.set_write_timeout(Some(Duration::from_secs(3)))?;
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line)?;
            ensure!(
                line == format!("GET /releases?per_page=100&page={} HTTP/1.1\r\n", index + 1),
                "unexpected request: {line}"
            );
            loop {
                line.clear();
                ensure!(reader.read_line(&mut line)? > 0, "truncated request");
                if line == "\r\n" {
                    break;
                }
            }
            let body = serde_json::to_vec(&page)?;
            write!(
                reader.get_mut(),
                "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            reader.get_mut().write_all(&body)?;
        }
        Ok(())
    });
    let result = update::fetch_newer_release_from(&endpoint, current);
    server.join().expect("release fixture panicked")?;
    ensure!(
        result.as_ref().map(|release| release.version.as_str()) == expected,
        "{name}: expected {expected:?}, got {result:?}"
    );
    if let Some(release) = result {
        ensure!(
            release.notes == "Publication order update",
            "release notes lost"
        );
        ensure!(
            release.url.ends_with(&format!("/v{}", release.version)),
            "wrong release URL"
        );
        let installer = release
            .installer()
            .expect("installer for the selected release");
        ensure!(
            installer.size == 1234 && installer.sha256.as_deref() == Some("a".repeat(64).as_str()),
            "installer metadata lost"
        );
    }
    println!("PASS {name}");
    Ok(())
}

fn gateway(version: &str) -> Value {
    json!({
        "version": version,
        "notes": "Publication order update",
        "url": format!("https://example.invalid/releases/tag/v{version}"),
        "assets": [{
            "name": format!("fintwind-{version}-{}-Setup.exe", std::env::consts::ARCH),
            "browser_download_url": "https://example.invalid/installer.exe",
            "size": 1234,
            "sha256": "a".repeat(64)
        }]
    })
}

fn check_gateway(
    name: &str,
    current: &str,
    status: u16,
    body: Value,
    expected: Option<&str>,
) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}/v1/latest", listener.local_addr()?);
    let current = current.to_owned();
    let expected_request = format!("GET /v1/latest?current={current} HTTP/1.1\r\n");
    let server = std::thread::spawn(move || -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    ensure!(Instant::now() < deadline, "missing gateway request");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error.into()),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        ensure!(
            line == expected_request,
            "unexpected request: {line}"
        );
        loop {
            line.clear();
            ensure!(reader.read_line(&mut line)? > 0, "truncated request");
            if line == "\r\n" {
                break;
            }
        }
        let body = serde_json::to_vec(&body)?;
        write!(
            reader.get_mut(),
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        reader.get_mut().write_all(&body)?;
        Ok(())
    });
    let result = update::fetch_gateway_release_from(&endpoint, &current);
    server.join().expect("gateway fixture panicked")?;
    ensure!(
        result.as_ref().map(|release| release.version.as_str()) == expected,
        "{name}: expected {expected:?}, got {result:?}"
    );
    if let Some(release) = result {
        let installer = release
            .installer()
            .expect("installer for the selected release");
        ensure!(installer.size == 1234, "installer metadata lost");
    }
    println!("PASS {name}");
    Ok(())
}

fn gateway_checks() -> Result<()> {
    check_gateway(
        "gateway offers the newer release",
        "0.2.13",
        200,
        gateway("0.2.14"),
        Some("0.2.14"),
    )?;
    check_gateway(
        "gateway 204 means the installed release is current",
        "0.2.14",
        204,
        json!(null),
        None,
    )?;
    check_gateway(
        "gateway echo of the installed version is not offered",
        "0.2.13",
        200,
        gateway("0.2.13"),
        None,
    )?;
    check_gateway(
        "gateway failure offers nothing",
        "0.2.13",
        502,
        json!({"error": "unavailable"}),
        None,
    )?;
    check_gateway(
        "gateway garbage offers nothing",
        "0.2.13",
        200,
        json!({"unexpected": true}),
        None,
    )?;
    Ok(())
}

fn main() -> Result<()> {
    gateway_checks()?;
    let old = release(10, "v2.0.13", "2026-10-21T12:00:00Z");
    let corrected = release(11, "v0.2.14", "2026-10-22T12:00:00Z");
    let mut draft = release(12, "v9.0.0", "2026-10-23T12:00:00Z");
    draft["draft"] = json!(true);
    let mut beta = release(13, "v10.0.0-beta", "2026-10-24T12:00:00Z");
    beta["prerelease"] = json!(true);
    let feed = json!([draft, old, corrected, beta]);
    check(
        "a later lower-numbered release is offered",
        "2.0.13",
        vec![(200, feed.clone())],
        Some("0.2.14"),
    )?;
    check(
        "the installed latest release is not offered again",
        "V0.2.14",
        vec![(200, feed)],
        None,
    )?;
    check(
        "an older higher-numbered release does not win",
        "0.2.14",
        vec![(
            200,
            json!([
                release(50, "v99.0.0", "2026-10-20T12:00:00Z"),
                corrected.clone()
            ]),
        )],
        None,
    )?;
    let first_page = vec![old.clone(); 100];
    check(
        "publication order spans all pages",
        "2.0.13",
        vec![(200, json!(first_page)), (200, json!([corrected]))],
        Some("0.2.14"),
    )?;
    check(
        "a failed later page does not offer a partial result",
        "0.2.14",
        vec![
            (200, json!(vec![old; 100])),
            (503, json!({"error":"unavailable"})),
        ],
        None,
    )?;
    check(
        "an empty feed offers no update",
        "2.0.13",
        vec![(200, json!([]))],
        None,
    )?;
    check(
        "malformed metadata offers no update",
        "2.0.13",
        vec![(200, json!({"unexpected":true}))],
        None,
    )?;
    println!("Update release black-box checks passed.");
    Ok(())
}
