//! Per-application volume.
//!
//! Two channels, because players expose volume in two incompatible ways:
//!
//! * `mixer` — the Windows audio session mixer (`ISimpleAudioVolume`). Works for
//!   anything that plays audio, but can only see an exe name or an AUMID.
//! * `player` — the player's own slider, driven through UI Automation by a small
//!   PowerShell worker. Needed by Spotify, whose mixer session is shared with
//!   its preload helper and whose SMTC volume is read-only.
//!
//! `auto` tries the player channel first and labels whichever channel answered.
//! Once a drag starts the labelled channel is kept: a failed player write must
//! never silently become a mixer write, because that changes a different volume
//! than the one the user is looking at.

pub mod mixer;
pub mod player;

use std::sync::{Arc, Mutex};

use super::{Queue, VolumeEvent};

/// One channel's answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    Player(f32),
    Mixer(f32),
}

impl Level {
    pub fn value(self) -> f32 {
        match self {
            Level::Player(v) | Level::Mixer(v) => v,
        }
    }
}

/// Answers one read/write against the player channel.
///
/// `value == None` is a read. Returned value is the level the channel reports
/// *after* the operation, so a clamped write surfaces the clamped result.
pub fn resolve_with(
    mode: &str,
    value: Option<f32>,
    player: &mut dyn FnMut(Option<f32>) -> Option<f32>,
    mixer: &mut dyn FnMut(Option<f32>) -> Option<f32>,
) -> Option<(Level, &'static str)> {
    let mut used = if mode == "mixer" { "mixer" } else { "player" };
    let mut level: Option<Level> = None;
    if used != "mixer" {
        if let Some(v) = player(value) {
            level = Some(Level::Player(v));
        }
    }
    if mode == "auto" {
        // The label is what the UI shows; a successful player read locks the
        // channel to "player" for the rest of the drag.
        used = if level.is_some() { "player" } else { "mixer" };
    }
    if used == "mixer" && level.is_none() {
        if let Some(v) = mixer(value) {
            level = Some(Level::Mixer(v));
        }
    } else if used == "mixer" {
        // An explicit `mixer` request always goes through the mixer.
        if let Some(v) = mixer(value) {
            level = Some(Level::Mixer(v));
        }
    }
    level.map(|l| (l, if used == "player" { "player" } else { "mixer" }))
}

/// Live channels for one request.
pub fn resolve(app: &str, mode: &str, value: Option<f32>) -> Option<(Level, &'static str)> {
    resolve_with(
        mode,
        value,
        &mut |v| match v {
            Some(x) => player::write(app, x),
            None => player::read(app),
        },
        &mut |v| match v {
            Some(x) => mixer::write(app, x),
            None => mixer::read(app),
        },
    )
}

/// One queued volume request.
#[derive(Clone, Debug, PartialEq)]
pub enum VolumeCmd {
    Get {
        app: String,
        mode: String,
        request_id: i64,
    },
    Set {
        app: String,
        value: f32,
        mode: String,
        request_id: i64,
    },
}

impl VolumeCmd {
    pub fn app(&self) -> &str {
        match self {
            VolumeCmd::Get { app, .. } | VolumeCmd::Set { app, .. } => app,
        }
    }
    pub fn mode(&self) -> &str {
        match self {
            VolumeCmd::Get { mode, .. } | VolumeCmd::Set { mode, .. } => mode,
        }
    }
    pub fn request_id(&self) -> i64 {
        match self {
            VolumeCmd::Get { request_id, .. } | VolumeCmd::Set { request_id, .. } => *request_id,
        }
    }
}

