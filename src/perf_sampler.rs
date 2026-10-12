//! Read-only resource sampling for the processes this Fintwind instance owns.
//!
//! Ownership is derived, never guessed from process names: each sample walks
//! the parent-child tree outward from this process using a Toolhelp snapshot,
//! and every membership decision is guarded by process creation times, so a
//! reused PID can never adopt an unrelated process. That covers the daemon,
//! the remote-control host, the pooled OpenCode servers, and whatever they
//! spawn (terminals, tools, browser helpers) — all descendants, nothing else
//! on the machine.
//!
//! The module is plain std + Win32 on purpose: no gpui, no locks the UI
//! thread could touch. The window in `perf_monitor` moves a [`Sampler`]
//! through a background spawn and reads only the returned [`Sample`].
//!
//! Stated measure of each column, kept next to the code that fills it:
//! CPU is one sample interval's process-time delta divided by wall time and
//! every logical processor on the machine (`GetActiveProcessorCount` across
//! all groups, which is Task Manager's denominator — not the app's affinity
//! limited parallelism); a first sample has no delta and reads unknown.
//! Private memory is `PrivateUsage` (committed private bytes); working set
//! is `WorkingSetSize` and is never summed because it double-counts shared
//! pages. I/O is `GetProcessIoCounters` — every byte the process has
//! transferred since it started, including pipes and network, which is why
//! the window's columns say I/O and not disk. A total over a column exists
//! only when every row's value for that column is known: a partial sum
//! would read as complete.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// One process row of a sample. Memory and I/O are in mebibytes so every
/// column reads at one scale; `None` means unknown — a counter read that
/// failed, or a first CPU sample without a delta — and renders as unknown,
/// never as zero.
///
/// Membership is identity-verified: a process the sampler cannot open, and
/// therefore cannot verify a creation time for, never appears as a blank
/// row — it is left out of the sample entirely, and this narrower scope is
/// the honest report rather than a fabricated row.
#[derive(Clone, Debug)]
pub(crate) struct SampleRow {
    pub name: String,
    pub pid: u32,
    /// The verified creation time this row's identity rests on: the same
    /// value the ownership walk matched, stable across samples while the
    /// process lives. `0` only when the root's own times read unknown.
    pub create_time: u64,
    pub cpu_percent: Option<f32>,
    pub private_mb: Option<f64>,
    pub working_set_mb: Option<f64>,
    pub io_read_mb: Option<f64>,
    pub io_write_mb: Option<f64>,
}

/// One complete sample: the owned process rows plus the totals the window
/// draws beneath the table.
#[derive(Clone, Debug)]
pub(crate) struct Sample {
    pub rows: Vec<SampleRow>,
    pub total_cpu_percent: Option<f32>,
    pub total_private_mb: Option<f64>,
    pub total_io_read_mb: Option<f64>,
    pub total_io_write_mb: Option<f64>,
}

/// What one owned process reported. `None` fields stay unknown rather than
/// reading as zero.
#[derive(Default)]
struct Metrics {
    create_time: Option<u64>,
    cpu_100ns: Option<u64>,
    working_set: Option<u64>,
    private: Option<u64>,
    io_read: Option<u64>,
    io_write: Option<u64>,
}

/// CPU baselines live for as long as the sampling loop does: the window
/// closes, the loop drops, the next opening starts fresh with unknown CPU.
#[derive(Default)]
pub(crate) struct Sampler {
    baselines: HashMap<u32, CpuBaseline>,
}

#[derive(Clone, Copy)]
struct CpuBaseline {
    /// The verified creation time the baseline belongs to. A PID whose
    /// current creation time differs is a different process; its baseline
    /// resets. An unopened process has no baseline at all.
    create_time: u64,
    /// Kernel + user thread time, 100 ns units.
    cpu_100ns: u64,
    at: Instant,
}

impl Sampler {
    /// Take one sample of this process's descendant tree. Fails when the
    /// system process list itself is unusable; the caller keeps showing the
    /// previous sample. Per-process gaps stay `None` inside an otherwise
    /// valid sample.
    pub(crate) fn sample(&mut self) -> Result<Sample, String> {
        sample_impl(self)
    }
}

