//! OpenCode session token/cost usage scanner and split index logic.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use fintwind_http::encode_path_segment;
use fintwind_protocol::model::unix_time;
use fintwind_protocol::provider_session::{
    UsageDayShare, UsageEntry, UsageModelLane, UsageStats,
};

const PAGE_LIMIT: usize = 200;
const SESSION_LIST_LIMIT: usize = 500;
const MAX_SESSION_PAGES: usize = 50;
const MAX_MESSAGE_PAGES: usize = 50;

/// What a session row is, in the only distinction that matters for usage.
///
/// `parentID` alone conflates two very different things, and treating them
/// the same either double-counts or drops real spend:
///
/// - a **fork** (`forkSessionID` set) replays the parent's history into a new
///   session. OpenCode zeroes the cloned `step-finish` parts on fork
///   (#31136/#31138) precisely so its token row stays its own post-fork
///   spend, which *is* additive — so a fork counts as its own entry.
/// - a **sub-agent** (`parentID` set, no fork marker) is a separate session
///   with its own token accounting, and its parent's aggregate does *not*
///   include it. Measured on a real store: 103 sub-agent sessions holding
///   128.8M tokens that a `parentID`-only filter discards — 8.6% of the
///   store's total.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum SessionKind {
    TopLevel,
    Fork,
    SubAgent,
}

pub fn session_kind(row: &Value) -> SessionKind {
    if row
        .get("forkSessionID")
        .or_else(|| row.get("fork_session_id"))
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return SessionKind::Fork;
    }
    if row
        .get("parentID")
        .or_else(|| row.get("parent_id"))
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return SessionKind::SubAgent;
    }
    SessionKind::TopLevel
}

/// The sidebar models one linear conversation per row: forks and sub-agent
/// children are both left out of it.
pub fn is_child_session(row: &Value) -> bool {
    !matches!(session_kind(row), SessionKind::TopLevel)
}


pub fn ms_to_seconds(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .map(|milliseconds| milliseconds / 1_000)
}

///
/// Reconcile the global session listing, then read only uncached/changed
/// histories in a bounded pool. Model attribution is independent of calendar
/// range. Publish cached facts before waiting for any message history.
pub fn fetch_usage_stats(
    request: &(impl Fn(&str) -> anyhow::Result<Value> + Sync),
    cache: &crate::usage_index::UsageCache,
    publish: &impl Fn(UsageStats),
) -> anyhow::Result<UsageStats> {
    let mut stats = UsageStats::default();
    let mut cursor: Option<String> = None;
    let mut rows_by_id: HashMap<String, RowFacts> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut page_cursors = HashSet::new();
    let listing_started = std::time::Instant::now();

    for page in 0..MAX_SESSION_PAGES {
        let mut path = format!("/api/session?limit={SESSION_LIST_LIMIT}");
        if let Some(token) = &cursor {
            path.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let response = request(&path)?;
        let rows = response
            .pointer("/data")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("invalid usage session listing"))?;
        let exhausted = rows.is_empty();
        let next = response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .filter(|next| !next.is_empty());
        if let Some(next) = next {
            anyhow::ensure!(
                page_cursors.insert(next.to_owned()),
                "usage session cursor repeated; cached history retained"
            );
        }
        let has_more = next.is_some();
        anyhow::ensure!(
            !exhausted || !has_more,
            "empty usage page has a continuation cursor"
        );
        stats.sessions_scanned += rows.len();
        for row in &rows {
            let (id, facts) =
                row_facts(row).ok_or_else(|| anyhow::anyhow!("invalid usage session row"))?;
            seen.insert(id.clone());
            rows_by_id.insert(id, facts);
        }
        if exhausted || !has_more {
            break;
        }
        cursor = response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned);
        // Ran out of pages with more still listed. Say so: a silently short
        // scan is indistinguishable from a quiet month.
        if page + 1 == MAX_SESSION_PAGES {
            stats.truncated = true;
            eprintln!(
                "usage scan hit the {MAX_SESSION_PAGES}-page session cap; \
                 statistics cover only the newest portion"
            );
        }
    }

    anyhow::ensure!(
        !stats.truncated,
        "usage session listing is incomplete; cached history retained"
    );
    eprintln!(
        "usage listing: {} ms, {} sessions",
        listing_started.elapsed().as_millis(),
        stats.sessions_scanned
    );
    // Parents before children: a sub-agent folds into its parent's entry, and
    // a HashMap's visit order cannot be relied on to bring them together. The
    // partition is also what makes the result deterministic — the same store
    // has to produce the same totals twice.
    let depths: HashMap<String, usize> = rows_by_id
        .iter()
        .filter(|(_, facts)| facts.kind == SessionKind::SubAgent)
        .map(|(id, facts)| {
            let mut depth = 0;
            let mut parent = facts.parent.as_deref();
            while let Some(next) = parent {
                depth += 1;
                if depth > rows_by_id.len() {
                    break;
                }
                parent = rows_by_id.get(next).and_then(|row| row.parent.as_deref());
            }
            (id.clone(), depth)
        })
        .collect();
    let (mut subagents, parents): (Vec<_>, Vec<_>) = rows_by_id
        .into_iter()
        .partition(|(_, facts)| facts.kind == SessionKind::SubAgent);
    subagents.sort_by_key(|(id, _)| depths.get(id).copied().unwrap_or_default());
    let rows: Vec<_> = parents.into_iter().chain(subagents).collect();
    let mut splits = Vec::with_capacity(rows.len());
    let mut pending = Vec::new();
    for (index, (id, facts)) in rows.iter().enumerate() {
        let fingerprint = serde_json::to_string(facts)?;
        let recent = unix_time().saturating_sub(facts.updated) < 7 * 86_400;
        let cached = cache.split(id, &fingerprint, recent)?;
        if cached.is_some() {
            splits.push(cached);
            continue;
        }
        let previous = cache.previous_split(id)?;
        if cached.is_none() && (previous.is_some() || facts.tokens.iter().any(|tokens| *tokens > 0))
        {
            pending.push(index);
        }
        splits.push(cached.or(previous));
    }
    pending.sort_by_key(|index| std::cmp::Reverse(rows[*index].1.updated));
    stats.refreshing = true;
    stats.entries = assemble_usage_entries(&rows, &splits);
    publish(stats.clone());
    // Bounded batches persist progress and publish at most once per batch.
    // All sessions need model attribution, not just cross-day sessions.
    let mut published = std::time::Instant::now();
    for batch in pending.chunks(16) {
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|worker| {
                    let rows = &rows;
                    scope.spawn(move || {
                        batch
                            .iter()
                            .skip(worker)
                            .step_by(4)
                            .copied()
                            .map(|index| {
                                let (id, facts) = &rows[index];
                                (index, indexed_session_split(request, cache, id, facts))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("usage worker panicked"))
                })
                .collect::<anyhow::Result<Vec<_>>>()
        })?;
        for (index, result) in results.into_iter().flatten() {
            match result {
                Ok(Some(split)) => splits[index] = Some(split),
                Ok(None) => {}
                Err(error) => {
                    // A partial history must not turn an otherwise usable
                    // snapshot into a page-level failure. Keep its cached split
                    // (or unknown summary) and retry in a later reconciliation.
                    eprintln!("usage history skipped for {}: {error}", rows[index].0);
                }
            }
        }
        if published.elapsed() >= Duration::from_secs(1) {
            stats.entries = assemble_usage_entries(&rows, &splits);
            cache.save_snapshot(&stats)?;
            publish(stats.clone());
            published = std::time::Instant::now();
        }
    }
    stats.entries = assemble_usage_entries(&rows, &splits);
    // An incomplete listing is not evidence that unseen sessions were deleted.
    if !stats.truncated {
        cache.retain(&seen)?;
    }
    Ok(stats)
}

