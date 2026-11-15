use std::{collections::HashSet, sync::Arc};

use jiff::tz::TimeZone as JiffTimeZone;
use serde_json::Value;

use crate::{
    LoadedEntry, PricingMap, TimestampMs, TokenUsageRaw, UsageEntry, UsageMessage,
    calculate_cost_for_usage_at, cli::CostMode, format_date_tz, format_rfc3339_millis,
    missing_pricing_model_for_candidates,
};

/// Thinking-effort suffixes the Devin CLI appends to model names, from its
/// thinking levels. Stripping them lets the pricing lookup find the base model.
const EFFORT_SUFFIXES: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

pub(super) struct DevinEntry {
    pub(super) session_id: String,
    node_id: u64,
    request_id: Option<String>,
    timestamp: TimestampMs,
    timestamp_text: String,
    model: String,
    usage: TokenUsageRaw,
    project_path: String,
}

impl DevinEntry {
    /// The key the loader dedupes on: the response's API request id when the
    /// node carries one, otherwise the node itself.
    pub(super) fn dedupe_key(&self) -> String {
        self.request_id
            .clone()
            .unwrap_or_else(|| format!("node:{}", self.node_id))
    }

    fn message_id(&self) -> String {
        format!("devin:{}:{}", self.session_id, self.dedupe_key())
    }
}

/// One candidate row from the sessions database, before parsing. The loader
/// prefilters with SQL so only plausibly-assistant rows get here.
pub(super) struct RawMessage {
    pub(super) session_id: String,
    pub(super) row_id: i64,
    pub(super) created_at: Option<f64>,
    pub(super) chat_message: String,
    pub(super) session_model: Option<String>,
    pub(super) working_directory: Option<String>,
}

/// Parses one assistant chat message into an entry, skipping rows without
/// usable usage metrics. The workspace bundles SQLite without JSON functions
/// (`.cargo/config.toml` sets `SQLITE_OMIT_JSON`), so metrics are parsed here
/// rather than extracted in SQL.
pub(super) fn parse_message_row(raw: RawMessage) -> Option<DevinEntry> {
    if raw.session_id.is_empty() {
        return None;
    }
    let message: Value = serde_json::from_str(&raw.chat_message).ok()?;
    // The SQL prefilter matches any message containing `"role":"assistant"`,
    // including user messages that quote one, so the role check stays here.
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let metadata = message.get("metadata");
    let metrics = metadata.and_then(|metadata| metadata.get("metrics"))?;
    let usage = TokenUsageRaw {
        input_tokens: read_tokens(metrics, "input_tokens"),
        output_tokens: read_tokens(metrics, "output_tokens"),
        cache_creation_input_tokens: read_tokens(metrics, "cache_creation_tokens"),
        cache_read_input_tokens: read_tokens(metrics, "cache_read_tokens"),
        speed: None,
        cache_creation: None,
    };
    if usage.input_tokens == 0
        && usage.output_tokens == 0
        && usage.cache_read_input_tokens == 0
        && usage.cache_creation_input_tokens == 0
    {
        return None;
    }
    let created_at = raw.created_at.filter(|value| value.is_finite())?;
    let timestamp = timestamp_from_number(created_at)?;
    let request_id = read_non_empty(metadata, "request_id");
    let generation_model = read_non_empty(metadata, "generation_model");
    let session_model = raw
        .session_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string);
    let model = generation_model
        .or(session_model)
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())?;
    Some(DevinEntry {
        session_id: raw.session_id,
        node_id: raw.row_id.max(0) as u64,
        request_id,
        timestamp_text: format_rfc3339_millis(timestamp),
        timestamp,
        model,
        usage,
        project_path: project_path_from_directory(raw.working_directory.as_deref()),
    })
}