#[cfg(windows)]
fn sample_impl(sampler: &mut Sampler) -> Result<Sample, String> {
    use std::mem::size_of;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount};

    /// Closes the snapshot whenever this call leaves, on every path.
    struct Snapshot(windows_sys::Win32::Foundation::HANDLE);

    impl Drop for Snapshot {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// One process visible in the system snapshot: parent and image name.
    struct Entry {
        parent: u32,
        name: String,
    }

    // The system-wide snapshot. One call per sample; the walk below touches
    // only the entries this process owns.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err("could not list the system processes".to_owned());
    }
    let _snapshot = Snapshot(snapshot);

    let mut alive: HashMap<u32, Entry> = HashMap::new();
    unsafe {
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry) == 0 {
            return Err(format!(
                "could not read the process list (error {})",
                GetLastError()
            ));
        }
        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            alive.insert(
                entry.th32ProcessID,
                Entry {
                    parent: entry.th32ParentProcessID,
                    name: if name.is_empty() {
                        "System".to_owned()
                    } else {
                        name
                    },
                },
            );
            if Process32NextW(snapshot, &mut entry) == 0 {
                match GetLastError() {
                    ERROR_NO_MORE_FILES => break,
                    error => {
                        return Err(format!("could not enumerate the processes (error {error})"));
                    }
                }
            }
        }
    }
    // An incomplete walk must never pass an `expect` on the missing self.
    if !alive.contains_key(&std::process::id()) {
        return Err("this process is missing from the system list".to_owned());
    }

    // Children grouped by parent, so the walk never rescans the table.
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, entry) in &alive {
        children.entry(entry.parent).or_default().push(pid);
    }

    // Descendants tracked by an earlier sample stay owned even after their
    // own parent has exited — the tree walk cannot see through a dead link.
    // Only identities whose creation time was verified on some earlier
    // sample may continue: a tracked entry recorded as unopened (`None`)
    // never seeds the walk, so a PID reused by an unrelated process cannot
    // be claimed on PID alone.
    fn owned_tree() -> &'static Mutex<HashMap<u32, (u32, Option<u64>)>> {
        static TREE: OnceLock<Mutex<HashMap<u32, (u32, Option<u64>)>>> = OnceLock::new();
        TREE.get_or_init(|| Mutex::new(HashMap::new()))
    }
    let self_pid = std::process::id();
    let tree = owned_tree()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tracked = tree.clone();
    let mut queue: Vec<u32> = tree
        .iter()
        .filter(|&(pid, (_, create))| {
            *pid != self_pid && create.is_some() && alive.contains_key(pid)
        })
        .map(|(pid, _)| *pid)
        .collect();
    drop(tree);

    queue.push(self_pid);

    // Every member of `owned` carries a verified creation time; the root is
    // the one member allowed in regardless (its own counters can still read
    // unknown).
    let mut owned: HashMap<u32, Metrics> = HashMap::new();
    let mut visited: HashSet<u32> = HashSet::new();
    // The claimed parent of each BFS-discovered child, checked when the
    // child itself is visited.
    let mut claimed_parent: HashMap<u32, u32> = HashMap::new();

    while let Some(pid) = queue.pop() {
        if !visited.insert(pid) {
            continue;
        }
        let metrics = open_metrics(pid);
        if pid == self_pid {
            // The root is always this process, whatever its counters read.
            owned.insert(pid, metrics);
        } else {
            // Identity: a tracked PID must show the same creation time it
            // was verified with, or it was reused by something this app
            // never started. An unopened PID proves nothing and joins
            // nothing.
            let Some(create) = metrics.create_time else {
                continue;
            };
            if let Some(&(_, Some(prev))) = tracked.get(&pid) {
                if prev != create {
                    continue;
                }
            }
            // A tree edge needs a live, verified parent and a child no
            // older than it; an unknown parent age must not wave the child
            // through.
            let verified_edge = claimed_parent
                .get(&pid)
                .and_then(|parent| owned.get(parent))
                .and_then(|parent| parent.create_time)
                .map(|parent_create| create >= parent_create)
                .unwrap_or(false);
            // Orphan seeds carry no edge: their identity was checked above
            // against the recorded creation time.
            if claimed_parent.contains_key(&pid) && !verified_edge {
                continue;
            }
            owned.insert(pid, metrics);
        }
        if let Some(kids) = children.get(&pid) {
            for &kid in kids {
                if !visited.contains(&kid) {
                    claimed_parent.insert(kid, pid);
                    queue.push(kid);
                }
            }
        }
    }

    // Publish the surviving ownership for the next sample, replacing the
    // table wholesale so exited PIDs never accumulate. Only verified
    // identities are published: an unopened process is never remembered as
    // a claim.
    let published: HashMap<u32, (u32, Option<u64>)> = owned
        .iter()
        .filter_map(|(&pid, metrics)| {
            let create = metrics.create_time?;
            let parent = if pid == self_pid {
                0
            } else {
                claimed_parent
                    .get(&pid)
                    .copied()
                    .or_else(|| tracked.get(&pid).map(|&(parent, _)| parent))
                    .unwrap_or(0)
            };
            Some((pid, (parent, Some(create))))
        })
        .collect();
    *owned_tree()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = published;

    // Normalize each process's CPU delta against every logical processor on
    // the machine — Task Manager's denominator, not the app's affinity
    // limited parallelism — and advance the baselines.
    let machine_processors = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } as f64;
    let logical_processors = if machine_processors > 0.0 {
        machine_processors
    } else {
        std::thread::available_parallelism()
            .map(|count| count.get() as f64)
            .unwrap_or(1.0)
    };
    let now = Instant::now();
    let live: HashSet<u32> = owned.keys().copied().collect();
    let mut rows: Vec<SampleRow> = Vec::with_capacity(owned.len());
    for (pid, metrics) in owned {
        // `owned` only ever gains members that the walk read out of `alive`,
        // but an absent entry must skip the row, never panic.
        let Some(entry) = alive.get(&pid) else {
            continue;
        };
        let cpu_percent = match (
            sampler.baselines.get(&pid),
            metrics.create_time,
            metrics.cpu_100ns,
        ) {
            (
                Some(CpuBaseline {
                    create_time: baseline_create,
                    cpu_100ns: baseline_cpu,
                    at: baseline_at,
                }),
                Some(create),
                Some(cpu),
            ) if *baseline_create == create => {
                let delta = cpu.saturating_sub(*baseline_cpu);
                let wall = now.saturating_duration_since(*baseline_at);
                let wall_seconds = wall.as_secs_f64().max(0.001);
                Some(
                    ((delta as f64 / 10_000_000.0) / (wall_seconds * logical_processors) * 100.0)
                        .clamp(0.0, 100.0) as f32,
                )
            }
            _ => None,
        };
        if let (Some(create), Some(cpu)) = (metrics.create_time, metrics.cpu_100ns) {
            sampler.baselines.insert(
                pid,
                CpuBaseline {
                    create_time: create,
                    cpu_100ns: cpu,
                    at: now,
                },
            );
        } else {
            sampler.baselines.remove(&pid);
        }
        rows.push(SampleRow {
            name: entry.name.clone(),
            pid,
            create_time: metrics.create_time.unwrap_or(0),
            cpu_percent,
            private_mb: metrics.private.map(|bytes| bytes as f64 / 1_048_576.0),
            working_set_mb: metrics.working_set.map(|bytes| bytes as f64 / 1_048_576.0),
            io_read_mb: metrics.io_read.map(|bytes| bytes as f64 / 1_048_576.0),
            io_write_mb: metrics.io_write.map(|bytes| bytes as f64 / 1_048_576.0),
        });
    }
    sampler.baselines.retain(|pid, _| live.contains(pid));

    // Busiest first; unknown CPU sorts below measured values.
    rows.sort_by(|a, b| {
        b.cpu_percent
            .unwrap_or(-1.0)
            .total_cmp(&a.cpu_percent.unwrap_or(-1.0))
            .then_with(|| {
                b.private_mb
                    .unwrap_or(0.0)
                    .total_cmp(&a.private_mb.unwrap_or(0.0))
            })
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pid.cmp(&b.pid))
    });

    // A total exists only when every row's value is known; a partial sum
    // would read as complete.
    fn fold_strict(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
        let mut total = 0.0;
        for value in values {
            total += value?;
        }
        Some(total)
    }
    let total_cpu_percent = if rows.iter().any(|row| row.cpu_percent.is_none()) {
        None
    } else {
        Some(rows.iter().filter_map(|row| row.cpu_percent).sum())
    };

    Ok(Sample {
        total_cpu_percent,
        total_private_mb: fold_strict(rows.iter().map(|row| row.private_mb)),
        total_io_read_mb: fold_strict(rows.iter().map(|row| row.io_read_mb)),
        total_io_write_mb: fold_strict(rows.iter().map(|row| row.io_write_mb)),
        rows,
    })
}

