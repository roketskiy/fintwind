//! Pool of resident `opencode serve` processes, one per opencode binary.
//!
//! Fintwind runs its own private global server per binary instead of joining
//! the user-level `opencode service`. Every session that uses the same
//! binary shares one long-lived process regardless of workspace; a session's
//! workspace is addressed by the directory it carries on each API request,
//! so the process itself always runs in a stable data directory rather than
//! in any user workspace (see [`serve_working_directory`]).
//!
//! The pool's slot keeps a strong reference to the running server, so
//! dropping the last session handle leaves the process resident for the next
//! session. A pooled server is torn down only by [`shutdown_all`], which the
//! daemon calls at exit after dropping its sessions — while the driver's SSE
//! and permission threads are still connected. Dedicated servers and
//! superseded dead generations still die with their last handle.

use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context;

use crate::opencode_session::OpenCodeServer;

/// How long a teardown waits for the server to actually exit, so a session
/// starting right after it cannot adopt a dying process.
const SERVER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// A reference to a server process, shared or dedicated.
///
/// Clones add handles. Dropping the last handle kills a dedicated or
/// superseded server; a pooled global server outlives its handles until
/// [`shutdown_all`].
#[derive(Clone)]
pub(crate) struct PooledServer {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    server: OpenCodeServer,
}

impl Drop for PoolInner {
    fn drop(&mut self) {
        // Reached only by dedicated servers and generations that lost their
        // slot to a replacement. The global slot holds a strong reference,
        // and `shutdown_all` shuts its server down before releasing it, so a
        // session handle dropping can never stop the resident process.
        self.server.shutdown(SERVER_EXIT_TIMEOUT);
    }
}

impl Deref for PooledServer {
    type Target = OpenCodeServer;

    fn deref(&self) -> &Self::Target {
        &self.inner.server
    }
}

impl PooledServer {
    /// Wraps a server a session started for itself, outside the global pool.
    #[cfg(test)]
    pub(crate) fn dedicated(server: OpenCodeServer) -> Self {
        Self {
            inner: Arc::new(PoolInner { server }),
        }
    }
}

type PoolKey = PathBuf; // binary path

enum PoolState {
    Vacant,
    Starting,
    Running(Arc<PoolInner>),
    Stopping,
}

struct PoolSlot {
    state: Mutex<PoolState>,
    changed: Condvar,
}

impl Default for PoolSlot {
    fn default() -> Self {
        Self {
            state: Mutex::new(PoolState::Vacant),
            changed: Condvar::new(),
        }
    }
}

struct Pool {
    /// Set once `shutdown_all` ran. Every later `acquire` must refuse to
    /// start a process nobody would ever shut down again.
    closed: bool,
    slots: HashMap<PoolKey, Arc<PoolSlot>>,
}

fn pool() -> &'static Mutex<Pool> {
    static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();
    POOL.get_or_init(|| {
        Mutex::new(Pool {
            closed: false,
            slots: HashMap::new(),
        })
    })
}

/// Returns the binary's resident server, starting one if none is alive.
///
/// The workspace directory is per-request location data carried by callers
/// on the API itself and does not select the process. Blocking (process
/// start plus health probe), so callers must already be off the UI thread —
/// driver start on the background executor is.
pub(crate) fn acquire(binary: &Path, cwd: &Path) -> anyhow::Result<PooledServer> {
    let _ = cwd;
    acquire_with_start(binary, || -> anyhow::Result<OpenCodeServer> {
        let directory = serve_working_directory()?;
        OpenCodeServer::start(binary, &directory)
    })
}

fn acquire_with_start(
    binary: &Path,
    start: impl FnOnce() -> anyhow::Result<OpenCodeServer>,
) -> anyhow::Result<PooledServer> {
    let key = binary.to_path_buf();
    // Closed check and slot insert share one lock hold: a `shutdown_all`
    // racing this must either see the slot (and wait it out) or have the
    // acquire refuse — never a slot started after the drain.
    let slot = {
        let mut pool = pool().lock().unwrap();
        if pool.closed {
            anyhow::bail!(
                "the opencode server pool is shut down; refusing to start {key:?}"
            );
        }
        Arc::clone(pool.slots.entry(key).or_default())
    };

    let superseded = loop {
        let mut state = slot.state.lock().unwrap();
        match &*state {
            PoolState::Running(current) => {
                if current.server.is_alive() {
                    return Ok(PooledServer {
                        inner: Arc::clone(current),
                    });
                }

                // The process exited while stale session handles may still
                // exist. Take the strong reference out of the slot before
                // starting the replacement, so the stale generation's final
                // drop shuts down only the dead process and can never reach
                // the replacement stored in the slot.
                let dead = Arc::clone(current);
                *state = PoolState::Starting;
                break Some(dead);
            }
            PoolState::Vacant => {
                *state = PoolState::Starting;
                break None;
            }
            PoolState::Starting | PoolState::Stopping => {
                state = slot.changed.wait(state).unwrap();
            }
        }
    };

    // Dropping the old generation can run its destructor, which shuts the
    // dead process down. Do that outside the slot lock so a concurrent
    // acquire never waits on a destructor.
    drop(superseded);

    let started = start();
    let mut state = slot.state.lock().unwrap();
    match started {
        Ok(server) => {
            let inner = Arc::new(PoolInner { server });
            *state = PoolState::Running(Arc::clone(&inner));
            slot.changed.notify_all();
            Ok(PooledServer { inner })
        }
        Err(error) => {
            *state = PoolState::Vacant;
            slot.changed.notify_all();
            Err(error)
        }
    }
}

