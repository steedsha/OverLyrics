//! Per-application volume, fronted by a single worker thread.
//!
//! The mixer and player channels live in [`crate::smtc::volume`]; this module
//! owns the thread that runs them, because both need a COM apartment and both
//! can block (a UI Automation read waits for a player's window). One thread
//! serves every window: a second worker would double the PowerShell children a
//! volume drag can spawn.

// The worker initialises its own COM apartment; that call has no safe binding.
#![allow(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use crate::smtc::volume::{
    Level, VolumeCmd, VolumeHandle, answer, resolve, resolve_with,
};
use crate::smtc::{EventSink, Queue};

/// How long the worker waits for a request before checking the shutdown flag.
const IDLE_POLL: Duration = Duration::from_millis(500);

/// Starts the volume worker and returns the shell's handle to it.
///
/// The thread lives as long as the process; there is no per-window task. A
/// failed COM initialisation leaves the handle in place but every request is
/// answered `available:false`, which is what the panel already renders.
pub fn spawn(sink: Arc<dyn EventSink>, current_app: Arc<Mutex<String>>) -> VolumeHandle {
    let handle = VolumeHandle::new(current_app.clone());
    let queue: Arc<Queue<VolumeCmd>> = handle.queue();
    let _ = std::thread::Builder::new()
        .name("oly-volume".into())
        .spawn(move || {
            if !init_mta() {
                log::error!("[volume] COM apartment initialization failed");
                while let Some(cmd) = queue.pop_wait(IDLE_POLL) {
                    sink.volume(answer(&cmd, &current_app.lock().unwrap_or_else(|e| e.into_inner()), None));
                }
                return;
            }
            loop {
                let Some(cmd) = queue.pop_wait(IDLE_POLL) else {
                    continue;
                };
                let live = current_app
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let asked = cmd.app().to_string();
                let target = if asked.is_empty() { live.clone() } else { asked };
                let result = if target.trim().is_empty() {
                    None
                } else {
                    match &cmd {
                        VolumeCmd::Get { mode, .. } => resolve(&target, mode, None),
                        VolumeCmd::Set { value, mode, .. } => resolve(&target, mode, Some(*value)),
                    }
                };
                sink.volume(answer(&cmd, &live, result));
            }
        });
    handle
}

fn init_mta() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
        const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        hr.is_ok() || hr.0 == RPC_E_CHANGED_MODE
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_request_is_answered_not_dropped() {
        let cmd = VolumeCmd::Get {
            app: String::new(),
            mode: "auto".into(),
            request_id: 1,
        };
        let event = answer(&cmd, "", None);
        assert!(!event.available);
        assert_eq!(event.reason, Some("no-session"));
    }
}
