//! G4 acceptance plan: the declarative scenario set the 30-minute run must
//! cover, and the check that it actually does.
//!
//! The plan file (`native/tests/fixtures/acceptance.json`) is data, not prose:
//! `oly-qa replay <plan>` parses it, sums the phases, and fails when the set
//! cannot satisfy the acceptance requirements (30 minutes, ≥20 track changes,
//! ≥50 panel toggles, ≥20 shape round trips, four shapes, three DPIs, the
//! 800×500 third size, and every interaction family the task names). A run
//! that quietly drops a requirement cannot pass by omission.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One acceptance phase: a shape at a DPI, run for a fixed wall time, with a
/// counted workload.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Phase {
    pub name: String,
    /// `mini` / `album` / `lyrics` / `window`.
    pub shape: String,
    /// `WxH`, the logical window size the phase runs at.
    pub viewport: String,
    /// Display scale (1.0 / 1.25 / 1.5 …).
    pub dpi: f32,
    /// Wall-clock seconds the phase runs.
    pub seconds: u64,
    /// Track changes driven during the phase.
    #[serde(default)]
    pub tracks: u32,
    /// Panel open/close toggles (more / picker / volume).
    #[serde(default, rename = "panelToggles")]
    pub panel_toggles: u32,
    /// Shape round trips (window → compact → window).
    #[serde(default, rename = "shapeCycles")]
    pub shape_cycles: u32,
    /// Interaction families exercised in the phase.
    #[serde(default)]
    pub interactions: Vec<String>,
}

/// The whole acceptance plan.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AcceptancePlan {
    pub name: String,
    /// The display refresh rate the present metrics are judged against.
    #[serde(rename = "refreshHz")]
    pub refresh_hz: f32,
    pub phases: Vec<Phase>,
    /// Interaction families the sample plate cannot express at all. They are
    /// reported as unsupported (`null` in the acceptance table), never counted
    /// as covered.
    #[serde(default, rename = "knownUnsupported")]
    pub known_unsupported: Vec<String>,
}

/// The interaction families the G4 task requires.
pub const REQUIRED_INTERACTIONS: &[&str] = &[
    "pause",
    "wrap",
    "wheel",
    "thumb",
    "resize",
    "background",
    "motion-loop",
    "panels",
    "picker",
    "volume",
    "source-switch",
    "translation-toggle",
    "colour-dialog-hold",
    "multi-window",
];

/// Minimums from the task text.
pub const MIN_TOTAL_SECONDS: u64 = 1800;
pub const MIN_TRACKS: u32 = 20;
pub const MIN_PANEL_TOGGLES: u32 = 50;
pub const MIN_SHAPE_CYCLES: u32 = 20;
pub const MIN_DPI: usize = 3;
pub const MIN_SHAPES: usize = 4;

/// The resolved coverage of a plan.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AcceptanceReport {
    pub name: String,
    #[serde(rename = "refreshHz")]
    pub refresh_hz: f32,
    #[serde(rename = "totalSeconds")]
    pub total_seconds: u64,
    pub shapes: Vec<String>,
    pub viewports: Vec<String>,
    pub dpis: Vec<f32>,
    pub tracks: u32,
    #[serde(rename = "panelToggles")]
    pub panel_toggles: u32,
    #[serde(rename = "shapeCycles")]
    pub shape_cycles: u32,
    pub interactions: Vec<String>,
    /// Requirements the plan misses.
    pub gaps: Vec<String>,
    /// Required interactions the sample plate cannot express; listed so the
    /// acceptance table can mark them unsupported instead of green.
    pub unsupported: Vec<String>,
}

impl AcceptanceReport {
    pub fn ok(&self) -> bool {
        self.gaps.is_empty()
    }
}

