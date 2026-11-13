//! Bounded cache for Git results that cannot go stale.
//!
//! Live worktree state — status, directory pages, single-file diffs — is not
//! cached here. Its only reliable invalidation signal is a file event, and the
//! daemon does not watch the worktree yet, so a cached answer could show a tree
//! the user already changed. What is cached is immutable by construction: a
//! commit's numstat, and one completed turn's diff between two fixed refs.
//!
//! Keys carry the canonical worktree path, which is the identity that separates
//! two worktrees sharing one Git common directory. Entries are evicted oldest
//! first against an entry count and a byte budget, so a large turn diff cannot
//! pin megabytes forever.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Upper bound on cached results.
const MAX_ENTRIES: usize = 128;
/// Upper bound on the summed size of their payloads.
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// Entries older than this are dropped the next time anything is inserted.
const MAX_AGE: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CacheKey {
    CommitStats { worktree: PathBuf, hash: String },
    TurnDiff { worktree: PathBuf, source: String },
}

pub(crate) enum Cached {
    CommitStats(fintwind_protocol::git::CommitStats),
    ReviewDiff(fintwind_protocol::workspace::ReviewDiffData),
}

impl Cached {
    fn bytes(&self) -> usize {
        match self {
            Self::CommitStats(_) => 64,
            Self::ReviewDiff(data) => data.patch.len() + data.numstat.len() + 128,
        }
    }
}

struct Entry {
    value: Arc<Cached>,
    bytes: usize,
    created: Instant,
    used: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<CacheKey, Entry>,
    bytes: usize,
    clock: u64,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

/// Returns a cached value younger than `max_age`.
pub(crate) fn get(key: &CacheKey, max_age: Duration) -> Option<Arc<Cached>> {
    let mut cache = cache().lock().ok()?;
    let expired = cache
        .entries
        .get(key)
        .is_some_and(|entry| entry.created.elapsed() > max_age);
    if expired {
        if let Some(entry) = cache.entries.remove(key) {
            cache.bytes -= entry.bytes;
        }
        return None;
    }
    cache.clock += 1;
    let used = cache.clock;
    let entry = cache.entries.get_mut(key)?;
    entry.used = used;
    Some(entry.value.clone())
}

pub(crate) fn insert(key: CacheKey, value: Cached) {
    let Ok(mut cache) = cache().lock() else {
        return;
    };
    let bytes = value.bytes();
    if bytes > MAX_BYTES {
        return;
    }
    // Reading never touches an entry whose key is not asked for again, so age
    // is also enforced here: a closed workspace's diff does not stay resident
    // for the life of the daemon.
    let now = Instant::now();
    let mut freed = 0;
    cache.entries.retain(|_, entry| {
        let keep = now.duration_since(entry.created) <= MAX_AGE;
        if !keep {
            freed += entry.bytes;
        }
        keep
    });
    cache.bytes -= freed;
    if let Some(previous) = cache.entries.remove(&key) {
        cache.bytes -= previous.bytes;
    }
    while cache.entries.len() >= MAX_ENTRIES || cache.bytes + bytes > MAX_BYTES {
        let oldest = cache
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.used)
            .map(|(key, _)| key.clone());
        let Some(oldest) = oldest else {
            break;
        };
        if let Some(entry) = cache.entries.remove(&oldest) {
            cache.bytes -= entry.bytes;
        }
    }
    cache.clock += 1;
    let used = cache.clock;
    cache.entries.insert(
        key,
        Entry {
            value: Arc::new(value),
            bytes,
            created: Instant::now(),
            used,
        },
    );
    cache.bytes += bytes;
}

/// Drops every entry belonging to one worktree. Called when the refs a cached
/// turn diff names can move or disappear.
pub(crate) fn invalidate_worktree(cwd: &Path) {
    let identity = worktree_identity(cwd);
    let Ok(mut cache) = cache().lock() else {
        return;
    };
    let mut freed = 0;
    cache.entries.retain(|key, entry| {
        let belongs = match key {
            CacheKey::CommitStats { worktree, .. } | CacheKey::TurnDiff { worktree, .. } => {
                worktree == &identity
            }
        };
        if belongs {
            freed += entry.bytes;
        }
        !belongs
    });
    cache.bytes -= freed;
}

/// The identity a cache key must carry: the worktree directory itself, not the
/// common Git directory shared by every linked worktree.
pub(crate) fn worktree_identity(cwd: &Path) -> PathBuf {
    std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf())
}
