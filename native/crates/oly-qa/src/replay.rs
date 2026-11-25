//! `oly-qa replay <case>`: feed a recorded packet script — and only that —
//! to the experiment's transport model. No online services, no real players.

use oly_contracts::PlayerPacket;
use oly_core::transport::TransportClock;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One scripted packet: the monotonic ms it arrives at, plus the packet.
#[derive(Clone, Debug, Deserialize)]
pub struct ScriptedPacket {
    /// Monotonic arrival time, ms (the case's clock seed base).
    #[serde(rename = "tMs")]
    pub t_ms: f64,
    pub packet: PlayerPacket,
}

/// A replay case: a recorded sequence with the clock seed that produced it.
#[derive(Clone, Debug, Deserialize)]
pub struct ReplayCase {
    #[serde(default)]
    pub name: String,
    /// The monotonic base (ms) the trace was recorded at. Replay anchors the
    /// first packet here instead of reading the wall clock.
    #[serde(rename = "clockSeedMs")]
    pub clock_seed_ms: f64,
    #[serde(default)]
    pub packets: Vec<ScriptedPacket>,
}

impl ReplayCase {
    /// Loads a case JSON from disk.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
    }
}

/// One recorded playhead observation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Playhead {
    /// The clock's now for this packet (seed + arrival offset).
    #[serde(rename = "nowMs")]
    pub now_ms: f64,
    /// The displayed playhead, in lyric time (`TransportClock::feed`).
    pub projected: f64,
    /// The sample was inside tolerance and the display held.
    pub held: bool,
    /// The renderer would have sought this frame.
    pub seek_pending: bool,
}

/// The result of one replay run.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ReplayResult {
    pub playheads: Vec<Playhead>,
}

/// Feeds every scripted packet to a fresh [`TransportClock`], at each
/// packet's own timestamp (the deterministic base the case carries — never
/// the wall clock).
pub fn run(case: &ReplayCase) -> anyhow::Result<ReplayResult> {
    let mut clock = TransportClock::new();
    let mut result = ReplayResult::default();
    for scripted in &case.packets {
        let outcome = clock.feed(Some(&scripted.packet), scripted.t_ms);
        result.playheads.push(Playhead {
            now_ms: scripted.t_ms,
            projected: outcome.projected,
            held: outcome.held,
            seek_pending: outcome.seek_pending,
        });
    }
    Ok(result)
}