impl AcceptancePlan {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
    }

    /// Resolves the plan into totals and checks them against the task's
    /// minimums. Pure: no filesystem, no clock.
    pub fn validate(&self) -> AcceptanceReport {
        let mut shapes = BTreeSet::new();
        let mut viewports = BTreeSet::new();
        let mut dpis = BTreeSet::new();
        let mut interactions = BTreeSet::new();
        let mut total_seconds = 0u64;
        let mut tracks = 0u32;
        let mut panel_toggles = 0u32;
        let mut shape_cycles = 0u32;

        for phase in &self.phases {
            shapes.insert(phase.shape.clone());
            viewports.insert(phase.viewport.clone());
            dpis.insert((phase.dpi * 100.0).round() as i64);
            total_seconds += phase.seconds;
            tracks += phase.tracks;
            panel_toggles += phase.panel_toggles;
            shape_cycles += phase.shape_cycles;
            for i in &phase.interactions {
                interactions.insert(i.clone());
            }
        }

        let unsupported: BTreeSet<String> = self
            .known_unsupported
            .iter()
            .filter(|u| REQUIRED_INTERACTIONS.contains(&u.as_str()))
            .cloned()
            .collect();

        let mut gaps = Vec::new();
        if total_seconds < MIN_TOTAL_SECONDS {
            gaps.push(format!(
                "total runtime {total_seconds}s is under the {MIN_TOTAL_SECONDS}s minimum"
            ));
        }
        if shapes.len() < MIN_SHAPES {
            gaps.push(format!(
                "only {} shape(s) covered ({:?}), four are required",
                shapes.len(),
                shapes
            ));
        }
        if dpis.len() < MIN_DPI {
            gaps.push(format!(
                "only {} DPI step(s) covered, three are required",
                dpis.len()
            ));
        }
        if !viewports.contains("800x500") {
            gaps.push("the 800x500 third size is not covered".to_string());
        }
        if tracks < MIN_TRACKS {
            gaps.push(format!(
                "only {tracks} track changes, {MIN_TRACKS} are required"
            ));
        }
        if panel_toggles < MIN_PANEL_TOGGLES {
            gaps.push(format!(
                "only {panel_toggles} panel toggles, {MIN_PANEL_TOGGLES} are required"
            ));
        }
        if shape_cycles < MIN_SHAPE_CYCLES {
            gaps.push(format!(
                "only {shape_cycles} shape round trips, {MIN_SHAPE_CYCLES} are required"
            ));
        }
        for required in REQUIRED_INTERACTIONS {
            if !interactions.contains(*required) && !unsupported.contains(*required) {
                gaps.push(format!("interaction family {required} is not covered"));
            }
        }

        AcceptanceReport {
            name: self.name.clone(),
            refresh_hz: self.refresh_hz,
            total_seconds,
            shapes: shapes.into_iter().collect(),
            viewports: viewports.into_iter().collect(),
            dpis: dpis.into_iter().map(|d| d as f32 / 100.0).collect(),
            tracks,
            panel_toggles,
            shape_cycles,
            interactions: interactions.into_iter().collect(),
            gaps,
            unsupported: unsupported.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(shape: &str, viewport: &str, dpi: f32, seconds: u64) -> Phase {
        Phase {
            name: format!("{shape}-{viewport}-{dpi}"),
            shape: shape.to_string(),
            viewport: viewport.to_string(),
            dpi,
            seconds,
            tracks: 0,
            panel_toggles: 0,
            shape_cycles: 0,
            interactions: Vec::new(),
        }
    }

    fn full_plan() -> AcceptancePlan {
        let all: Vec<String> = REQUIRED_INTERACTIONS
            .iter()
            .filter(|i| **i != "multi-window")
            .map(|i| i.to_string())
            .collect();
        AcceptancePlan {
            name: "test".to_string(),
            refresh_hz: 239.0,
            phases: vec![
                Phase {
                    tracks: 12,
                    panel_toggles: 30,
                    shape_cycles: 12,
                    interactions: all.clone(),
                    ..phase("window", "1272x843", 1.0, 900)
                },
                Phase {
                    tracks: 8,
                    panel_toggles: 20,
                    shape_cycles: 8,
                    interactions: all.clone(),
                    ..phase("mini", "800x500", 1.25, 450)
                },
                Phase {
                    tracks: 6,
                    panel_toggles: 10,
                    shape_cycles: 4,
                    interactions: all,
                    ..phase("lyrics", "380x650", 1.5, 300)
                },
                phase("album", "350x350", 1.5, 150),
            ],
            known_unsupported: vec!["multi-window".to_string()],
        }
    }

    #[test]
    fn a_complete_plan_passes_and_reports_totals() {
        let report = full_plan().validate();
        assert!(report.ok(), "unexpected gaps: {:?}", report.gaps);
        assert_eq!(report.total_seconds, 1800);
        assert_eq!(report.tracks, 26);
        assert_eq!(report.panel_toggles, 60);
        assert_eq!(report.shape_cycles, 24);
        assert_eq!(report.dpis, vec![1.0, 1.25, 1.5]);
        assert!(report.unsupported.contains(&"multi-window".to_string()));
    }

    #[test]
    fn a_short_run_is_rejected() {
        let mut plan = full_plan();
        plan.phases[0].seconds = 100;
        let report = plan.validate();
        assert!(!report.ok());
        assert!(report.gaps.iter().any(|g| g.contains("minimum")));
    }

    #[test]
    fn a_missing_shape_or_dpi_is_rejected() {
        let mut plan = full_plan();
        plan.phases.pop();
        let report = plan.validate();
        assert!(report.gaps.iter().any(|g| g.contains("shape(s) covered")));
    }

    #[test]
    fn a_missing_interaction_family_is_named_in_the_gap() {
        let mut plan = full_plan();
        plan.phases.iter_mut().for_each(|p| {
            p.interactions.retain(|i| i != "colour-dialog-hold");
        });
        let report = plan.validate();
        assert!(
            report
                .gaps
                .iter()
                .any(|g| g.contains("colour-dialog-hold")),
            "gaps: {:?}",
            report.gaps
        );
    }

    #[test]
    fn the_800x500_size_is_mandatory() {
        let mut plan = full_plan();
        plan.phases[1].viewport = "900x600".to_string();
        let report = plan.validate();
        assert!(report.gaps.iter().any(|g| g.contains("800x500")));
    }
}
