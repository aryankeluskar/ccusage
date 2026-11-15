# ccusage-adapter-devin

The Devin adapter: it turns the Devin CLI sessions database into the usage entries the reports render.

## Owns

- `loader.rs` — reading the source, request-id dedup, and date filtering entry points.
- `parser.rs` — raw record parsing, token mapping, and model naming.
- `paths.rs` — environment variables, default directories, and file discovery.
- `report.rs` — the JSON and table shapes where they differ from the shared ones.

Anything that is not specific to this source belongs in `ccusage-core` or `ccusage-adapter-common` instead.

## Data source

- `~/.local/share/devin/cli/sessions.db` (macOS, Linux), `%APPDATA%\devin\cli\sessions.db` (Windows)
- `DEVIN_SESSIONS_DIR` overrides the directory holding `sessions.db`
- `DEVIN_DATA_DIR` overrides the data root that holds `cli/sessions.db`

Reads SQLite with the bundled `sqlite` crate, which is why this crate declares it and most adapters do not. The workspace compiles that bundled SQLite without JSON functions (`.cargo/config.toml` sets `SQLITE_OMIT_JSON`), so the loader cannot project the metrics with `json_extract`: it prefilters assistant rows with a `LIKE` on `chat_message` — the scan and the non-matching row text stay inside SQLite — and parses each candidate message with `serde_json`. The parser re-checks the role, because a user message can quote an assistant message and pass the prefilter.

Devin stores the whole conversation forest in `message_nodes`: when it rebuilds context, it re-saves earlier messages as new nodes, so the same model response appears under several node ids. Every response carries its API `request_id`, and duplicated nodes share it, so the loader keeps the first node per `(session_id, request_id)` and counts each response once. Responses without a `request_id` are kept as-is.

Token buckets map one-to-one onto the shared shape: `input_tokens` is the uncached input, `cache_read_tokens` and `cache_creation_tokens` are the cached input, and `output_tokens` includes reasoning.

Costs are never recorded locally (the `total_acu_cost` and `total_credit_cost` session fields stay `0`; billing lives on the Devin account), so costs come from the pricing map. Devin appends a thinking-effort suffix (`-low`, `-medium`, `-high`, `-xhigh`, `-max`) to model names, so the pricing lookup also tries the name without the suffix.

## Public surface

- `loader::load_entries`
- `report::report_from_rows`
- `report::summarize_entries`
- `run`

## Depends on

- `ccusage-adapter-common`
- `ccusage-core`
- `jiff`
- `sqlite`

## Build layer

Built in the `adapters` Crane artifact layer; the layer compiles all adapters in one Cargo invocation, so they build concurrently.
