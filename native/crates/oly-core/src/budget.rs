//! Task 10: the experiment-wide RAM ledger (the G2 accounting skeleton).
//!
//! The plan's G2 gate is a 180 MiB private-working-set line with a 20 MiB
//! margin, split into named lines: runtime/platform 60, business 20,
//! graphics-CPU 24, image cache 16, video 32, other windows 12, telemetry 8,
//! other 8 (all MiB). The plan is explicit that these are *initial targets*,
//! not measurements — acceptance numbers come from the OS (system working
//! set), never from this ledger.
//!
//! What this module owns is the engineering side of that discipline: every
//! subsystem holds a [`BudgetGuard`] and charges its known allocations
//! against its line; `report()` names the offender when a line is over.
//! That turns "the process grew" into a per-subsystem question (allocator
//! analysis targets the line that blew) without inventing a second
//! accounting scheme: texture/decoder budgets still live in `cache.rs`
//! (`TextureCache`, `video_nv12_bytes`); this ledger holds the subsystem
//! lines that wrap them.

/// One subsystem's RAM line, identified as a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BudgetLine {
    RuntimePlatform,
    Business,
    GraphicsCpu,
    ImageCache,
    Video,
    OtherWindows,
    Telemetry,
    Other,
}

impl BudgetLine {
    /// All eight plan lines, in the plan's stable report order.
    pub const ALL: [BudgetLine; 8] = [
        BudgetLine::RuntimePlatform,
        BudgetLine::Business,
        BudgetLine::GraphicsCpu,
        BudgetLine::ImageCache,
        BudgetLine::Video,
        BudgetLine::OtherWindows,
        BudgetLine::Telemetry,
        BudgetLine::Other,
    ];

    /// The line's plan name (for diagnostics).
    pub const fn name(self) -> &'static str {
        match self {
            BudgetLine::RuntimePlatform => "runtime/platform",
            BudgetLine::Business => "business",
            BudgetLine::GraphicsCpu => "graphics-cpu",
            BudgetLine::ImageCache => "image-cache",
            BudgetLine::Video => "video",
            BudgetLine::OtherWindows => "other-windows",
            BudgetLine::Telemetry => "telemetry",
            BudgetLine::Other => "other",
        }
    }
}

/// The plan's initial allocation targets, in MiB (§Task 10, G2 table).
pub const BUDGET_RUNTIME_PLATFORM: u64 = 60;
pub const BUDGET_BUSINESS: u64 = 20;
pub const BUDGET_GRAPHICS_CPU: u64 = 24;
pub const BUDGET_IMAGE_CACHE: u64 = 16;
pub const BUDGET_VIDEO: u64 = 32;
pub const BUDGET_OTHER_WINDOWS: u64 = 12;
pub const BUDGET_TELEMETRY: u64 = 8;
/// The plan's remainder line ("other 8").
pub const BUDGET_OTHER: u64 = 8;

/// The G2 private-RAM line: 180 MiB with a 20 MiB margin.
pub const BUDGET_TOTAL_LINE: u64 = 180;
pub const BUDGET_TOTAL_SLACK: u64 = 20;

/// A line's MiB figure.
impl BudgetLine {
    pub const fn budget_mib(self) -> u64 {
        match self {
            BudgetLine::RuntimePlatform => BUDGET_RUNTIME_PLATFORM,
            BudgetLine::Business => BUDGET_BUSINESS,
            BudgetLine::GraphicsCpu => BUDGET_GRAPHICS_CPU,
            BudgetLine::ImageCache => BUDGET_IMAGE_CACHE,
            BudgetLine::Video => BUDGET_VIDEO,
            BudgetLine::OtherWindows => BUDGET_OTHER_WINDOWS,
            BudgetLine::Telemetry => BUDGET_TELEMETRY,
            BudgetLine::Other => BUDGET_OTHER,
        }
    }

    pub const fn budget_bytes(self) -> u64 {
        self.budget_mib() * 1024 * 1024
    }
}

/// One line's standing in a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineReport {
    pub line: BudgetLine,
    pub used_bytes: u64,
    pub budget_bytes: u64,
    pub over: bool,
}

/// The ledger's standing at a point in time.
#[derive(Clone, Debug)]
pub struct Report {
    /// Every plan line, in [`BudgetLine::ALL`] order.
    pub lines: Vec<LineReport>,
    /// Bytes charged across all lines.
    pub total_bytes: u64,
    /// True when every line is at or under its budget.
    pub all_within_budget: bool,
}

impl Report {
    /// Bytes charged against the video line (the QA replay's headline).
    pub fn video_bytes(&self) -> u64 {
        self.lines
            .iter()
            .find(|l| l.line == BudgetLine::Video)
            .map(|l| l.used_bytes)
            .unwrap_or(0)
    }
}

/// Per-subsystem charge accounting against the plan's G2 lines.
///
/// Not a memory measurement: subsystems report their known allocations
/// here (incrementally — a decoded frame in, a frame released out), and
/// the report flags the line that blew. The measured truth at acceptance
/// time remains the OS working set.
#[derive(Debug, Default)]
pub struct BudgetGuard {
    used: [u64; 8],
}

impl BudgetGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a line's current usage to `bytes` (absolute restatement).
    pub fn set(&mut self, line: BudgetLine, bytes: u64) {
        self.used[line as usize] = bytes;
    }

    /// Adds a delta to a line (a decoded frame in).
    pub fn add(&mut self, line: BudgetLine, bytes: u64) {
        self.used[line as usize] = self.used[line as usize].saturating_add(bytes);
    }

    /// Removes a delta from a line, saturating at zero (a frame released
    /// out must never wrap the accounting).
    pub fn remove(&mut self, line: BudgetLine, bytes: u64) {
        self.used[line as usize] = self.used[line as usize].saturating_sub(bytes);
    }

    /// The line's currently charged bytes.
    pub fn charged(&self, line: BudgetLine) -> u64 {
        self.used[line as usize]
    }

    /// Produces a report covering all eight lines, flagging overruns.
    pub fn report(&self) -> Report {
        let mut lines = Vec::with_capacity(8);
        let mut total = 0u64;
        let mut all_ok = true;
        for line in BudgetLine::ALL {
            let used = self.used[line as usize];
            let budget = line.budget_bytes();
            let over = used > budget;
            all_ok &= !over;
            total = total.saturating_add(used);
            lines.push(LineReport {
                line,
                used_bytes: used,
                budget_bytes: budget,
                over,
            });
        }
        Report {
            lines,
            total_bytes: total,
            all_within_budget: all_ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_allocation_constants_match_the_g2_table() {
        assert_eq!(BUDGET_RUNTIME_PLATFORM, 60);
        assert_eq!(BUDGET_BUSINESS, 20);
        assert_eq!(BUDGET_GRAPHICS_CPU, 24);
        assert_eq!(BUDGET_IMAGE_CACHE, 16);
        assert_eq!(BUDGET_VIDEO, 32);
        assert_eq!(BUDGET_OTHER_WINDOWS, 12);
        assert_eq!(BUDGET_TELEMETRY, 8);
        assert_eq!(BUDGET_OTHER, 8);
        assert_eq!(BUDGET_TOTAL_LINE, 180);
        assert_eq!(BUDGET_TOTAL_SLACK, 20);
    }

    #[test]
    fn named_lines_sum_to_the_plan_line() {
        let named: u64 = BudgetLine::ALL.iter().map(|l| l.budget_mib()).sum();
        assert_eq!(named, BUDGET_TOTAL_LINE, "60+20+24+16+32+12+8+8 = 180");
    }
}
