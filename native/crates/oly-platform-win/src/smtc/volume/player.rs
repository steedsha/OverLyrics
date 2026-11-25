//! Player slider volume, driven through UI Automation.
//!
//! Spotify's mixer session is shared with its preload helper and its SMTC
//! volume is read-only, so the only honest way to change its volume is to move
//! the slider in its own window. That work runs in a small Windows PowerShell
//! worker: UIA client providers are COM objects that can hang inside a broken
//! player, and isolating them in a child process lets a stuck call be killed
//! without losing the SMTC thread.
//!
//! The script path is resolved by the host at startup; when it is missing the
//! channel reports unavailable rather than falling back to the mixer, because a
//! fallback would change a different volume than the one the user sees.

#![allow(unsafe_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW`: the worker must never flash a console over the lyrics.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long the worker is given to answer one request before it is killed and
/// restarted. The safety deadline carried in the request is 1500 ms so a late
/// lookup cannot start a write after the UI has already fallen back.
const REPLY_TIMEOUT: Duration = Duration::from_millis(1800);
const COLD_START_REPLY_TIMEOUT: Duration = Duration::from_millis(3000);
const REQUEST_DEADLINE_MS: i64 = 1500;

static SCRIPT: Mutex<Option<PathBuf>> = Mutex::new(None);
static WORKER: Mutex<Option<Worker>> = Mutex::new(None);

/// Called once at startup with the bundled script location.
pub fn init(script: Option<PathBuf>) {
    *SCRIPT.lock().unwrap_or_else(|e| e.into_inner()) = script;
}

pub fn read(app: &str) -> Option<f32> {
    visit(app, None)
}

pub fn write(app: &str, value: f32) -> Option<f32> {
    visit(app, Some(value.clamp(0.0, 1.0)))
}

fn visit(app: &str, value: Option<f32>) -> Option<f32> {
    if app.trim().is_empty() {
        return None;
    }
    let mut guard = WORKER.lock().unwrap_or_else(|e| e.into_inner());
    let cold_start = guard.as_mut().is_none_or(|w| !w.alive());
    if cold_start {
        if let Some(old) = guard.take() {
            old.stop();
        }
        *guard = start();
    }
    let worker = guard.as_mut()?;

    let deadline_budget = if cold_start { 2800 } else { REQUEST_DEADLINE_MS };
    let request = serde_json::json!({
        "app": app,
        "value": value,
        "deadline": super::super::now_ms() + deadline_budget,
    });
    let mut line = serde_json::to_string(&request).ok()?;
    line.push('\n');
    if worker.stdin.write_all(line.as_bytes()).is_err() || worker.stdin.flush().is_err() {
        if let Some(old) = guard.take() {
            old.stop();
        }
        return None;
    }

    let timeout = if cold_start { COLD_START_REPLY_TIMEOUT } else { REPLY_TIMEOUT };
    match worker.lines.recv_timeout(timeout) {
        Ok(reply) => parse_reply(&reply),
        Err(_) => {
            // A timed-out UIA call leaves the worker in an unknown state; kill
            // it instead of letting the next request queue behind the hang.
            if let Some(old) = guard.take() {
                old.stop();
            }
            None
        }
    }
}

/// A reply is only trusted when it is a finite fraction; anything else means
/// the script could not distinguish a slider from something else on the page.
pub fn parse_reply(reply: &str) -> Option<f32> {
    let parsed: serde_json::Value = serde_json::from_str(reply).ok()?;
    let value = parsed.get("value")?.as_f64()?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return None;
    }
    Some(value as f32)
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Worker {
    fn alive(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Closing stdin makes the script's `ReadLine` return null, which is its
    /// shutdown signal; a worker that ignores it is killed.
    fn stop(mut self) {
        drop(self.stdin);
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
fn start() -> Option<Worker> {
    let script = SCRIPT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    if !script.is_file() {
        return None;
    }
    // Windows PowerShell 5.1 specifically: `Add-Type -AssemblyName
    // UIAutomationClient` and the .NET Framework UIA client are not available
    // in PowerShell 7, and PATH may point at either.
    let system_root = std::env::var_os("SystemRoot")?;
    let powershell = PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let mut command = Command::new(&powershell);
    command
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-MTA")
        .arg("-WindowStyle")
        .arg("Hidden")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().ok()?;

    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let (tx, lines) = channel();
    std::thread::Builder::new()
        .name("smtc-volume-ps".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(text) => {
                        if tx.send(text).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .ok()?;
    Some(Worker {
        child,
        stdin,
        lines,
    })
}

#[cfg(not(windows))]
fn start() -> Option<Worker> {
    None
}

#[cfg(test)]
mod tests {
    use super::parse_reply;

    #[test]
    fn only_a_finite_fraction_is_accepted() {
        assert_eq!(parse_reply(r#"{"value":0.42}"#), Some(0.42));
        assert_eq!(parse_reply(r#"{"value":0}"#), Some(0.0));
        assert_eq!(parse_reply(r#"{"value":1}"#), Some(1.0));
        assert_eq!(parse_reply(r#"{"value":1.5}"#), None);
        assert_eq!(parse_reply(r#"{"value":-0.1}"#), None);
        assert_eq!(parse_reply(r#"{"value":null}"#), None);
        assert_eq!(parse_reply("not json"), None);
        assert_eq!(parse_reply(r#"{"other":0.5}"#), None);
    }
}
