//! Task 10 red test: the experiment-wide memory ledger.
//!
//! The plan's G2 gate is a private-RAM line (180 MiB + 20 MiB slack) split
//! into named budgets. The numbers only mean something if every subsystem
//! reports into ONE ledger, so this module owns the accounting: subsystem
//! baselines, current usage, and a `report()` that fails when a subsystem
//! exceeds its budget or the total breaks the line.
//!
//! This is the accounting skeleton, not the measurement: the plan is explicit
//! that real numbers come from system statistics, not from self-reporting.
//! Subsystems opt in by holding a `BudgetGuard` and reporting through it.

use oly_core::budget::{
    BudgetGuard, BudgetLine, BUDGET_GRAPHICS_CPU, BUDGET_IMAGE_CACHE, BUDGET_OTHER_WINDOWS,
    BUDGET_RUNTIME_PLATFORM, BUDGET_TELEMETRY, BUDGET_TOTAL_LINE, BUDGET_TOTAL_SLACK,
    BUDGET_VIDEO, BUDGET_BUSINESS,
};

/// The plan's initial allocation, verbatim (MiB).
#[test]
fn plan_allocation_constants_match_the_g2_table() {
    assert_eq!(BUDGET_RUNTIME_PLATFORM, 60);
    assert_eq!(BUDGET_BUSINESS, 20);
    assert_eq!(BUDGET_GRAPHICS_CPU, 24);
    assert_eq!(BUDGET_IMAGE_CACHE, 16);
    assert_eq!(BUDGET_VIDEO, 32);
    assert_eq!(BUDGET_OTHER_WINDOWS, 12);
    assert_eq!(BUDGET_TELEMETRY, 8);
    // Sum of the named lines (the plan's "other 8" is the remainder below).
    let named: u64 = 60 + 20 + 24 + 16 + 32 + 12 + 8;
    assert_eq!(BUDGET_TOTAL_LINE, 180);
    assert_eq!(BUDGET_TOTAL_SLACK, 20);
    // The named lines plus the 8 MiB "other" must fit the line.
    assert!(named + 8 <= BUDGET_TOTAL_LINE + BUDGET_TOTAL_SLACK);
}

/// A subsystem over budget is an error, not a warning.
#[test]
fn over_budget_subsystem_is_reported_as_failed() {
    let mut ledger = BudgetGuard::new();
    ledger.set(BudgetLine::Video, 40 * 1024 * 1024); // over 32 MiB
    let report = ledger.report();
    assert!(
        !report.all_within_budget,
        "40 MiB in a 32 MiB line must fail"
    );
    let video = report
        .lines
        .iter()
        .find(|l| l.line == BudgetLine::Video)
        .expect("video line present");
    assert_eq!(video.used_bytes, 40 * 1024 * 1024);
    assert_eq!(video.budget_bytes, 32 * 1024 * 1024);
    assert!(video.over);
}

/// Within-budget usage passes, and the total is checked against the line.
#[test]
fn within_budget_totals_pass_until_the_line_breaks() {
    let mut ledger = BudgetGuard::new();
    ledger.set(BudgetLine::RuntimePlatform, 50 * 1024 * 1024);
    ledger.set(BudgetLine::Business, 15 * 1024 * 1024);
    ledger.set(BudgetLine::Video, 30 * 1024 * 1024);
    let report = ledger.report();
    assert!(
        report.all_within_budget,
        "all lines under budget must pass"
    );

    // Push the total past the line: still under per-line budgets is possible
    // only if some line is over, so force it through the slack accounting.
    ledger.set(BudgetLine::RuntimePlatform, 120 * 1024 * 1024); // over 60
    let report = ledger.report();
    assert!(!report.all_within_budget);
    let total_mib = report.total_bytes / (1024 * 1024);
    assert!(total_mib >= 120, "total must include the 120 MiB platform");
}

/// Usage accounting is a delta-able add, not just a set: a subsystem reports
/// increments (a decoded frame in, a frame released out).
#[test]
fn add_and_remove_track_deltas() {
    let mut ledger = BudgetGuard::new();
    let frame = oly_core::cache::video_nv12_bytes(1080, 1080); // 1080*1080*1.5
    ledger.add(BudgetLine::Video, frame);
    ledger.add(BudgetLine::Video, frame);
    assert_eq!(
        ledger.report().video_bytes(),
        frame * 2,
        "two frames accounted"
    );
    ledger.remove(BudgetLine::Video, frame);
    assert_eq!(ledger.report().video_bytes(), frame);
    // Removing more than accounted must saturate at zero, not wrap.
    ledger.remove(BudgetLine::Video, frame * 5);
    assert_eq!(ledger.report().video_bytes(), 0);
}

/// The named lines are all present in every report, in a stable order.
#[test]
fn report_lists_every_named_line() {
    let ledger = BudgetGuard::new();
    let report = ledger.report();
    let lines: Vec<_> = report.lines.iter().map(|l| l.line).collect();
    for expected in [
        BudgetLine::RuntimePlatform,
        BudgetLine::Business,
        BudgetLine::GraphicsCpu,
        BudgetLine::ImageCache,
        BudgetLine::Video,
        BudgetLine::OtherWindows,
        BudgetLine::Telemetry,
        BudgetLine::Other,
    ] {
        assert!(
            lines.contains(&expected),
            "line {expected:?} missing from report"
        );
    }
    assert_eq!(lines.len(), 8, "exactly the 8 plan lines");
}
