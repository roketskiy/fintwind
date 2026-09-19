use chrono::Datelike as _;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanUsage {
    pub plan_label: Option<String>,
    pub windows: Vec<PlanWindow>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanWindow {
    pub label: String,
    pub percent: f64,
    pub resets_at: Option<i64>,
}

pub fn format_tokens(tokens: u64) -> String {
    if tokens >= 999_500 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Cache-hit and occupancy fractions, always with one decimal place so a
/// 96.0% session is distinguishable from a rounded 100%.
pub fn format_percent(percent: f64) -> String {
    format!("{:.1}%", percent)
}

/// Session cache hit rate: cached prompt tokens over all input tokens.
pub fn cache_hit_percent(cache_read: u64, prompt_tokens: u64) -> Option<f64> {
    (prompt_tokens > 0).then(|| cache_read.min(prompt_tokens) as f64 * 100.0 / prompt_tokens as f64)
}

pub fn reset_label(resets_at: i64, now: i64) -> String {
    let delta = resets_at - now;
    if delta <= 0 {
        return tr!("usage.resets_soon");
    }
    let minutes = (delta + 59) / 60;
    if minutes < 60 {
        return tr!("usage.resets_in_minutes", count = minutes);
    }
    if minutes < 24 * 60 {
        let hours = minutes / 60;
        return match minutes % 60 {
            0 => tr!("usage.resets_in_hours", count = hours),
            remainder => tr!(
                "usage.resets_in_hours_minutes",
                hours = hours,
                minutes = remainder
            ),
        };
    }
    use chrono::TimeZone as _;
    match chrono::Local.timestamp_opt(resets_at, 0) {
        chrono::LocalResult::Single(date) if crate::i18n::uses_east_asian_date_format() => tr!(
            "usage.resets_date",
            date = format!(
                "{}月{}日 {}",
                date.month(),
                date.day(),
                date.format("%H:%M")
            )
        ),
        chrono::LocalResult::Single(date) => tr!(
            "usage.resets_date",
            date = date.format("%a %-I:%M %p").to_string()
        ),
        _ => tr!("usage.resets_soon"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_rate_is_cached_over_all_input() {
        assert_eq!(
            format_percent(cache_hit_percent(10_741_120, 11_266_541).unwrap()),
            "95.3%"
        );
        assert_eq!(cache_hit_percent(0, 0), None);
        assert_eq!(cache_hit_percent(100, 50), Some(100.0));
    }
}
