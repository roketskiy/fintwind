use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener};
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use rusqlite::Connection;
use uuid::{Uuid, Version};

#[path = "../src/activity.rs"]
mod activity;

fn captured_report(database: &Path, status: u16) -> Result<(bool, serde_json::Value)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}/api/activity", listener.local_addr()?);
    let receiver = std::thread::spawn(move || -> Result<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    ensure!(Instant::now() < deadline, "activity request did not arrive");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        ensure!(
            line == "POST /api/activity HTTP/1.1\r\n",
            "wrong activity method or path"
        );
        let mut length = None;
        let mut json = false;
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').context("invalid request header")?;
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>()?);
            }
            if name.eq_ignore_ascii_case("content-type") {
                json = value.trim() == "application/json";
            }
        }
        ensure!(json, "activity content type is not JSON");
        let length = length.context("missing content length")?;
        ensure!(length < 1024, "activity body is unexpectedly large");
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        write!(
            reader.get_mut(),
            "HTTP/1.1 {status} Result\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        Ok(serde_json::from_slice(&body)?)
    });
    let result = activity::report_to(database, &endpoint);
    if let Err(error) = &result {
        eprintln!("activity report returned {error:#}");
    }
    let accepted = result.is_ok();
    let body = receiver
        .join()
        .map_err(|_| anyhow::anyhow!("receiver panicked"))??;
    Ok((accepted, body))
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let [flag, endpoint, database] = args.as_slice() {
        ensure!(flag == "--report-to", "expected --report-to URL DATABASE");
        let url = url::Url::parse(endpoint)?;
        let loopback = url.host_str().and_then(|host| host.parse::<IpAddr>().ok());
        ensure!(
            url.scheme() == "http"
                && loopback.is_some_and(|address| address.is_loopback())
                && url.username().is_empty()
                && url.password().is_none(),
            "integration endpoint must be a loopback HTTP URL without credentials"
        );
        let database = Path::new(database);
        ensure!(
            database.is_absolute(),
            "integration database path must be absolute"
        );
        activity::report_to(database, endpoint)?;
        println!("activity integration request accepted");
        return Ok(());
    }
    ensure!(
        args.is_empty(),
        "expected no arguments or --report-to URL DATABASE"
    );
    let directory =
        std::env::temp_dir().join(format!("fintwind-activity-blackbox-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let database = directory.join("app.db");
    let connection = Connection::open(&database)?;
    connection.execute_batch(
        "PRAGMA user_version = 73;
         CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO app_settings VALUES ('theme', 'dark');",
    )?;
    drop(connection);
    let first = activity::installation_id(&database)?;
    ensure!(
        first.get_version() == Some(Version::Random),
        "identity is not UUID v4"
    );
    ensure!(
        first == activity::installation_id(&database)?,
        "identity changed across reopen"
    );
    ensure!(
        first != activity::installation_id(&directory.join("other.db"))?,
        "independent databases share identity"
    );
    let concurrent = directory.join("concurrent.db");
    let barrier = Arc::new(Barrier::new(8));
    let workers = (0..8)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let database = concurrent.clone();
            std::thread::spawn(move || {
                barrier.wait();
                activity::installation_id(&database)
            })
        })
        .collect::<Vec<_>>();
    let identities = workers
        .into_iter()
        .map(|worker| {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("initializer panicked"))?
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        identities.iter().all(|identity| *identity == identities[0]),
        "simultaneous initialization split identity"
    );
    if cfg!(debug_assertions) {
        activity::report_startup(&database)?;
    }
    let expected = serde_json::json!({ "installation_id": first });
    let (accepted, payload) = captured_report(&database, 204)?;
    ensure!(
        accepted && payload == expected,
        "activity success or payload mismatch"
    );
    let (accepted, payload) = captured_report(&database, 503)?;
    ensure!(
        !accepted && payload == expected,
        "activity failure was not surfaced"
    );
    ensure!(
        first == activity::installation_id(&database)?,
        "HTTP failure changed identity"
    );
    let connection = Connection::open(&database)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    ensure!(version == 73, "activity changed database migration version");
    let theme: String = connection.query_row(
        "SELECT value FROM app_settings WHERE key = 'theme'",
        [],
        |row| row.get(0),
    )?;
    ensure!(theme == "dark", "activity changed application data");
    let count: i64 =
        connection.query_row("SELECT count(*) FROM installation_identity", [], |row| {
            row.get(0)
        })?;
    ensure!(count == 1, "identity table is not a singleton");
    drop(connection);
    std::fs::remove_dir_all(&directory)?;
    println!("activity blackbox passed: stable UUID v4, independent databases, concurrent initialization, exact POST, HTTP failure, preserved schema and data");
    Ok(())
}