/// Opens the process once and reads every counter through the same handle.
/// A denied or vanished process leaves its fields unknown; the caller then
/// treats the identity as unverified.
#[cfg(windows)]
fn open_metrics(pid: u32) -> Metrics {
    use std::mem::{size_of, zeroed};

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessIoCounters, GetProcessTimes, IO_COUNTERS, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let mut metrics = Metrics::default();
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return metrics;
    }

    unsafe {
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) != 0 {
            let filetime = |value: &FILETIME| -> u64 {
                ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
            };
            metrics.create_time = Some(filetime(&creation));
            metrics.cpu_100ns = Some(filetime(&kernel).saturating_add(filetime(&user)));
        }

        // `PrivateUsage` sits one field past `PROCESS_MEMORY_COUNTERS`'s
        // declared size; the EX variant is the documented way to read it.
        let mut memory: PROCESS_MEMORY_COUNTERS_EX = zeroed();
        memory.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if GetProcessMemoryInfo(
            handle,
            &mut memory as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            memory.cb,
        ) != 0
        {
            metrics.working_set = Some(memory.WorkingSetSize as u64);
            metrics.private = Some(memory.PrivateUsage as u64);
        }

        let mut io: IO_COUNTERS = zeroed();
        if GetProcessIoCounters(handle, &mut io) != 0 {
            metrics.io_read = Some(io.ReadTransferCount as u64);
            metrics.io_write = Some(io.WriteTransferCount as u64);
        }

        CloseHandle(handle);
    }
    metrics
}

#[cfg(not(windows))]
fn sample_impl(_: &mut Sampler) -> Result<Sample, String> {
    Err("resource sampling is only available on Windows".to_owned())
}
