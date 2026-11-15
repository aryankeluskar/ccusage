use std::{collections::HashSet, path::Path};

use crate::{LoadedEntry, PricingMap, Result, cli::SharedArgs, read_files_parallel};

use super::{
    parser::{DevinEntry, RawMessage, parse_message_row, to_loaded_entry},
    paths::sessions_db_paths,
};

pub fn load_entries(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(
        crate::progress::UsageLoadAgent("Devin"),
        shared.json,
        || load_entries_inner(shared, pricing),
    )
}

fn load_entries_inner(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    let tz = crate::parse_tz(shared.timezone.as_deref());
    let db_paths = sessions_db_paths()?;
    // Load each sessions database in parallel (a fresh read-only connection per
    // DB), then run the sequential first-wins dedup over the original path order
    // so the surviving message matches the single-threaded read.
    let loaded = read_files_parallel(&db_paths, shared.single_thread, |db_path| {
        load_sessions_db_entries(db_path, shared)
    });
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for db_entries in loaded {
        for entry in db_entries {
            if !seen.insert((entry.session_id.clone(), entry.dedupe_key())) {
                continue;
            }
            entries.push(to_loaded_entry(entry, tz.as_ref(), pricing));
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

fn load_sessions_db_entries(db_path: &Path, shared: &SharedArgs) -> Vec<DevinEntry> {
    let connection = match sqlite::Connection::open_with_flags(
        db_path,
        sqlite::OpenFlags::new().with_read_only(),
    ) {
        Ok(connection) => connection,
        Err(error) => {
            crate::debug_log(
                shared,
                format!(
                    "Failed to open Devin sessions database: {}: {error}",
                    db_path.display()
                ),
            );
            return Vec::new();
        }
    };
    let mut statement = match connection.prepare(
        // The workspace bundles SQLite without JSON functions (`.cargo/config.toml`
        // sets `SQLITE_OMIT_JSON`), so metrics cannot be projected in SQL. The LIKE
        // prefilter keeps the scan and the row text inside SQLite; only plausibly-
        // assistant rows cross into Rust for parsing. The parser re-checks the
        // role, because user messages can quote assistant messages.
        "
            SELECT
                nodes.session_id,
                nodes.row_id,
                nodes.created_at,
                nodes.chat_message,
                sessions.model,
                sessions.working_directory
            FROM message_nodes nodes
            JOIN sessions ON sessions.id = nodes.session_id
            WHERE nodes.chat_message LIKE '%\"role\":\"assistant\"%'
            ORDER BY nodes.row_id
        ",
    ) {
        Ok(statement) => statement,
        Err(error) => {
            crate::debug_log(
                shared,
                format!(
                    "Failed to read Devin sessions database: {}: {error}",
                    db_path.display()
                ),
            );
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    loop {
        match statement.next() {
            Ok(sqlite::State::Row) => {
                if let Some(entry) = parse_message_row(read_raw_message(&statement)) {
                    entries.push(entry);
                }
            }
            Ok(sqlite::State::Done) => break,
            Err(error) => {
                crate::debug_log(
                    shared,
                    format!(
                        "Failed to query Devin sessions database: {}: {error}",
                        db_path.display()
                    ),
                );
                break;
            }
        }
    }
    entries
}

fn read_raw_message(statement: &sqlite::Statement<'_>) -> RawMessage {
    RawMessage {
        session_id: statement.read::<String, _>(0).unwrap_or_default(),
        row_id: statement.read::<i64, _>(1).unwrap_or(0),
        created_at: read_created_at(statement),
        chat_message: statement.read::<String, _>(3).unwrap_or_default(),
        session_model: statement.read::<String, _>(4).ok(),
        working_directory: statement.read::<String, _>(5).ok(),
    }
}

fn read_created_at(statement: &sqlite::Statement<'_>) -> Option<f64> {
    statement
        .read::<f64, _>(2)
        .ok()
        .filter(|value| value.is_finite())
        .or_else(|| statement.read::<i64, _>(2).ok().map(|value| value as f64))
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::Path};

    use super::*;
    use crate::{cli::CostMode, paths};
    use ccusage_test_support::{EnvVarsGuard, fs_fixture};
    use serde_json::json;

    /// Sets the given variables, clears the other Devin discovery variables,
    /// and restores everything on drop.
    fn isolate(vars: &[(&'static str, Option<OsString>)]) -> EnvVarsGuard {
        let mut all: Vec<(&'static str, Option<OsString>)> = vec![
            (paths::DEVIN_DATA_DIR_ENV, None),
            (paths::DEVIN_SESSIONS_DIR_ENV, None),
        ];
        all.extend_from_slice(vars);
        EnvVarsGuard::set_many(all)
    }

    fn create_sessions_db(path: &Path) -> sqlite::Connection {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let db = sqlite::open(path).unwrap();
        db.execute(
            "
                CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    model TEXT,
                    working_directory TEXT
                );
                CREATE TABLE message_nodes (
                    row_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    chat_message TEXT NOT NULL
                );
            ",
        )
        .unwrap();
        db
    }

    fn insert_message(db: &sqlite::Connection, session: &str, created_at: i64, chat_message: &str) {
        let mut statement = db
            .prepare(
                "
                INSERT INTO message_nodes (session_id, created_at, chat_message)
                VALUES (?1, ?2, ?3)
            ",
            )
            .unwrap();
        statement.bind((1, session)).unwrap();
        statement.bind((2, created_at)).unwrap();
        statement.bind((3, chat_message)).unwrap();
        statement.next().unwrap();
    }

    fn assistant_message(
        request_id: Option<&str>,
        model: Option<&str>,
        tokens: [i64; 4],
    ) -> String {
        // The Devin CLI writes compact JSON, and the loader's LIKE prefilter
        // matches the compact form, so fixtures must be compact too.
        json!({
            "message_id": "m-1",
            "role": "assistant",
            "content": "done",
            "metadata": {
                "request_id": request_id,
                "generation_model": model,
                "metrics": {
                    "input_tokens": tokens[0],
                    "output_tokens": tokens[1],
                    "cache_read_tokens": tokens[2],
                    "cache_creation_tokens": tokens[3],
                },
            },
        })
        .to_string()
    }

    fn insert_session(db: &sqlite::Connection, id: &str, model: &str, directory: &str) {
        let mut statement = db
            .prepare(
                "
                INSERT INTO sessions (id, model, working_directory)
                VALUES (?1, ?2, ?3)
            ",
            )
            .unwrap();
        statement.bind((1, id)).unwrap();
        statement.bind((2, model)).unwrap();
        statement.bind((3, directory)).unwrap();
        statement.next().unwrap();
    }

    #[test]
    fn counts_each_request_once_across_rebuilt_context_nodes() {
        let fixture = fs_fixture!({});
        let db_path = fixture.path("cli/sessions.db");
        let db = create_sessions_db(&db_path);
        insert_session(&db, "oval-risk", "gpt-6-astra-medium", "/home/dev/my-app");
        let original = assistant_message(
            Some("request-1"),
            Some("gpt-6-astra-medium"),
            [2, 100, 417_684, 4_839],
        );
        let rebuilt = assistant_message(
            Some("request-1"),
            Some("gpt-6-astra-medium"),
            [2, 100, 417_684, 4_839],
        );
        let followup = assistant_message(
            Some("request-2"),
            Some("gpt-6-astra-medium"),
            [2, 50, 417_684, 4_839],
        );
        insert_message(&db, "oval-risk", 1_763_000_000, &original);
        insert_message(&db, "oval-risk", 1_763_000_010, &followup);
        insert_message(&db, "oval-risk", 1_763_000_020, &rebuilt);

        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            mode: CostMode::Calculate,
            ..SharedArgs::default()
        };
        let _env = isolate(&[(
            paths::DEVIN_DATA_DIR_ENV,
            Some(fixture.root().as_os_str().to_owned()),
        )]);
        let pricing = PricingMap::load_embedded();
        let entries = load_entries(&shared, &pricing).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].session_id.as_ref(), "oval-risk");
        assert_eq!(entries[0].model.as_deref(), Some("gpt-6-astra-medium"));
        assert_eq!(entries[0].project_path.as_ref(), "my-app");
        assert_eq!(entries[0].data.message.usage.input_tokens, 2);
        assert_eq!(entries[0].data.message.usage.output_tokens, 100);
        assert_eq!(
            entries[0].data.message.usage.cache_read_input_tokens,
            417_684
        );
        assert_eq!(
            entries[0].data.message.usage.cache_creation_input_tokens,
            4_839
        );
        // The earliest node wins, so the rebuilt duplicate is dropped.
        assert_eq!(entries[1].data.message.usage.output_tokens, 50);
        assert_eq!(entries[0].timestamp.as_millis(), 1_763_000_000_000);
        assert_eq!(entries[1].timestamp.as_millis(), 1_763_000_010_000);
    }

    #[test]
    fn keeps_messages_without_a_request_id() {
        let fixture = fs_fixture!({});
        let db_path = fixture.path("sessions.db");
        let db = create_sessions_db(&db_path);
        insert_session(&db, "oval-risk", "gpt-6-astra-medium", "/home/dev/my-app");
        insert_message(
            &db,
            "oval-risk",
            1_763_000_000,
            &assistant_message(None, Some("gpt-6-astra-medium"), [10, 20, 0, 0]),
        );
        insert_message(
            &db,
            "oval-risk",
            1_763_000_001,
            &assistant_message(None, Some("gpt-6-astra-medium"), [30, 40, 0, 0]),
        );

        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        };
        let _env = isolate(&[(
            paths::DEVIN_SESSIONS_DIR_ENV,
            Some(fixture.root().as_os_str().to_owned()),
        )]);
        let pricing = PricingMap::load_embedded();
        let entries = load_entries(&shared, &pricing).unwrap();

        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn falls_back_to_the_session_model() {
        let fixture = fs_fixture!({});
        let db_path = fixture.path("cli/sessions.db");
        let db = create_sessions_db(&db_path);
        insert_session(
            &db,
            "oval-risk",
            "claude-sonnet-5-5-medium",
            "/home/dev/my-app",
        );
        insert_message(
            &db,
            "oval-risk",
            1_763_000_000,
            &assistant_message(Some("request-1"), None, [10, 20, 0, 0]),
        );

        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        };
        let _env = isolate(&[(
            paths::DEVIN_DATA_DIR_ENV,
            Some(fixture.root().as_os_str().to_owned()),
        )]);
        let pricing = PricingMap::load_embedded();
        let entries = load_entries(&shared, &pricing).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].model.as_deref(),
            Some("claude-sonnet-5-5-medium")
        );
    }

    #[test]
    fn skips_rows_without_usage_metrics() {
        let fixture = fs_fixture!({});
        let db_path = fixture.path("cli/sessions.db");
        let db = create_sessions_db(&db_path);
        insert_session(&db, "oval-risk", "gpt-6-astra-medium", "/home/dev/my-app");
        insert_message(
            &db,
            "oval-risk",
            1_763_000_000,
            r#"{"role":"user","content":"hi"}"#,
        );
        insert_message(
            &db,
            "oval-risk",
            1_763_000_001,
            r#"{"role":"assistant","content":"no metrics","metadata":{}}"#,
        );
        // A user message quoting an assistant message passes the loader's
        // LIKE prefilter and must be rejected by the parser.
        insert_message(
            &db,
            "oval-risk",
            1_763_000_002,
            r#"{"role":"user","content":"earlier you said {\"role\":\"assistant\"}"}"#,
        );

        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        };
        let _env = isolate(&[(
            paths::DEVIN_DATA_DIR_ENV,
            Some(fixture.root().as_os_str().to_owned()),
        )]);
        let pricing = PricingMap::load_embedded();
        let entries = load_entries(&shared, &pricing).unwrap();

        assert!(entries.is_empty());
    }

    #[test]
    fn returns_empty_for_a_database_with_unrelated_tables() {
        let fixture = fs_fixture!({});
        let db_path = fixture.path("cli/sessions.db");
        std::fs::create_dir_all(fixture.path("cli")).unwrap();
        let db = sqlite::open(&db_path).unwrap();
        db.execute("CREATE TABLE other (id TEXT);").unwrap();

        let shared = SharedArgs::default();
        let _env = isolate(&[(
            paths::DEVIN_DATA_DIR_ENV,
            Some(fixture.root().as_os_str().to_owned()),
        )]);
        let pricing = PricingMap::load_embedded();
        let entries = load_entries(&shared, &pricing).unwrap();

        assert!(entries.is_empty());
    }

    #[test]
    #[ignore = "requires real Devin CLI sessions database"]
    fn smoke_real_devin_data_dir_loads_without_error() {
        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        };
        let pricing = PricingMap::load_embedded();
        let entries = load_entries(&shared, &pricing).unwrap();
        assert!(!entries.is_empty());
    }
}