/// Shuts down every pooled global server. Called by the daemon at exit, and
/// safe to call repeatedly: without live servers it is a no-op.
///
/// Waits out in-flight starts, so a server that begins starting before the
/// call cannot outlive it.
pub(crate) fn shutdown_all() {
    let slots: Vec<Arc<PoolSlot>> = {
        let mut pool = pool().lock().unwrap();
        pool.closed = true;
        pool.slots.drain().map(|(_, slot)| slot).collect()
    };
    for slot in slots {
        let running = loop {
            let mut state = slot.state.lock().unwrap();
            match &*state {
                PoolState::Running(current) => {
                    let server = Arc::clone(current);
                    *state = PoolState::Stopping;
                    break Some(server);
                }
                PoolState::Vacant => break None,
                PoolState::Starting | PoolState::Stopping => {
                    state = slot.changed.wait(state).unwrap();
                }
            }
        };
        let Some(inner) = running else {
            continue;
        };
        inner.server.shutdown(SERVER_EXIT_TIMEOUT);
        let mut state = slot.state.lock().unwrap();
        *state = PoolState::Vacant;
        slot.changed.notify_all();
    }
}

/// The stable directory the shared serve process runs in: a Fintwind data
/// directory, never a user workspace.
///
/// Debug builds share the checkout's gitignored `temp/` root with
/// [`crate::persistence::StateStore::default_path`]; release builds use the
/// per-user local data directory.
fn serve_working_directory() -> anyhow::Result<PathBuf> {
    let directory = if cfg!(debug_assertions) {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
            .join("temp")
            .join("opencode-serve")
    } else {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(crate::identity::DATA_DIRECTORY_NAME)
            .join("opencode-serve")
    };
    std::fs::create_dir_all(&directory).with_context(|| {
        format!(
            "could not create the OpenCode serve directory {}",
            directory.display()
        )
    })?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Instant;
    use uuid::Uuid;

    /// The pool is process-global and keyed by binary alone, so the
    /// real-server tests must run one at a time.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Reopens the process-global pool after a previous test's
    /// `shutdown_all`: clears the closed flag and leaves the map empty.
    /// Call while holding `TEST_LOCK`, before the first acquire.
    #[cfg(test)]
    fn reopen_pool() {
        let mut pool = pool().lock().unwrap();
        pool.closed = false;
        pool.slots.clear();
    }

    struct TestWorkspace {
        path: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("fintwind-opencode-pool-test-{}", Uuid::new_v4()));
            std::fs::create_dir(&path).expect("the test workspace should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn port_is_open(port: u16) -> bool {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    }

    fn wait_until_closed(port: u16) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && port_is_open(port) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!port_is_open(port), "the server should have stopped");
    }

    /// Proves the pool's contract against a real server: sessions in
    /// different workspaces but the same binary share one process, the
    /// process stays resident after every session handle drops, and only
    /// `shutdown_all` stops it. Ignored by default: needs the CLI installed.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn workspace_sessions_share_one_server_until_shutdown_all() {
        let _guard = TEST_LOCK.lock().unwrap();
        reopen_pool();
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let workspace_a = TestWorkspace::new();
        let workspace_b = TestWorkspace::new();

        let first =
            acquire(&binary, workspace_a.path()).expect("the first session should start the server");
        let port = first.port;
        assert!(port_is_open(port), "the server should be listening");

        let second =
            acquire(&binary, workspace_b.path()).expect("the second session should reuse it");
        assert_eq!(second.port, port, "both sessions must share one process");
        assert!(Arc::ptr_eq(&first.inner, &second.inner));

        drop(first);
        drop(second);
        assert!(
            port_is_open(port),
            "the server must stay resident without session handles"
        );

        shutdown_all();
        wait_until_closed(port);
        shutdown_all();
    }

    /// Two sessions starting together must share the same in-flight startup
    /// instead of briefly launching competing servers for one binary.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn concurrent_acquires_start_one_server() {
        let _guard = TEST_LOCK.lock().unwrap();
        reopen_pool();
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let barrier = Arc::new(Barrier::new(3));
        let starts = Arc::new(AtomicUsize::new(0));

        let acquire_concurrently = |barrier: Arc<Barrier>, starts: Arc<AtomicUsize>| {
            let binary = binary.clone();
            thread::spawn(move || {
                barrier.wait();
                acquire_with_start(&binary, || -> anyhow::Result<OpenCodeServer> {
                    starts.fetch_add(1, Ordering::Relaxed);
                    OpenCodeServer::start(&binary, &serve_working_directory()?)
                })
                .expect("the concurrent session should acquire a server")
            })
        };
        let first = acquire_concurrently(Arc::clone(&barrier), Arc::clone(&starts));
        let second = acquire_concurrently(Arc::clone(&barrier), Arc::clone(&starts));
        barrier.wait();

        let first = first.join().expect("the first acquire should not panic");
        let second = second.join().expect("the second acquire should not panic");
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert!(Arc::ptr_eq(&first.inner, &second.inner));
        assert_eq!(first.port, second.port);

        drop(first);
        drop(second);
        shutdown_all();
    }

    /// A dedicated server — Computer Use bakes per-session configuration into
    /// the environment — dies with its handle just like a superseded pooled
    /// generation.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn dedicated_server_dies_with_its_last_handle() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let cwd = TestWorkspace::new();

        let server = OpenCodeServer::start(&binary, cwd.path()).expect("the server should start");
        let port = server.port;
        let handle = PooledServer::dedicated(server);
        assert!(port_is_open(port));

        let clone = handle.clone();
        drop(handle);
        assert!(clone.is_alive(), "a clone should keep the server alive");

        let stopping = Instant::now();
        drop(clone);
        assert!(
            stopping.elapsed() < Duration::from_secs(2),
            "dedicated teardown should reap the exited child promptly"
        );
        wait_until_closed(port);
    }

    /// An exited generation must be replaced even while an older session
    /// handle still references it, and that stale handle must not kill the
    /// replacement when it eventually drops.
    #[test]
    #[ignore = "requires an installed opencode"]
    fn dead_server_recovers_without_cross_generation_teardown() {
        let _guard = TEST_LOCK.lock().unwrap();
        reopen_pool();
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let workspace = TestWorkspace::new();

        let stale = acquire(&binary, workspace.path()).expect("the first server should start");
        stale.shutdown(SERVER_EXIT_TIMEOUT);
        assert!(!stale.is_alive());

        let replacement =
            acquire(&binary, workspace.path()).expect("the dead server should be replaced");
        assert!(!Arc::ptr_eq(&stale.inner, &replacement.inner));
        drop(stale);
        assert!(
            replacement.is_alive(),
            "a stale generation must not stop its replacement"
        );
        assert!(port_is_open(replacement.port));

        drop(replacement);
        shutdown_all();
    }

    /// After `shutdown_all`, an acquire must refuse to start a process the
    /// pool would never shut down again; after reopening, starting works.
    /// Neither step needs opencode: the start closure never runs a real
    /// server.
    #[test]
    fn acquire_after_shutdown_all_cannot_start() {
        let _guard = TEST_LOCK.lock().unwrap();
        reopen_pool();

        shutdown_all();
        let refused = acquire_with_start(Path::new("opencode-must-not-start"), || {
            panic!("the start closure must not run once the pool is closed");
        });
        assert!(
            refused.is_err(),
            "acquire must fail after shutdown_all instead of orphaning a process"
        );

        reopen_pool();
        let started = AtomicUsize::new(0);
        let restarted = acquire_with_start(Path::new("opencode-must-not-start"), || {
            started.fetch_add(1, Ordering::Relaxed);
            Err(anyhow::anyhow!("no real server in this test"))
        });
        assert!(restarted.is_err(), "the fake start never produces a server");
        assert_eq!(
            started.load(Ordering::Relaxed),
            1,
            "a reopened pool must run the start closure again"
        );
    }

    /// The serve working directory is derived and created without opencode:
    /// it is its own directory under the data root, never a workspace.
    #[test]
    fn serve_working_directory_is_created_under_the_data_root() {
        let directory = serve_working_directory().expect("the serve directory should be created");
        assert!(directory.is_dir());
        assert_eq!(
            directory.file_name().and_then(|name| name.to_str()),
            Some("opencode-serve"),
            "the serve process must run in its own directory"
        );
    }
}
