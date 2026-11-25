//! Swap-chain cadence sampling.
//!
//! The numbers come from `GetFrameStatistics`, which reports what the display
//! stack accepted rather than what the application asked for. Two properties
//! matter for the acceptance report: the QPC frequency is read from the
//! platform (never assumed to be 10 MHz), and the timestamps are converted to
//! milliseconds before any interval is computed.

use gpui_kit::Window;

/// Read the platform's QPC frequency once, in ticks per second.
///
/// The acceptance report calls out an earlier build that divided QPC ticks by
/// `10_000.0` on the assumption the counter ran at 10 MHz. That is a
/// platform-defined value, so it is queried instead of assumed; the fallback
/// only exists so a non-Windows host can still format a report.
pub fn qpc_hz() -> f64 {
    #[cfg(windows)]
    {
        use std::sync::OnceLock;
        static HZ: OnceLock<f64> = OnceLock::new();
        *HZ.get_or_init(|| {
            use windows::Win32::System::Performance::QueryPerformanceFrequency;
            let mut hz = 0i64;
            // Safety: the out-parameter is a valid stack slot for the call.
            if unsafe { QueryPerformanceFrequency(&mut hz) }.is_ok() && hz > 0 {
                hz as f64
            } else {
                10_000_000.0
            }
        })
    }
    #[cfg(not(windows))]
    {
        10_000_000.0
    }
}

/// One `GetFrameStatistics` observation.
#[derive(Clone, Copy, Debug)]
pub struct PresentSample {
    pub present_count: u64,
    pub refresh_count: u64,
    /// QPC ticks at which the present was displayed.
    pub sync_qpc_time: i64,
}

/// Accumulates present statistics for one run.
#[derive(Default)]
pub struct PresentCadence {
    last: Option<PresentSample>,
    /// Intervals between accepted presents, in milliseconds.
    intervals: Vec<f64>,
    presents: u64,
    missed_vsync: u64,
    over_8_33: u64,
    over_16_67: u64,
    stagnant: u64,
    rejected: u64,
    discontinuities: u64,
    /// Interval deltas discarded because the swap chain's counters went
    /// backwards or jumped by an implausible amount (a device reset or a
    /// recreated swap chain resets both counters, and DWM may briefly return a
    /// stale pair). Kept separate from `discontinuities`, which counts failed
    /// statistics queries.
    resets: u64,
    /// The last raw counters the swap chain reported, so the report can be
    /// checked against `GetFrameStatistics` instead of only ratios.
    last_present_count: u64,
    last_refresh_count: u64,
    /// Wall-clock milliseconds covered by the samples.
    wall_ms: f64,
    hz: f64,
}

impl PresentCadence {
    pub fn new() -> Self {
        Self {
            hz: qpc_hz(),
            ..Default::default()
        }
    }

    /// The measured QPC frequency, so a report can state what it divided by.
    pub fn hz(&self) -> f64 {
        self.hz
    }

    /// Samples the swap chain once per paint.
    pub fn observe(&mut self, window: &Window, wall_ms: f64) {
        let Ok(stats) = window.present_statistics() else {
            // DWM reports a disjoint statistics window after a mode change or
            // a compositor restart. Skipping the sample keeps a bogus zero out
            // of the series; the discontinuity counter records that it happened.
            self.discontinuities += 1;
            return;
        };
        let sample = PresentSample {
            present_count: stats.present_count,
            refresh_count: stats.refresh_count,
            sync_qpc_time: stats.sync_qpc_time,
        };
        self.wall_ms = self.wall_ms.max(wall_ms);
        if let Some(last) = self.last {
            // A recreated swap chain restarts both counters at zero, and DWM
            // can briefly hand back a stale pair. Either way the deltas below
            // are meaningless, so the interval is counted as a reset instead of
            // being folded into the missed-vsync total.
            let rewind = sample.present_count < last.present_count
                || sample.refresh_count < last.refresh_count;
            let jump = sample.present_count.saturating_sub(last.present_count) > 10_000
                || sample.refresh_count.saturating_sub(last.refresh_count) > 10_000;
            if rewind || jump {
                self.resets += 1;
            } else {
                let delta_ticks = sample.sync_qpc_time.saturating_sub(last.sync_qpc_time);
                if delta_ticks > 0 {
                    let ms = delta_ticks as f64 * 1000.0 / self.hz;
                    if ms > 0.0 && ms < 5_000.0 {
                        self.intervals.push(ms);
                        if ms > 8.33 {
                            self.over_8_33 += 1;
                        }
                        if ms > 16.67 {
                            self.over_16_67 += 1;
                        }
                    } else if ms >= 5_000.0 {
                        self.discontinuities += 1;
                    }
                }
                let presents = sample.present_count.saturating_sub(last.present_count);
                if presents == 0 {
                    self.stagnant += 1;
                }
                self.presents += presents;
                let refreshes = sample.refresh_count.saturating_sub(last.refresh_count);
                if refreshes > presents {
                    self.missed_vsync += refreshes - presents;
                }
                if presents > refreshes + 1 {
                    // More accepted presents than refreshes: the app is not the
                    // limiting factor, which is not a "missed vsync".
                    self.rejected += 1;
                }
            }
        }
        self.last_present_count = sample.present_count;
        self.last_refresh_count = sample.refresh_count;
        self.last = Some(sample);
    }

    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty() && self.presents == 0
    }

    pub fn percentiles(&self) -> (f64, f64, f64, f64) {
        if self.intervals.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let mut sorted = self.intervals.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let pick = |q: f64| -> f64 {
            let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
            sorted[idx.min(sorted.len() - 1)]
        };
        let max = *sorted.last().unwrap_or(&0.0);
        (pick(0.50), pick(0.95), pick(0.99), max)
    }

    /// The `present-report.json` body.
    pub fn report(&self) -> String {
        let (p50, p95, p99, max) = self.percentiles();
        let mean = if self.intervals.is_empty() {
            0.0
        } else {
            self.intervals.iter().sum::<f64>() / self.intervals.len() as f64
        };
        format!(
            concat!(
                "{{\n",
                "  \"qpcHz\": {:.0},\n",
                "  \"samples\": {},\n",
                "  \"presents\": {},\n",
                "  \"wallMs\": {:.1},\n",
                "  \"meanMs\": {:.4},\n",
                "  \"p50Ms\": {:.4},\n",
                "  \"p95Ms\": {:.4},\n",
                "  \"p99Ms\": {:.4},\n",
                "  \"maxMs\": {:.4},\n",
                "  \"over8_33\": {},\n",
                "  \"over16_67\": {},\n",
                "  \"missedVsync\": {},\n",
                "  \"stagnant\": {},\n",
                "  \"rejected\": {},\n",
                "  \"discontinuities\": {},\n",
                "  \"resets\": {},\n",
                "  \"lastPresentCount\": {},\n",
                "  \"lastRefreshCount\": {}\n",
                "}}\n"
            ),
            self.hz,
            self.intervals.len(),
            self.presents,
            self.wall_ms,
            mean,
            p50,
            p95,
            p99,
            max,
            self.over_8_33,
            self.over_16_67,
            self.missed_vsync,
            self.stagnant,
            self.rejected,
            self.discontinuities,
            self.resets,
            self.last_present_count,
            self.last_refresh_count,
        )
    }
}