fn assemble_usage_entries(
    rows: &[(String, RowFacts)],
    splits: &[Option<SessionSplit>],
) -> Vec<UsageEntry> {
    let mut entries = Vec::with_capacity(rows.len());
    let mut entry_index: HashMap<String, usize> = HashMap::new();
    for (row_index, (id, facts)) in rows.iter().enumerate() {
        let Some(mut entry) = usage_entry_from_row(facts) else {
            continue;
        };
        let split = splits[row_index].as_ref();
        if facts.kind == SessionKind::SubAgent
            && let Some(&parent_index) = facts.parent.as_deref().and_then(|p| entry_index.get(p)) {
                if let Some(split) = split {
                    apply_usage_split(&mut entry, split);
                }
                fold_subagent_into(&mut entries[parent_index], &entry, id);
                entry_index.insert(id.clone(), parent_index);
                // The parent's entry owns the totals; the child's model is
                // retained separately for the model and provider rankings.
                continue;
            }
            // An orphaned sub-agent (its parent was deleted or aged out of the
            // listing) is still real spend. Keep it rather than drop it.
        entry_index.insert(id.clone(), entries.len());
        if let Some(split) = split {
            apply_usage_split(&mut entry, split);
        }
        entries.push(entry);
    }

    entries.sort_by_key(|entry| entry.timestamp);
    entries
}

fn apply_usage_split(entry: &mut UsageEntry, split: &SessionSplit) {
    // A summary can lag behind a completed message. Never drop verified usage
    // just because the session row has not caught up yet.
    entry.input_tokens = entry
        .input_tokens
        .max(split.messages.iter().map(|message| message.input).sum());
    entry.output_tokens = entry.output_tokens.max(
        split
            .messages
            .iter()
            .map(|message| message.output.saturating_sub(message.reasoning))
            .sum(),
    );
    entry.reasoning_tokens = entry
        .reasoning_tokens
        .max(split.messages.iter().map(|message| message.reasoning).sum());
    entry.cache_read_tokens = entry.cache_read_tokens.max(
        split
            .messages
            .iter()
            .map(|message| message.cache_read)
            .sum(),
    );
    entry.cache_write_tokens = entry.cache_write_tokens.max(
        split
            .messages
            .iter()
            .map(|message| message.cache_write)
            .sum(),
    );
    entry.days = Some(split.days.clone());
    entry.hours = split.hours.clone();
    entry.model_lanes = split.models.clone();
}