/// Turns one command plus its channel answer into the event the panel reads.
///
/// An empty target means nothing is playing: the request is answered
/// `available:false` with `reason:"no-session"` rather than being dropped, so a
/// panel that asked can stop its spinner.
pub fn answer(cmd: &VolumeCmd, current_app: &str, result: Option<(Level, &'static str)>) -> VolumeEvent {
    let asked = cmd.app();
    let target = if asked.is_empty() { current_app } else { asked };
    match result {
        Some((level, channel)) if !target.is_empty() => VolumeEvent {
            app: asked.to_string(),
            available: true,
            value: Some(level.value()),
            mode: channel.to_string(),
            request_id: cmd.request_id(),
            reason: None,
        },
        _ => VolumeEvent {
            app: asked.to_string(),
            available: false,
            value: None,
            mode: if cmd.mode() == "auto" { "mixer".into() } else { cmd.mode().to_string() },
            request_id: cmd.request_id(),
            reason: if target.is_empty() {
                Some("no-session")
            } else {
                Some("internal-unavailable")
            },
        },
    }
}

/// Handle handed to the UI. The worker thread owns COM; this only enqueues.
pub struct VolumeHandle {
    queue: Arc<Queue<VolumeCmd>>,
    current_app: Arc<Mutex<String>>,
}

impl VolumeHandle {
    pub fn new(current_app: Arc<Mutex<String>>) -> Self {
        Self {
            queue: Arc::new(Queue::with_capacity(32)),
            current_app,
        }
    }

    pub fn get(&self, app: String, mode: String, request_id: i64) {
        self.queue.push(VolumeCmd::Get {
            app,
            mode,
            request_id,
        });
    }

    pub fn set(&self, app: String, value: f32, mode: String, request_id: i64) {
        self.queue.push(VolumeCmd::Set {
            app,
            value: value.clamp(0.0, 1.0),
            mode,
            request_id,
        });
    }

    /// The app the SMTC worker last followed; used when a request names none.
    pub fn current_app(&self) -> String {
        self.current_app
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn queue(&self) -> Arc<Queue<VolumeCmd>> {
        self.queue.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(v: f32) -> Option<f32> {
        Some(v)
    }

    #[test]
    fn auto_prefers_the_player_channel_and_labels_it() {
        let mut player = |v: Option<f32>| ok(v.unwrap_or(0.4));
        let mut mixer = |_v: Option<f32>| ok(0.9);
        let (level, channel) = resolve_with("auto", None, &mut player, &mut mixer).unwrap();
        assert_eq!(level, Level::Player(0.4));
        assert_eq!(channel, "player");
    }

    #[test]
    fn auto_falls_back_to_the_mixer_when_the_player_is_unavailable() {
        let mut player = |_v: Option<f32>| None;
        let mut mixer = |_v: Option<f32>| ok(0.25);
        let (level, channel) = resolve_with("auto", None, &mut player, &mut mixer).unwrap();
        assert_eq!(level, Level::Mixer(0.25));
        assert_eq!(channel, "mixer");
    }

    #[test]
    fn an_explicit_mixer_drag_never_touches_the_player() {
        let mut player = |_v: Option<f32>| panic!("player channel must not be used");
        let mut mixer = |v: Option<f32>| ok(v.unwrap());
        let (level, channel) = resolve_with("mixer", Some(0.5), &mut player, &mut mixer).unwrap();
        assert_eq!(level, Level::Mixer(0.5));
        assert_eq!(channel, "mixer");
    }

    #[test]
    fn a_player_drag_does_not_fall_through_to_the_mixer() {
        // A failed player write must fail: writing the mixer instead would move
        // a different volume than the slider the user is holding.
        let mut player = |_v: Option<f32>| None;
        let mut mixer = |_v: Option<f32>| ok(1.0);
        assert_eq!(resolve_with("player", Some(0.3), &mut player, &mut mixer), None);
    }

    #[test]
    fn an_empty_target_is_answered_rather_than_dropped() {
        let cmd = VolumeCmd::Get {
            app: String::new(),
            mode: "auto".into(),
            request_id: 3,
        };
        let event = answer(&cmd, "", None);
        assert!(!event.available);
        assert_eq!(event.reason, Some("no-session"));
        assert_eq!(event.mode, "mixer");
        assert_eq!(event.request_id, 3);
    }

    #[test]
    fn a_successful_answer_carries_the_channel_and_value() {
        let cmd = VolumeCmd::Set {
            app: "Spotify.exe".into(),
            value: 0.2,
            mode: "auto".into(),
            request_id: 9,
        };
        let event = answer(&cmd, "Spotify.exe", Some((Level::Player(0.2), "player")));
        assert!(event.available);
        assert_eq!(event.value, Some(0.2));
        assert_eq!(event.mode, "player");
        assert_eq!(event.reason, None);
    }
}
