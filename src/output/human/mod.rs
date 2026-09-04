//! Human-facing CLI printers, split by surface (0252).
//!
//! Public paths stay `crate::output::human::{print_*, DoctorReport, ...}`.

mod dead_code;
mod doctor;
mod hotspots;
mod impact;
mod scan;
mod verify;

pub use dead_code::{
    DEAD_CODE_EMPTY_STATE, DEAD_CODE_HONESTY_FOOTER, print_dead_code_explanation,
    print_dead_code_explanation_struct, print_dead_code_grouped, print_dead_code_summary,
};
pub(crate) use doctor::doctor_should_print_remediation;
pub(crate) use doctor::print_doctor_report_to;
pub use doctor::{
    DoctorHumanProfile, DoctorReport, DoctorSummaryCounts, format_doctor_summary_text,
    format_doctor_tool_line, format_hygiene_collapse_trailer, format_signing_deferred_trailer,
    partition_doctor_findings_for_human, print_doctor_report, wsl_support_line,
};
pub use hotspots::{
    print_hotspots, print_hotspots_table, print_hotspots_table_with_centrality,
    print_semantic_hotspots,
};
pub use impact::{print_impact_brief, print_impact_summary, print_impact_summary_with_full};
pub use scan::print_scan_summary;
pub use verify::{print_verify_plan, print_verify_result};

#[cfg(test)]
mod tests;