/// Fold a sub-agent's usage into its parent.
///
/// A sub-agent books its tokens into its own session row and its parent's
/// aggregate does not include them, so the parent's total is short by exactly
/// this amount until the fold. Every lane is added, not just the headline
/// number: folding only `total` would leave the input/output breakdown the
/// KPI card prints disagreeing with the figures above it.
///
/// The cost joins too, and it is merged rather than replaced: a sub-agent's
/// estimate is additive spend, and throwing it away would make the page's
/// "cost" KPI quietly low on exactly the sessions that delegate the most.
fn fold_subagent_into(parent: &mut UsageEntry, child: &UsageEntry, child_id: &str) {
    let mut child_days = child
        .days
        .clone()
        .unwrap_or_else(|| vec![entry_whole_day(child)]);
    let missing_input = child
        .input_tokens
        .saturating_sub(child_days.iter().map(|day| day.input).sum());
    let missing_output = child
        .output_tokens
        .saturating_add(child.reasoning_tokens)
        .saturating_sub(child_days.iter().map(|day| day.output).sum());
    let missing_read = child
        .cache_read_tokens
        .saturating_sub(child_days.iter().map(|day| day.cache_read).sum());
    let missing_write = child
        .cache_write_tokens
        .saturating_sub(child_days.iter().map(|day| day.cache_write).sum());
    let missing_total = missing_input
        .saturating_add(missing_output)
        .saturating_add(missing_read)
        .saturating_add(missing_write);
    if missing_total > 0 {
        child_days.push(UsageDayShare {
            timestamp: child.timestamp,
            direct: missing_input.saturating_add(missing_output),
            total: missing_total,
            input: missing_input,
            output: missing_output,
            cache_read: missing_read,
            cache_write: missing_write,
            cost: (child.cost.unwrap_or_default()
                - child_days.iter().map(|day| day.cost).sum::<f64>())
            .max(0.0),
            ..Default::default()
        });
    }
    let mut child_hours = if child.hours.is_empty() {
        vec![entry_whole_hour(child)]
    } else {
        child.hours.clone()
    };
    if !child.hours.is_empty() {
        let missing = [
            child.input_tokens,
            child.output_tokens.saturating_add(child.reasoning_tokens),
            child.cache_read_tokens,
            child.cache_write_tokens,
        ];
        let recorded = [
            child_hours.iter().map(|hour| hour.input).sum(),
            child_hours.iter().map(|hour| hour.output).sum(),
            child_hours.iter().map(|hour| hour.cache_read).sum(),
            child_hours.iter().map(|hour| hour.cache_write).sum(),
        ];
        let remaining =
            std::array::from_fn::<_, 4, _>(|index| missing[index].saturating_sub(recorded[index]));
        if remaining.iter().any(|tokens| *tokens > 0) {
            child_hours.push(fintwind_protocol::provider_session::UsageHourShare {
                timestamp: child.timestamp,
                estimated: true,
                input: remaining[0],
                output: remaining[1],
                cache_read: remaining[2],
                cache_write: remaining[3],
                cost: 0.0,
            });
        }
    }
    let mut model_parts = child
        .model_lanes
        .iter()
        .filter(|lane| lane.total > 0)
        .map(
            |lane| fintwind_protocol::provider_session::UsageSubagentModel {
                session_id: child_id.to_owned(),
                model: Some(lane.model.clone()),
                input_tokens: lane.tokens[0],
                output_tokens: lane.tokens[1],
                reasoning_tokens: lane.tokens[2],
                cache_read_tokens: lane.tokens[3],
                cache_write_tokens: lane.tokens[4],
                cost: Some(lane.cost),
                days: lane.days.clone(),
                hours: lane.hours.clone(),
                requests: lane.requests.clone(),
            },
        )
        .collect::<Vec<_>>();
    let represented = [
        model_parts
            .iter()
            .map(|part| part.input_tokens)
            .sum::<u64>(),
        model_parts
            .iter()
            .map(|part| part.output_tokens)
            .sum::<u64>(),
        model_parts
            .iter()
            .map(|part| part.reasoning_tokens)
            .sum::<u64>(),
        model_parts
            .iter()
            .map(|part| part.cache_read_tokens)
            .sum::<u64>(),
        model_parts
            .iter()
            .map(|part| part.cache_write_tokens)
            .sum::<u64>(),
    ];
    let totals = [
        child.input_tokens,
        child.output_tokens,
        child.reasoning_tokens,
        child.cache_read_tokens,
        child.cache_write_tokens,
    ];
    let remainder =
        std::array::from_fn::<_, 5, _>(|index| totals[index].saturating_sub(represented[index]));
    if model_parts.is_empty() || remainder.iter().any(|tokens| *tokens > 0) {
        let mut accounted_days = HashSet::new();
        let remainder_days = child_days
            .iter()
            .filter_map(|day| {
                let same_day = model_parts
                    .iter()
                    .flat_map(|part| &part.days)
                    .filter(|part| {
                        local_day(part.timestamp) == local_day(day.timestamp)
                            && !accounted_days.contains(&local_day(day.timestamp))
                    });
                let parts: Vec<_> = same_day.collect();
                accounted_days.insert(local_day(day.timestamp));
                let mut remaining = *day;
                remaining.input = day
                    .input
                    .saturating_sub(parts.iter().map(|part| part.input).sum());
                remaining.output = day
                    .output
                    .saturating_sub(parts.iter().map(|part| part.output).sum());
                remaining.cache_read = day
                    .cache_read
                    .saturating_sub(parts.iter().map(|part| part.cache_read).sum());
                remaining.cache_write = day
                    .cache_write
                    .saturating_sub(parts.iter().map(|part| part.cache_write).sum());
                remaining.total = remaining.input
                    + remaining.output
                    + remaining.cache_read
                    + remaining.cache_write;
                remaining.direct = remaining.input + remaining.output;
                remaining.cost =
                    (day.cost - parts.iter().map(|part| part.cost).sum::<f64>()).max(0.0);
                (remaining.total > 0).then_some(remaining)
            })
            .collect();
        let mut accounted_hours = HashSet::new();
        let remainder_hours = child_hours
            .iter()
            .filter_map(|hour| {
                let parts: Vec<_> = model_parts
                    .iter()
                    .flat_map(|part| &part.hours)
                    .filter(|part| {
                        part.timestamp / 3600 == hour.timestamp / 3600
                            && !accounted_hours.contains(&(hour.timestamp / 3600))
                    })
                    .collect();
                accounted_hours.insert(hour.timestamp / 3600);
                let mut remaining = *hour;
                remaining.input = hour
                    .input
                    .saturating_sub(parts.iter().map(|part| part.input).sum());
                remaining.output = hour
                    .output
                    .saturating_sub(parts.iter().map(|part| part.output).sum());
                remaining.cache_read = hour
                    .cache_read
                    .saturating_sub(parts.iter().map(|part| part.cache_read).sum());
                remaining.cache_write = hour
                    .cache_write
                    .saturating_sub(parts.iter().map(|part| part.cache_write).sum());
                remaining.estimated = true;
                (remaining.input + remaining.output + remaining.cache_read + remaining.cache_write
                    > 0)
                .then_some(remaining)
            })
            .collect();
        model_parts.push(fintwind_protocol::provider_session::UsageSubagentModel {
            session_id: child_id.to_owned(),
            model: child.model.clone(),
            input_tokens: remainder[0],
            output_tokens: remainder[1],
            reasoning_tokens: remainder[2],
            cache_read_tokens: remainder[3],
            cache_write_tokens: remainder[4],
            cost: child.cost.map(|cost| {
                (cost
                    - model_parts
                        .iter()
                        .map(|part| part.cost.unwrap_or_default())
                        .sum::<f64>())
                .max(0.0)
            }),
            days: remainder_days,
            hours: remainder_hours,
            requests: Vec::new(),
        });
    }
    let priced_hours = model_parts
        .iter()
        .flat_map(|part| part.hours.iter().copied())
        .collect::<Vec<_>>();
    parent.subagent_models.extend(model_parts);
    // Preserve the child's date (and hour when available) before merging its
    // cumulative totals. The parent's last touch is not when the child spent.
    if parent.days.is_none() {
        parent.days = Some(vec![entry_whole_day(parent)]);
    }
    if parent.hours.is_empty() {
        parent.hours.push(entry_whole_hour(parent));
    }
    parent
        .days
        .as_mut()
        .unwrap()
        .extend(child_days.into_iter().map(|mut share| {
            share.subagent = true;
            share
        }));
    parent.hours.extend(priced_hours);
    parent.input_tokens = parent.input_tokens.saturating_add(child.input_tokens);
    parent.output_tokens = parent.output_tokens.saturating_add(child.output_tokens);
    parent.reasoning_tokens = parent
        .reasoning_tokens
        .saturating_add(child.reasoning_tokens);
    parent.cache_read_tokens = parent
        .cache_read_tokens
        .saturating_add(child.cache_read_tokens);
    parent.cache_write_tokens = parent
        .cache_write_tokens
        .saturating_add(child.cache_write_tokens);
    parent.cost = match (parent.cost, child.cost) {
        (Some(parent_cost), Some(child_cost)) => Some(parent_cost + child_cost),
        (parent_cost, child_cost) => parent_cost.or(child_cost),
    };
    parent.subagent_sessions = parent.subagent_sessions.saturating_add(1);
    parent.subagent_tokens = parent.subagent_tokens.saturating_add(child.total_tokens());
    parent.subagent_direct = parent.subagent_direct.saturating_add(
        child
            .input_tokens
            .saturating_add(child.output_tokens)
            .saturating_add(child.reasoning_tokens),
    );
}

