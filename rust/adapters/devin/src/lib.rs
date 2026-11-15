use ccusage_adapter_common::{
    filter_loaded_entries_by_date, filter_session_summaries_by_last_activity, read_files_parallel,
};
use ccusage_core::*;

mod loader;
mod parser;
mod paths;
mod report;

use crate::{
    PricingMap, Result,
    cli::{AgentCommandArgs, AgentReportKind},
    print_json_or_jq, print_usage_table, sort_summaries, wants_json,
};

pub use loader::load_entries;
pub(crate) use report::report_from_rows;
pub use report::summarize_entries;

pub fn run(args: AgentCommandArgs) -> Result<()> {
    let shared = args.shared;
    let kind = args.kind;
    let pricing = PricingMap::load_with_overrides(
        shared.offline,
        crate::log_level() != Some(0),
        shared.pricing_overrides.iter(),
    );
    let mut entries = load_entries(&shared, &pricing)?;
    let mut rows = if kind == AgentReportKind::Session {
        summarize_entries(&entries, kind)?
    } else {
        filter_loaded_entries_by_date(&mut entries, &shared);
        summarize_entries(&entries, kind)?
    };
    if kind == AgentReportKind::Session {
        filter_session_summaries_by_last_activity(&mut rows, &shared);
    }
    sort_summaries(&mut rows, &shared.order, ccusage_core::summary_period);
    if wants_json(&shared) {
        return print_json_or_jq(
            report_from_rows(&rows, kind),
            shared.jq.as_deref(),
            shared.no_cost,
        );
    }
    print_usage_table(
        "Devin Token Usage Report",
        ccusage_core::first_column(kind),
        &rows,
        &shared,
        false,
        None,
    )?;
    Ok(())
}