fn read_tokens(metrics: &Value, key: &str) -> u64 {
    metrics.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn read_non_empty(metadata: Option<&Value>, key: &str) -> Option<String> {
    metadata
        .and_then(|metadata| metadata.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn timestamp_from_number(value: f64) -> Option<TimestampMs> {
    if !value.is_finite() {
        return None;
    }
    let millis = if value > 1e12 { value } else { value * 1000.0 };
    (millis > 0.0).then(|| TimestampMs::from_millis(millis.trunc() as i64))
}

fn project_path_from_directory(working_directory: Option<&str>) -> String {
    working_directory
        .and_then(|directory| {
            let trimmed = directory.trim_end_matches(['/', '\\']);
            trimmed
                .rsplit(['/', '\\'])
                .next()
                .filter(|base| !base.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Devin".to_string())
}

/// Strips a trailing thinking-effort suffix the Devin CLI appends to model
/// names, so `claude-opus-5-5-medium` prices as `claude-opus-5-5`.
fn strip_effort_suffix(model: &str) -> Option<&str> {
    let (base, suffix) = model.rsplit_once('-')?;
    EFFORT_SUFFIXES
        .contains(&suffix)
        .then(|| &model[..base.len()])
}

fn model_candidates(model: &str) -> Vec<String> {
    let mut candidates = vec![model.to_string()];
    if let Some(base) = strip_effort_suffix(model) {
        candidates.push(base.to_string());
    }
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}

pub(super) fn to_loaded_entry(
    entry: DevinEntry,
    tz: Option<&JiffTimeZone>,
    pricing: &PricingMap,
) -> LoadedEntry {
    let cost = calculate_devin_cost(&entry, pricing);
    let missing_pricing_model = missing_devin_pricing(&entry, pricing);
    let data = UsageEntry {
        session_id: Some(entry.session_id.clone()),
        timestamp: entry.timestamp_text.clone(),
        version: None,
        message: UsageMessage {
            usage: entry.usage,
            model: Some(entry.model.clone()),
            id: Some(entry.message_id()),
        },
        cost_usd: None,
        request_id: entry.request_id.clone(),
        is_api_error_message: None,
        is_sidechain: None,
    };
    LoadedEntry {
        date: format_date_tz(entry.timestamp, tz),
        timestamp: entry.timestamp,
        project: Arc::from("devin"),
        session_id: Arc::from(entry.session_id.as_str()),
        project_path: Arc::from(entry.project_path.as_str()),
        cost,
        credits: None,
        extra_total_tokens: 0,
        message_count: None,
        model: Some(entry.model),
        usage_limit_reset_time: None,
        missing_pricing_model,
        stored_cost: None,
        data,
    }
}

fn calculate_devin_cost(entry: &DevinEntry, pricing: &PricingMap) -> f64 {
    for candidate in model_candidates(&entry.model) {
        let cost = calculate_cost_for_usage_at(
            Some(&candidate),
            entry.usage,
            None,
            Some(entry.timestamp),
            CostMode::Calculate,
            Some(pricing),
        );
        if cost.is_finite() && cost > 0.0 {
            return cost;
        }
    }
    0.0
}

fn missing_devin_pricing(entry: &DevinEntry, pricing: &PricingMap) -> Option<String> {
    missing_pricing_model_for_candidates(
        &entry.model,
        model_candidates(&entry.model),
        crate::total_usage_tokens(entry.usage),
        Some(pricing),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(chat_message: &str) -> RawMessage {
        RawMessage {
            session_id: "session-1".to_string(),
            row_id: 7,
            created_at: Some(1_763_000_000.0),
            chat_message: chat_message.to_string(),
            session_model: Some("gpt-6-astra-medium".to_string()),
            working_directory: Some("/Users/dev/my-app".to_string()),
        }
    }

    #[test]
    fn parses_metrics_from_an_assistant_message() {
        let entry = parse_message_row(raw(
            r#"{"message_id":"m-1","role":"assistant","content":"done","metadata":{"request_id":"req-1","generation_model":"claude-opus-5-5-medium","metrics":{"input_tokens":2,"output_tokens":100,"cache_read_tokens":417,"cache_creation_tokens":48}}}"#,
        ))
        .unwrap();

        assert_eq!(entry.session_id, "session-1");
        assert_eq!(entry.request_id.as_deref(), Some("req-1"));
        assert_eq!(entry.model, "claude-opus-5-5-medium");
        assert_eq!(entry.usage.input_tokens, 2);
        assert_eq!(entry.usage.output_tokens, 100);
        assert_eq!(entry.usage.cache_read_input_tokens, 417);
        assert_eq!(entry.usage.cache_creation_input_tokens, 48);
        assert_eq!(entry.project_path, "my-app");
        assert_eq!(entry.dedupe_key(), "req-1");
        assert_eq!(entry.timestamp.as_millis(), 1_763_000_000_000);
    }

    #[test]
    fn falls_back_to_the_node_when_the_request_id_is_missing() {
        let entry = parse_message_row(raw(
            r#"{"role":"assistant","metadata":{"generation_model":"gpt-6-astra-medium","metrics":{"input_tokens":2,"output_tokens":100,"cache_read_tokens":4,"cache_creation_tokens":8}}}"#,
        ))
        .unwrap();

        assert_eq!(entry.request_id, None);
        assert_eq!(entry.dedupe_key(), "node:7");
    }

    #[test]
    fn skips_rows_that_are_not_usable_usage_records() {
        // A user message quoting an assistant message passes the loader's
        // LIKE prefilter, so the role check here must reject it.
        let quoting_user_message =
            r#"{"role":"user","content":"earlier you said {\"role\":\"assistant\"}"}"#;
        assert!(parse_message_row(raw(quoting_user_message)).is_none());

        let no_metrics = r#"{"role":"assistant","content":"done","metadata":{}}"#;
        assert!(parse_message_row(raw(no_metrics)).is_none());

        let zero_metrics = r#"{"role":"assistant","metadata":{"metrics":{"input_tokens":0,"output_tokens":0,"cache_read_tokens":0,"cache_creation_tokens":0}}}"#;
        assert!(parse_message_row(raw(zero_metrics)).is_none());

        let malformed = "not json";
        assert!(parse_message_row(raw(malformed)).is_none());
    }

    fn entry(model: &str) -> DevinEntry {
        DevinEntry {
            session_id: "session-1".to_string(),
            node_id: 1,
            request_id: Some("request-1".to_string()),
            timestamp: crate::parse_ts_timestamp("2026-11-14T15:06:40.000Z").unwrap(),
            timestamp_text: "2026-11-14T15:06:40.000Z".to_string(),
            model: model.to_string(),
            usage: TokenUsageRaw {
                input_tokens: 1_000,
                output_tokens: 100,
                cache_creation_input_tokens: 20,
                cache_read_input_tokens: 50,
                speed: None,
                cache_creation: None,
            },
            project_path: "Devin".to_string(),
        }
    }

    #[test]
    fn strips_known_effort_suffixes_only() {
        assert_eq!(
            strip_effort_suffix("claude-opus-5-5-medium"),
            Some("claude-opus-5-5")
        );
        assert_eq!(strip_effort_suffix("gpt-6-astra-high"), Some("gpt-6-astra"));
        assert_eq!(
            strip_effort_suffix("claude-fable-5-2-xhigh"),
            Some("claude-fable-5-2")
        );
        assert_eq!(strip_effort_suffix("compactor"), None);
        assert_eq!(strip_effort_suffix("swe-1-6-fast"), None);
    }

    #[test]
    fn offers_base_model_alongside_the_recorded_name() {
        assert_eq!(
            model_candidates("claude-opus-5-5-medium"),
            vec!["claude-opus-5-5-medium", "claude-opus-5-5"]
        );
        assert_eq!(model_candidates("compactor"), vec!["compactor"]);
    }

    #[test]
    fn prices_effort_suffixed_models_from_embedded_pricing() {
        let pricing = PricingMap::load_embedded();

        for model in ["claude-opus-5-5-medium", "gpt-6-astra-medium"] {
            assert!(
                calculate_devin_cost(&entry(model), &pricing) > 0.0,
                "{model} should resolve to embedded pricing"
            );
        }
    }

    #[test]
    fn reports_missing_pricing_for_unknown_models() {
        let pricing = PricingMap::load_embedded();

        assert!(missing_devin_pricing(&entry("compactor"), &pricing).is_some());
        assert!(missing_devin_pricing(&entry("gpt-6-astra-medium"), &pricing).is_none());
    }

    #[test]
    fn derives_project_path_from_the_working_directory() {
        assert_eq!(
            project_path_from_directory(Some("/Users/dev/Developer/my-app")),
            "my-app"
        );
        assert_eq!(
            project_path_from_directory(Some("C:\\dev\\my-app\\")),
            "my-app"
        );
        assert_eq!(project_path_from_directory(Some("/")), "Devin");
        assert_eq!(project_path_from_directory(None), "Devin");
    }
}