fn entry_whole_day(entry: &UsageEntry) -> UsageDayShare {
    UsageDayShare {
        timestamp: entry.timestamp,
        subagent: false,
        direct: entry
            .input_tokens
            .saturating_add(entry.output_tokens)
            .saturating_add(entry.reasoning_tokens),
        total: entry.total_tokens(),
        input: entry.input_tokens,
        output: entry.output_tokens.saturating_add(entry.reasoning_tokens),
        cache_read: entry.cache_read_tokens,
        cache_write: entry.cache_write_tokens,
        cost: entry.cost.unwrap_or_default(),
    }
}

fn entry_whole_hour(entry: &UsageEntry) -> fintwind_protocol::provider_session::UsageHourShare {
    fintwind_protocol::provider_session::UsageHourShare {
        timestamp: entry.timestamp,
        estimated: true,
        input: entry.input_tokens,
        output: entry.output_tokens.saturating_add(entry.reasoning_tokens),
        cache_read: entry.cache_read_tokens,
        cache_write: entry.cache_write_tokens,
        cost: entry.cost.unwrap_or_default(),
    }
}

/// The parts of a session row the scan needs. Only these are kept, so the
/// scan never holds the session list's JSON alive past one page.
#[derive(Serialize)]
struct RowFacts {
    kind: SessionKind,
    /// `parentID` for a sub-agent; `None` otherwise.
    parent: Option<String>,
    /// Unix seconds.
    created: u64,
    updated: u64,
    /// Millisecond-resolution fingerprint; second truncation misses quick updates.
    updated_millis: u64,
    model: Option<String>,
    directory: Option<String>,
    cost: Option<f64>,
    tokens: [u64; 5],
}

