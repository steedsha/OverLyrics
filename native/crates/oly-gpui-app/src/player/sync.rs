#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn song(title: &str, album: &str) -> SongRef {
        SongRef {
            title: title.into(),
            artist: "Artist".into(),
            album: album.into(),
            album_artist: "Artist".into(),
            duration_ms: 180_000.0,
            app: "test".into(),
        }
    }
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "oly-sync-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn settle(state: &mut SyncState) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut error = None;
        while !state.ready || state.completed < state.sequence {
            if let Some(message) = state.poll() {
                error = Some(message);
            }
            assert!(Instant::now() < deadline, "offset worker did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        error
    }
    #[test]
    fn earlier_and_later_change_the_actual_lyric_offset() {
        let mut state = SyncState::new(false);
        state.select(Some(song("One", "")), "qq");
        assert_eq!(state.nudge(200).unwrap(), 200);
        assert_eq!(state.offset_ms(), 200);
        assert_eq!(state.nudge(-200).unwrap(), 0);
        assert_eq!(state.offset_ms(), 0);
    }
    #[test]
    fn providers_and_song_scopes_are_independent() {
        let mut state = SyncState::new(false);
        state.select(Some(song("One", "")), "qq");
        state.nudge(200).unwrap();
        state.select(Some(song("One", "")), "netease");
        assert_eq!(state.offset_ms(), 0);
        state.nudge(-200).unwrap();
        state.select(Some(song("Two", "")), "qq");
        assert_eq!(state.offset_ms(), 0);
        state.select(Some(song("One", "")), "qq");
        assert_eq!(state.offset_ms(), 200);
    }
    #[test]
    fn album_tracks_share_their_scope() {
        let mut state = SyncState::new(false);
        state.select(Some(song("One", "Album")), "qq");
        state.nudge(200).unwrap();
        state.select(Some(song("Two", "Album")), "qq");
        assert_eq!(state.offset_ms(), 200);
    }
    #[test]
    fn queued_saves_survive_restart_and_do_not_cross_selection() {
        let dir = root();
        let mut state = SyncState::with_root(true, Some(dir.clone()));
        state.select(Some(song("One", "")), "qq");
        settle(&mut state);
        state.nudge(200).unwrap();
        // A repeated player packet must preserve the unconfirmed preview.
        state.select(Some(song("One", "")), "qq");
        assert_eq!(state.offset_ms(), 200);
        state.nudge(200).unwrap();
        state.select(Some(song("Two", "")), "netease");
        settle(&mut state);
        assert_eq!(state.offset_ms(), 0);
        let mut restarted = SyncState::with_root(true, Some(dir.clone()));
        restarted.select(Some(song("One", "")), "qq");
        settle(&mut restarted);
        assert_eq!(restarted.offset_ms(), 400);
        restarted.nudge(-200).unwrap();
        settle(&mut restarted);
        assert_eq!(
            LyricOffsets::load(&dir)
                .unwrap()
                .get(&song("One", ""), "qq")
                .ms,
            200.0
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn failed_save_rolls_preview_back_to_the_last_saved_value() {
        let dir = root();
        let mut state = SyncState::with_root(true, Some(dir.clone()));
        state.select(Some(song("One", "")), "qq");
        settle(&mut state);
        state.nudge(200).unwrap();
        settle(&mut state);
        // A file at the sibling temporary filename reliably rejects saving.
        std::fs::create_dir(dir.join("lyric-offsets.json.tmp")).unwrap();
        state.nudge(200).unwrap();
        assert_eq!(state.offset_ms(), 400);
        assert!(settle(&mut state).is_some());
        assert_eq!(state.offset_ms(), 200);
        assert_eq!(
            LyricOffsets::load(&dir)
                .unwrap()
                .get(&song("One", ""), "qq")
                .ms,
            200.0
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn clear_all_wipes_worker_memory_and_disk() {
        let dir = root();
        let mut state = SyncState::with_root(true, Some(dir.clone()));
        state.select(Some(song("One", "")), "qq");
        settle(&mut state);
        state.nudge(400).unwrap();
        settle(&mut state);
        assert_eq!(state.offset_ms(), 400);

        state.select(Some(song("Two", "")), "netease");
        settle(&mut state);
        state.nudge(-200).unwrap();
        settle(&mut state);
        assert_eq!(state.offset_ms(), -200);

        state.clear_all().unwrap();
        assert_eq!(state.offset_ms(), 0);

        // Switching back to a previously offset song must read 0 from worker memory.
        state.select(Some(song("One", "")), "qq");
        settle(&mut state);
        assert_eq!(state.offset_ms(), 0);

        // Nudging after clear must not resurrect previously cleared tracks on disk.
        state.nudge(100).unwrap();
        settle(&mut state);
        let disk = LyricOffsets::load(&dir).unwrap();
        assert_eq!(disk.get(&song("One", ""), "qq").ms, 100.0);
        assert_eq!(disk.get(&song("Two", ""), "netease").ms, 0.0);
        let _ = std::fs::remove_dir_all(dir);
    }
}

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use oly_core::offsets::{
    offset_scope, LyricOffsets, OffsetContext, OffsetScope, SongRef, MAX_ABS_MS,
};

enum WorkerCmd {
    QueryOrSave(Request),
    ClearAll(Sender<Result<(), String>>),
}

struct Request {
    generation: u64,
    sequence: u64,
    song: SongRef,
    provider: String,
    key: String,
    target: Option<i64>,
}

struct Receipt {
    generation: u64,
    sequence: u64,
    key: String,
    value: i64,
    error: Option<String>,
}

/// UI-owned lyric timing preview. Disk access belongs exclusively to one worker.
/// Add `offset_ms()` to lyric lookup time, never to the playback/SMTC clock.
/// Call `poll()` from the UI tick to receive confirmations and rollback errors.
/// Replay mode stores offsets in memory and never resolves an AppData path.
pub struct SyncState {
    song: Option<SongRef>,
    provider: String,
    context: Option<OffsetContext>,
    memory: BTreeMap<String, i64>,
    draft: i64,
    ready: bool,
    generation: u64,
    sequence: u64,
    completed: u64,
    worker: Option<(Sender<WorkerCmd>, Receiver<Receipt>)>,
}

impl SyncState {
    pub fn new(persistent: bool) -> Self {
        Self::with_root(persistent, None)
    }

    /// Explicit directory is useful for isolated tests. The default is the
    /// AppData directory.
    pub fn with_root(persistent: bool, root: Option<PathBuf>) -> Self {
        let worker = persistent.then(|| {
            let dir = root.or_else(|| {
                std::env::var_os("APPDATA")
                    .map(|root| PathBuf::from(root).join("OverLyrics"))
            });
            let (requests, receiver) = mpsc::channel::<WorkerCmd>();
            let (sender, receipts) = mpsc::channel::<Receipt>();
            // One owner means saves remain ordered and never race on the
            // core store's atomic temporary file. Dropping the UI drains queued
            // requests before exit, without blocking the GUI on a thread join.
            std::thread::spawn(move || {
                let store = dir
                    .as_deref()
                    .ok_or_else(|| "无法确定歌词偏移存储目录".to_string())
                    .and_then(|dir| LyricOffsets::load(dir).map_err(|e| e.to_string()));
                while let Ok(cmd) = receiver.recv() {
                    match cmd {
                        WorkerCmd::ClearAll(reply) => {
                            let res = match &store {
                                Ok(store) => store.clear().map_err(|e| e.to_string()),
                                Err(error) => Err(error.clone()),
                            };
                            let _ = reply.send(res);
                        }
                        WorkerCmd::QueryOrSave(request) => {
                            let (value, error) = match &store {
                                Ok(store) => {
                                    let saved = store.get(&request.song, &request.provider).ms as i64;
                                    match request.target {
                                        Some(target) => match store.nudge(
                                            &request.song,
                                            &request.provider,
                                            (target - saved) as f64,
                                        ) {
                                            Ok(snapshot) => (snapshot.ms as i64, None),
                                            Err(error) => (saved, Some(error.to_string())),
                                        },
                                        None => (saved, None),
                                    }
                                }
                                Err(error) => (0, Some(error.clone())),
                            };
                            if sender
                                .send(Receipt {
                                    generation: request.generation,
                                    sequence: request.sequence,
                                    key: request.key,
                                    value,
                                    error,
                                })
                                .is_err()
                            {
                                // Continue draining writes even if the view was closed.
                            }
                        }
                    }
                }
            });
            (requests, receipts)
        });
        Self {
            song: None,
            provider: String::new(),
            context: None,
            memory: BTreeMap::new(),
            draft: 0,
            ready: !persistent,
            generation: 0,
            sequence: 0,
            completed: 0,
            worker,
        }
    }

    pub fn select(&mut self, song: Option<SongRef>, provider: &str) {
        let provider = provider.trim();
        let same_song=match (&self.song,&song) {
            (None,None)=>true,(Some(a),Some(b))=>a.title==b.title&&a.artist==b.artist&&a.album==b.album&&a.album_artist==b.album_artist&&a.app==b.app,_=>false
        };
        if same_song && self.provider == provider {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.sequence = 0;
        self.completed = 0;
        self.context = song.as_ref().and_then(|song| offset_scope(song, provider));
        self.song = song;
        self.provider = provider.to_string();
        self.draft = 0;
        self.ready = true;
        if let Some(context) = &self.context {
            if self.worker.is_some() {
                self.ready = false;
                if self.send(None).is_err() {
                    self.ready = true;
                }
            } else {
                self.draft = self.memory.get(&context.key).copied().unwrap_or(0);
            }
        }
    }

    fn send(&self, target: Option<i64>) -> Result<(), String> {
        let (Some(song), Some(context), Some((sender, _))) =
            (&self.song, &self.context, &self.worker)
        else {
            return Ok(());
        };
        sender
            .send(WorkerCmd::QueryOrSave(Request {
                generation: self.generation,
                sequence: self.sequence,
                song: song.clone(),
                provider: self.provider.clone(),
                key: context.key.clone(),
                target,
            }))
            .map_err(|_| "歌词偏移保存线程已停止".to_string())
    }

    /// Synchronously clears both the worker's in-memory `LyricOffsets` store
    /// and the on-disk `lyric-offsets.json`, plus any in-memory preview state.
    pub fn clear_all(&mut self) -> Result<(), String> {
        self.memory.clear();
        self.draft = 0;
        self.generation = self.generation.wrapping_add(1);
        self.sequence = 0;
        self.completed = 0;
        self.ready = true;
        if let Some((sender, _)) = &self.worker {
            let (tx, rx) = mpsc::channel();
            sender
                .send(WorkerCmd::ClearAll(tx))
                .map_err(|_| "歌词偏移保存线程已停止".to_string())?;
            rx.recv()
                .map_err(|_| "歌词偏移保存线程已停止".to_string())??;
        }
        Ok(())
    }

    pub fn has_worker(&self) -> bool {
        self.worker.is_some()
    }

    /// Positive advances lyrics; negative delays them. Persistent values are
    /// previews until `poll` receives the worker's atomic-save acknowledgement.
    pub fn nudge(&mut self, delta: i64) -> Result<i64, String> {
        if !self.enabled() {
            return Err("当前没有可调整的歌词来源".to_string());
        }
        let next = self
            .draft
            .saturating_add(delta)
            .clamp(-MAX_ABS_MS, MAX_ABS_MS);
        if next == self.draft {
            return Ok(next);
        }
        if self.worker.is_some() {
            self.sequence += 1;
            if let Err(error) = self.send(Some(next)) {
                self.sequence -= 1;
                return Err(error);
            }
        } else if let Some(context) = &self.context {
            self.memory.insert(context.key.clone(), next);
        }
        self.draft = next;
        Ok(next)
    }

    /// Receipts are guarded by both selection generation and storage key.
    /// A superseded receipt cannot change the new selection or its preview.
    pub fn poll(&mut self) -> Option<String> {
        let mut error = None;
        let Some((_, receiver)) = &self.worker else {
            return None;
        };
        while let Ok(receipt) = receiver.try_recv() {
            if receipt.generation != self.generation
                || self.context.as_ref().map(|context| &context.key) != Some(&receipt.key)
            {
                continue;
            }
            self.ready = true;
            self.completed = self.completed.max(receipt.sequence);
            if receipt.sequence == self.sequence {
                self.draft = receipt.value;
                if receipt.error.is_some() {
                    error = receipt.error;
                }
            }
        }
        error
    }

    pub fn offset_ms(&self) -> i64 {
        self.draft
    }

    pub fn enabled(&self) -> bool {
        self.ready && self.context.is_some()
    }

    pub fn label(&self) -> String {
        format!("{:+.1} 秒", self.draft as f64 / 1000.0)
    }

    pub fn scope_label(&self) -> String {
        match &self.context {
            Some(context) if context.scope == OffsetScope::Album => {
                format!("整张专辑 · {}", context.album)
            }
            Some(_) => "当前歌曲".to_string(),
            None => "暂无歌词".to_string(),
        }
    }
}