fn row_facts(row: &Value) -> Option<(String, RowFacts)> {
    let id = row.get("id").and_then(Value::as_str)?.to_owned();
    let time = row.get("time")?;
    let created =
        ms_to_seconds(time.get("created")).or_else(|| ms_to_seconds(time.get("updated")))?;
    let updated = ms_to_seconds(time.get("updated")).unwrap_or(created);
    let updated_millis = time
        .get("updated")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| updated.saturating_mul(1_000));
    let kind = session_kind(row);
    let parent = (kind == SessionKind::SubAgent)
        .then(|| {
            row.get("parentID")
                .or_else(|| row.get("parent_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .flatten();
    // Kept exactly as reported, zero included: a provider answering "free"
    // is not the same as answering nothing, and the page reports how many
    // sessions carried a cost at all.
    let cost = row.get("cost").and_then(Value::as_f64);
    let empty_tokens = Value::Null;
    let tokens = row.get("tokens").unwrap_or(&empty_tokens);
    let lane = |pointer: &str| tokens.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    Some((
        id,
        RowFacts {
            kind,
            parent,
            created,
            updated,
            updated_millis,
            model: row.get("model").and_then(|model| {
                let provider = model.get("providerID").and_then(Value::as_str)?;
                let id = model
                    .get("id")
                    .or_else(|| model.get("modelID"))
                    .and_then(Value::as_str)?;
                Some(format!("{provider}/{id}"))
            }),
            directory: row
                .pointer("/location/directory")
                .and_then(Value::as_str)
                .map(str::to_owned),
            cost,
            tokens: [
                lane("/input"),
                lane("/output"),
                lane("/reasoning"),
                lane("/cache/read"),
                lane("/cache/write"),
            ],
        },
    ))
}

/// One session row's contribution to the usage scan. Rows without a usable
/// timestamp fold out: they cannot land on a day, and the totals the page
/// draws are all day-bucketed. A row without `tokens` still counts as a
/// session — it just adds zero tokens.
fn usage_entry_from_row(facts: &RowFacts) -> Option<UsageEntry> {
    // Last activity is when the session's tokens were spent, as far as a
    // day bucket can tell.
    let timestamp = facts.updated.max(facts.created);
    Some(UsageEntry {
        timestamp,
        model: None,
        directory: facts.directory.clone(),
        cost: facts.cost,
        input_tokens: facts.tokens[0],
        output_tokens: facts.tokens[1],
        reasoning_tokens: facts.tokens[2],
        cache_read_tokens: facts.tokens[3],
        cache_write_tokens: facts.tokens[4],
        subagent_sessions: 0,
        subagent_tokens: 0,
        subagent_direct: 0,
        days: None,
        hours: Vec::new(),
        model_lanes: Vec::new(),
        subagent_models: Vec::new(),
    })
}

/// What one message walk yields for a session: the day split that repairs a
/// multi-day session's timeline, and the model split that repairs a
/// model-switching one's ranking. Both come from the same walk, so neither
/// costs an extra request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionSplit {
    days: Vec<UsageDayShare>,
    hours: Vec<fintwind_protocol::provider_session::UsageHourShare>,
    models: Vec<UsageModelLane>,
    messages: Vec<MessageUsage>,
}

/// Reuse a verified fingerprint or atomically replace the session's message
/// facts after a complete walk. Failed reads leave the old facts untouched.
fn indexed_session_split(
    request: &impl Fn(&str) -> anyhow::Result<Value>,
    cache: &crate::usage_index::UsageCache,
    session_id: &str,
    facts: &RowFacts,
) -> anyhow::Result<Option<SessionSplit>> {
    let fingerprint = serde_json::to_string(facts)?;
    let recent = unix_time().saturating_sub(facts.updated) < 7 * 86_400;
    if let Some(split) = cache.split(session_id, &fingerprint, recent)? {
        return Ok(Some(split));
    }
    let split = session_day_split(request, session_id)?;
    if split
        .as_ref()
        .is_some_and(|split| split.messages.is_empty())
        && facts.tokens.iter().any(|tokens| *tokens > 0)
    {
        anyhow::bail!("usage messages have not caught up with session {session_id}");
    }
    // An empty walk with nonzero summary was rejected above and stays retryable.
    if let Some(split) = &split {
        cache.save_split(session_id, &fingerprint, split)?;
    }
    Ok(split)
}

/// Walk one session's assistant messages, bucketing their tokens by local day
/// and by model. A failed detail walk is reported to the caller so the page
/// cannot claim its hourly series is complete when one session was missed.
fn session_day_split(
    request: &impl Fn(&str) -> anyhow::Result<Value>,
    session_id: &str,
) -> anyhow::Result<Option<SessionSplit>> {
    let mut cursor: Option<String> = None;
    let mut days: Vec<UsageDayShare> = Vec::new();
    let mut index_of_day: HashMap<u64, usize> = HashMap::new();
    let mut hours: Vec<fintwind_protocol::provider_session::UsageHourShare> = Vec::new();
    let mut index_of_hour: HashMap<u64, usize> = HashMap::new();
    let mut models: Vec<UsageModelLane> = Vec::new();
    let mut index_of_model: HashMap<String, usize> = HashMap::new();
    let mut messages = Vec::new();
    let mut seen = HashSet::new();
    let mut page_cursors = HashSet::new();
    let started = std::time::Instant::now();
    for page in 0..MAX_MESSAGE_PAGES {
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(45),
            "usage message scan exceeded its time budget"
        );
        let mut path = format!(
            "/api/session/{}/message?limit={PAGE_LIMIT}",
            encode_path_segment(session_id)
        );
        if let Some(token) = &cursor {
            path.push_str(&format!("&cursor={}", encode_path_segment(token)));
        }
        let response = request(&path)
            .map_err(|error| anyhow::anyhow!("usage day split failed for {session_id}: {error}"))?;
        let rows = response
            .pointer("/data")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .ok_or_else(|| anyhow::anyhow!("invalid usage message listing for {session_id}"))?;
        let next = response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .filter(|next| !next.is_empty());
        if let Some(next) = next {
            anyhow::ensure!(
                page_cursors.insert(next.to_owned()),
                "usage message cursor repeated for {session_id}"
            );
        }
        let has_more = next.is_some();
        anyhow::ensure!(
            !rows.is_empty() || !has_more,
            "empty usage message page has a continuation cursor"
        );

        for row in rows {
            let Some(usage) = message_usage(row) else {
                continue;
            };
            if !seen.insert(usage.id.clone()) {
                continue;
            }
            messages.push(usage.clone());
            let day = local_day(usage.timestamp) as u64;
            match index_of_day.get(&day) {
                Some(&index) => {
                    let existing = &mut days[index];
                    existing.direct = existing.direct.saturating_add(usage.direct);
                    existing.total = existing.total.saturating_add(usage.total);
                    existing.input = existing.input.saturating_add(usage.input);
                    existing.output = existing.output.saturating_add(usage.output);
                    existing.cache_read = existing.cache_read.saturating_add(usage.cache_read);
                    existing.cache_write = existing.cache_write.saturating_add(usage.cache_write);
                    existing.cost += usage.cost;
                }
                None => {
                    index_of_day.insert(day, days.len());
                    days.push(UsageDayShare {
                        timestamp: usage.timestamp,
                        subagent: false,
                        direct: usage.direct,
                        total: usage.total,
                        input: usage.input,
                        output: usage.output,
                        cache_read: usage.cache_read,
                        cache_write: usage.cache_write,
                        cost: usage.cost,
                    });
                }
            }
            let hour = usage.timestamp / 3600;
            match index_of_hour.get(&hour) {
                Some(&index) => {
                    let existing = &mut hours[index];
                    existing.input = existing.input.saturating_add(usage.input);
                    existing.output = existing.output.saturating_add(usage.output);
                    existing.cache_read = existing.cache_read.saturating_add(usage.cache_read);
                    existing.cache_write = existing.cache_write.saturating_add(usage.cache_write);
                    existing.cost += usage.cost;
                }
                None => {
                    index_of_hour.insert(hour, hours.len());
                    hours.push(fintwind_protocol::provider_session::UsageHourShare {
                        timestamp: usage.timestamp,
                        estimated: false,
                        input: usage.input,
                        output: usage.output,
                        cache_read: usage.cache_read,
                        cache_write: usage.cache_write,
                        cost: usage.cost,
                    });
                }
            }
            let Some(model) = usage.model else {
                continue;
            };
            let tokens = [
                usage.input,
                usage.output.saturating_sub(usage.reasoning),
                usage.reasoning,
                usage.cache_read,
                usage.cache_write,
            ];
            match index_of_model.get(&model) {
                Some(&index) => {
                    let existing = &mut models[index];
                    existing
                        .requests
                        .push(fintwind_protocol::provider_session::UsageRequest {
                            timestamp: usage.timestamp,
                            tokens,
                        });
                    existing.total = existing.total.saturating_add(usage.total);
                    existing.cost += usage.cost;
                    for (sum, part) in existing.tokens.iter_mut().zip(tokens) {
                        *sum = sum.saturating_add(part);
                    }
                    if let Some(day) = existing
                        .days
                        .iter_mut()
                        .find(|day| local_day(day.timestamp) == local_day(usage.timestamp))
                    {
                        day.direct = day.direct.saturating_add(usage.direct);
                        day.total = day.total.saturating_add(usage.total);
                        day.input = day.input.saturating_add(usage.input);
                        day.output = day.output.saturating_add(usage.output);
                        day.cache_read = day.cache_read.saturating_add(usage.cache_read);
                        day.cache_write = day.cache_write.saturating_add(usage.cache_write);
                        day.cost += usage.cost;
                    } else {
                        existing.days.push(UsageDayShare {
                            timestamp: usage.timestamp,
                            direct: usage.direct,
                            total: usage.total,
                            input: usage.input,
                            output: usage.output,
                            cache_read: usage.cache_read,
                            cache_write: usage.cache_write,
                            cost: usage.cost,
                            ..Default::default()
                        });
                    }
                    if let Some(hour) = existing
                        .hours
                        .iter_mut()
                        .find(|hour| hour.timestamp / 3600 == usage.timestamp / 3600)
                    {
                        hour.input = hour.input.saturating_add(usage.input);
                        hour.output = hour.output.saturating_add(usage.output);
                        hour.cache_read = hour.cache_read.saturating_add(usage.cache_read);
                        hour.cache_write = hour.cache_write.saturating_add(usage.cache_write);
                        hour.cost += usage.cost;
                    } else {
                        existing
                            .hours
                            .push(fintwind_protocol::provider_session::UsageHourShare {
                                timestamp: usage.timestamp,
                                estimated: false,
                                input: usage.input,
                                output: usage.output,
                                cache_read: usage.cache_read,
                                cache_write: usage.cache_write,
                                cost: usage.cost,
                            });
                    }
                }
                None => {
                    index_of_model.insert(model.clone(), models.len());
                    models.push(UsageModelLane {
                        model,
                        total: usage.total,
                        cost: usage.cost,
                        tokens,
                        requests: vec![fintwind_protocol::provider_session::UsageRequest {
                            timestamp: usage.timestamp,
                            tokens,
                        }],
                        days: vec![UsageDayShare {
                            timestamp: usage.timestamp,
                            direct: usage.direct,
                            total: usage.total,
                            input: usage.input,
                            output: usage.output,
                            cache_read: usage.cache_read,
                            cache_write: usage.cache_write,
                            cost: usage.cost,
                            ..Default::default()
                        }],
                        hours: vec![fintwind_protocol::provider_session::UsageHourShare {
                            timestamp: usage.timestamp,
                            estimated: false,
                            input: usage.input,
                            output: usage.output,
                            cache_read: usage.cache_read,
                            cache_write: usage.cache_write,
                            cost: usage.cost,
                        }],
                    });
                }
            }
        }

        if rows.is_empty() || !has_more {
            break;
        }
        if page + 1 == MAX_MESSAGE_PAGES {
            return Err(anyhow::anyhow!(
                "usage detail reached the message page limit for {session_id}"
            ));
        }
        cursor = response
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    days.sort_by_key(|share| share.timestamp);
    hours.sort_by_key(|share| share.timestamp);
    models.sort_by_key(|a| std::cmp::Reverse(a.total));
    Ok(Some(SessionSplit {
        days,
        hours,
        models,
        messages,
    }))
}

/// The message body inside a `/session/:id/message` row. Newer servers wrap
/// each message as `{info, parts}`; the beta fintwind's transcript reader was
/// written against answers flat. Accept both.
fn message_body(row: &Value) -> &Value {
    row.get("info").unwrap_or(row)
}

/// One assistant message's tokens, the model that produced them, and its
/// reported cost. User messages carry none, and an aborted turn's all-zero
/// row would otherwise book a day with no spend, so both are filtered here.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct MessageUsage {
    id: String,
    timestamp: u64,
    direct: u64,
    total: u64,
    input: u64,
    output: u64,
    reasoning: u64,
    cache_read: u64,
    cache_write: u64,
    model: Option<String>,
    cost: f64,
}

fn message_usage(row: &Value) -> Option<MessageUsage> {
    let body = message_body(row);
    // `type` on the flat shape, `role` inside the info envelope.
    if body
        .get("type")
        .or_else(|| body.get("role"))
        .and_then(Value::as_str)
        .is_some_and(|role| role != "assistant")
    {
        return None;
    }
    let timestamp = ms_to_seconds(body.pointer("/time/created"))?;
    let empty = Value::Null;
    let tokens = body.get("tokens").unwrap_or(&empty);
    let lane = |pointer: &str| tokens.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let (input, output, reasoning) = (lane("/input"), lane("/output"), lane("/reasoning"));
    let (cache_read, cache_write) = (lane("/cache/read"), lane("/cache/write"));
    let total = input
        .saturating_add(output)
        .saturating_add(reasoning)
        .saturating_add(cache_read)
        .saturating_add(cache_write);
    if total == 0 {
        return None;
    }
    Some(MessageUsage {
        id: body.get("id").and_then(Value::as_str)?.to_owned(),
        timestamp,
        direct: input.saturating_add(output).saturating_add(reasoning),
        total,
        input,
        output: output.saturating_add(reasoning),
        reasoning,
        cache_read,
        cache_write,
        // The message's own model, not the session's: a session that switched
        // models has both in its stream, and the session row names only the
        // model it ended on.
        model: message_model(body),
        cost: body.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

/// `<providerID>/<modelID>` for one message. The two shapes key the id
/// differently — the flat row nests it under `model`, the info envelope may
/// use either spelling — so both are tried before giving up.
fn message_model(body: &Value) -> Option<String> {
    let model = body.get("model")?;
    let provider = model.get("providerID").and_then(Value::as_str)?;
    let id = model
        .get("modelID")
        .or_else(|| model.get("id"))
        .and_then(Value::as_str)?;
    Some(format!("{provider}/{id}"))
}

/// The local calendar day a unix second falls on. Shared with the client so
/// the scan and the aggregation cannot drift apart on DST.
fn local_day(unix_seconds: u64) -> i64 {
    fintwind_protocol::model::local_day(unix_seconds)
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A sub-agent books its tokens into its own row and the parent's
    /// aggregate does not include them, so the fold is the only thing that
    /// makes the parent's total honest. Measured on a real store this was
    /// worth 8.6% of the whole total.
    #[test]
    fn a_subagents_usage_folds_into_its_parent() {
        let mut parent = UsageEntry {
            timestamp: 10,
            model: Some("p/m".to_owned()),
            directory: None,
            cost: Some(0.5),
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 5,
            cache_read_tokens: 1_000,
            cache_write_tokens: 50,
            subagent_sessions: 0,
            subagent_tokens: 0,
            subagent_direct: 0,
            days: None,
            hours: Vec::new(),
            model_lanes: Vec::new(),
            subagent_models: Vec::new(),
        };
        let child = UsageEntry {
            timestamp: 11,
            model: Some("p/m".to_owned()),
            directory: None,
            cost: Some(0.5),
            input_tokens: 7,
            output_tokens: 3,
            reasoning_tokens: 1,
            cache_read_tokens: 40,
            cache_write_tokens: 2,
            subagent_sessions: 0,
            subagent_tokens: 0,
            subagent_direct: 0,
            days: None,
            hours: Vec::new(),
            model_lanes: Vec::new(),
            subagent_models: Vec::new(),
        };

        fold_subagent_into(&mut parent, &child, "child");

        // Every lane moved, so the breakdown agrees with the total.
        assert_eq!(parent.total_tokens(), 1_175 + 53);
        assert_eq!(parent.input_tokens, 107);
        assert_eq!(parent.output_tokens, 23);
        assert_eq!(parent.cache_read_tokens, 1_040);
        assert_eq!(parent.cost, Some(1.0));
        // The subtitle numbers describe the folded-in part only, so the page
        // can say how much of the total came from sub-agents.
        assert_eq!(parent.subagent_sessions, 1);
        assert_eq!(parent.subagent_tokens, 53);
        // Split the same way the day chart splits it: non-cache first, then
        // the whole amount, so the timeline cannot push cache through a
        // channel labelled "excludes cache".
        assert_eq!(parent.subagent_direct, 11);
        // The parent keeps its own timestamp and model: the fold moves tokens,
        // not identity.
        assert_eq!(parent.timestamp, 10);
    }

    #[test]
    fn usage_entry_parses_the_session_row_shape() {
        let row = json!({
            "id": "ses_1", "title": "迁移会话",
            "time": {"created": 1_788_253_280_101_u64, "updated": 1_788_253_300_500_u64},
            "model": {"id": "glm-5.3", "providerID": "glmcoding"},
            "tokens": {"input": 130, "output": 90, "reasoning": 7, "cache": {"read": 1_000, "write": 10}},
            "cost": 1.25,
            "location": {"directory": "E:\\work\\x"}
        });
        let (_, facts) = row_facts(&row).unwrap();
        let entry = usage_entry_from_row(&facts).unwrap();
        assert_eq!(entry.timestamp, 1_788_253_300);
        assert_eq!(facts.model.as_deref(), Some("glmcoding/glm-5.3"));
        assert_eq!(entry.model, None);
        assert_eq!(entry.directory.as_deref(), Some("E:\\work\\x"));
        assert_eq!(entry.cost, Some(1.25));
        assert_eq!(entry.input_tokens, 130);
        assert_eq!(entry.output_tokens, 90);
        assert_eq!(entry.reasoning_tokens, 7);
        assert_eq!(entry.cache_read_tokens, 1_000);
        assert_eq!(entry.cache_write_tokens, 10);
        assert_eq!(entry.total_tokens(), 1_237);

        // A reported zero cost stays a reported zero: "the provider chose not
        // to charge" is an answer, and the page counts how many sessions
        // answered at all rather than filtering the zero away here.
        let free = json!({
            "id": "ses_2", "time": {"updated": 2_000_u64},
            "tokens": {"input": 5, "output": 1}, "cost": 0
        });
        let (_, facts) = row_facts(&free).unwrap();
        let entry = usage_entry_from_row(&facts).unwrap();
        assert_eq!(entry.cost, Some(0.0));
        assert_eq!(entry.timestamp, 2);
        assert_eq!(entry.total_tokens(), 6);

        // A session that never reported tokens still counts as a session,
        // just with zero tokens.
        let tokenless = json!({"id": "ses_4", "time": {"updated": 3_000_u64}});
        let (_, facts) = row_facts(&tokenless).unwrap();
        let entry = usage_entry_from_row(&facts).unwrap();
        assert_eq!(entry.timestamp, 3);
        assert_eq!(entry.total_tokens(), 0);

        // A timeless row cannot land on a day and folds out here.
        let timeless = json!({"id": "ses_3", "tokens": {"input": 1}});
        assert!(row_facts(&timeless).is_none());
    }



    /// A fork inherits the parent's history, so its session row carries the
    /// parent's tokens *and* `forkSessionID`; a sub-agent's tokens are its own
    /// and its parent's aggregate does not include them. Reading only
    /// `parentID` conflates the two: discarding a sub-agent as a fork drops
    /// real spend.
    #[test]
    fn session_kind_tells_a_fork_from_a_subagent() {
        let parent = json!({"id": "ses_p", "time": {"updated": 1}});
        let fork = json!({
            "id": "ses_f", "forkSessionID": "ses_p", "time": {"updated": 2}
        });
        let subagent = json!({
            "id": "ses_c", "parentID": "ses_p", "time": {"updated": 3}
        });

        assert_eq!(session_kind(&parent), SessionKind::TopLevel);
        assert_eq!(session_kind(&fork), SessionKind::Fork);
        assert_eq!(session_kind(&subagent), SessionKind::SubAgent);

        // Both are excluded from the sidebar's linear-conversation list.
        assert!(is_child_session(&fork));
        assert!(is_child_session(&subagent));
        assert!(!is_child_session(&parent));

        // Only a sub-agent folds into a parent: a fork's tokens already
        // belong to the parent's history, and folding it would count the same
        // messages twice.
        assert_eq!(session_kind(&subagent), SessionKind::SubAgent);
        assert_ne!(session_kind(&fork), SessionKind::SubAgent);
    }
}
