//! Live-session model and one bounded background search worker.
use oly_contracts::{LyricDoc, PlayerPacket};
use oly_core::transport::TransportClock;
use oly_engine::{
    net::Transport,
    service::{LyricsService, SearchFlags},
    types::{Candidate, Track},
};
use std::sync::{Arc, Condvar, Mutex};

const CONTROL_PREVIEW_MS: f64 = 15_000.0;

/// Display-only prediction. Raw packets and the source clock remain available
/// for rollback when dispatch fails, the player refuses, or confirmation expires.
struct ControlPreview {
    id: i64,
    generation: u64,
    playing: bool,
    media: f64,
    lyric_offset: f64,
    at: f64,
    rate: f64,
    duration: f64,
    source: String,
    accepted: bool,
    saw_opposite: bool,
    confirmed: bool,
}
impl ControlPreview {
    fn position(&self, now: f64) -> f64 {
        let elapsed = if self.playing { (now - self.at).max(0.0) * self.rate } else { 0.0 };
        let position = (self.media + elapsed).max(0.0);
        if self.duration > 0.0 { position.min(self.duration + 4000.0) } else { position }
    }
}

/// Keep the source's duplicate-sample identity after a continuous preview
/// handoff. A repeated coarse position must not look like a backward seek
/// merely because the display was rebased ahead of that position.
struct ConfirmedSample {
    raw: f64,
    displayed: f64,
    source: String,
    status: Option<oly_contracts::PlaybackStatus>,
}

#[derive(Default)]
pub struct LiveModel {
    pub packet: Option<PlayerPacket>,
    clock: TransportClock,
    generation: u64,
    search_key: String,
    next_control_id: i64,
    pending_control: Option<(i64, u64)>,
    preview: Option<ControlPreview>,
    confirmed_sample: Option<ConfirmedSample>,
    last_now: f64,
    song_started_at: f64,
    initial_zero_reanchored: bool,
}
impl LiveModel {
    /// Call after `prepare_control` succeeds, before dispatch, using the same
    /// monotonic millisecond base as feed/tick. A failed dispatch must report
    /// `control_result(request_id, false)` just like a player refusal.
    pub fn preview_control(&mut self, action: oly_platform_win::smtc::control::Action, now: f64) {
        use oly_platform_win::smtc::control::Action;
        self.expire_preview(now);
        let playing = match action {
            Action::Play => true,
            Action::Pause => false,
            _ => return,
        };
        let Some((id, generation)) = self.pending_control else { return; };
        if !self.accepts(generation) { return; }
        let Some(packet) = self.packet.as_ref() else { return; };
        let media = self.media_position(now);
        let lyric_offset = self.position(now) - media;
        // On a quick pause/play, a still-Playing packet belongs to the old
        // state. Wait for the opposite state before accepting it as confirmation.
        let saw_opposite = packet.is_playing() != playing || self.preview.is_none();
        self.preview = Some(ControlPreview {
            id, generation, playing, media, lyric_offset, at: now,
            rate: if packet.rate.is_finite() && packet.rate > 0.0 { packet.rate } else { 1.0 },
            duration: packet.duration_ms,
            source: packet.timeline_source.clone(), accepted: false,
            saw_opposite, confirmed: false,
        });
    }
    pub fn effective_playing(&self) -> bool {
        self.preview.as_ref().map_or_else(
            || self.packet.as_ref().is_some_and(PlayerPacket::is_playing),
            |preview| preview.playing,
        )
    }
    fn expire_preview(&mut self, now: f64) {
        self.last_now = now;
        if self.preview.as_ref().is_some_and(|p| {
            p.generation != self.generation || now - p.at >= CONTROL_PREVIEW_MS
        }) {
            let expired = self.preview.take().expect("preview expired");
            if self.pending_control.is_some_and(|(id, _)| id == expired.id) {
                self.pending_control = None;
            }
        }
    }
    fn finish_preview(&mut self, now: f64) {
        let ready = self.preview.as_ref().is_some_and(|p| p.accepted && p.confirmed);
        if !ready { return; }
        let preview = self.preview.take().expect("preview was ready");
        if let Some(mut packet) = self.packet.clone() {
            // Rebase only the display clock. Never rewrite source metadata.
            let displayed = preview.position(now);
            self.confirmed_sample = Some(ConfirmedSample {
                raw: packet.position_ms,
                displayed,
                source: packet.timeline_source.clone(),
                status: packet.status.clone(),
            });
            packet.position_ms = displayed;
            packet.last_updated_unix_ms = now;
            packet.track_changed = false;
            packet.position_discontinuity = false;
            let offset = self.clock.song_offset_ms();
            self.clock = TransportClock::new();
            self.clock.set_song_offset(offset);
            self.clock.feed(Some(&packet), now);
        }
    }

    pub fn prepare_control(
        &mut self,
        action: oly_platform_win::smtc::control::Action,
        now: i64,
    ) -> Option<oly_platform_win::smtc::control::ControlRequest> {
        use oly_platform_win::smtc::control::{Action, ControlRequest, SeekGuard};
        let packet = self.packet.as_ref()?;
        let enabled = match action {
            Action::Next => packet.ctrl.next,
            Action::Prev => packet.ctrl.prev,
            _ => packet.ctrl.playpause,
        };
        if !enabled || packet.title.is_empty() || packet.app.is_empty() {
            return None;
        }
        self.next_control_id = self.next_control_id.saturating_add(1);
        self.pending_control = Some((self.next_control_id, self.generation));
        Some(ControlRequest {
            action,
            request_id: self.next_control_id,
            identity: SeekGuard {
                app: packet.app.clone(),
                title: packet.title.clone(),
                artist: packet.artist.clone(),
                album: packet.album.clone(),
            },
            deadline_unix_ms: now.saturating_add(1500),
        })
    }
    pub fn can_seek(&self) -> bool {
        self.packet
            .as_ref()
            .is_some_and(|p| !p.idle && p.can_seek && p.duration_ms > 0.0)
    }
    pub fn prepare_seek(
        &mut self,
        ms: f64,
        now_unix_ms: i64,
    ) -> Option<oly_platform_win::smtc::control::SeekRequest> {
        use oly_platform_win::smtc::control::SeekRequest;
        let packet = self.packet.as_ref()?;
        if packet.idle
            || !packet.can_seek
            || packet.title.is_empty()
            || packet.app.is_empty()
            || !ms.is_finite()
        {
            return None;
        }
        let target_ms = if packet.duration_ms > 0.0 {
            ms.clamp(0.0, packet.duration_ms)
        } else {
            ms.max(0.0)
        };
        self.next_control_id = self.next_control_id.saturating_add(1);
        Some(SeekRequest {
            request_id: self.next_control_id,
            ms: target_ms.round() as i64,
            app: packet.app.clone(),
            title: packet.title.clone(),
            artist: packet.artist.clone(),
            album: packet.album.clone(),
            deadline_unix_ms: now_unix_ms.saturating_add(2500),
        })
    }
    pub fn control_result(&mut self, id: i64, accepted: bool) -> Option<bool> {
        let (pending, generation) = self.pending_control?;
        if id != pending {
            return None;
        }
        self.pending_control = None;
        if !self.accepts(generation) { return None; }
        if self.preview.as_ref().is_some_and(|preview| preview.id == id) {
            if accepted {
                self.preview.as_mut().expect("preview exists").accepted = true;
                self.finish_preview(self.last_now);
            } else {
                self.preview = None;
            }
        }
        Some(accepted)
    }
    /// Whether a dispatched transport action is awaiting its receipt.
    pub fn control_pending(&self) -> bool {
        self.pending_control.is_some_and(|(_, generation)| self.accepts(generation))
    }
    /// Media time before lyric synchronization or manual delay offsets.
    pub fn media_position(&self, now: f64) -> f64 {
        self.preview.as_ref().map_or_else(|| self.clock.local_pos_at(now), |p| p.position(now))
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn accepts(&self, generation: u64) -> bool {
        self.packet.is_some() && self.generation == generation
    }
    pub fn feed(&mut self, mut packet: PlayerPacket, now: f64) -> Option<Track> {
        self.expire_preview(now);
        if packet.is_idle_state() {
            self.idle();
            return None;
        }
        let duration = if packet.duration_ms.is_finite() {
            (packet.duration_ms.max(0.0) / 1000.0).round() as u64
        } else {
            0
        };
        let key = format!(
            "{}\u{1}{}\u{1}{}\u{1}{}",
            packet.track_key(),
            packet.album,
            packet.album_artist,
            duration
        );
        let changed = key != self.search_key;
        let song_changed = self
            .packet
            .as_ref()
            .is_none_or(|old| old.track_key() != packet.track_key());
        if !song_changed && !packet.track_changed && !packet.cover_updated && packet.cover.is_none()
        {
            packet.cover = self.packet.as_mut().and_then(|old| old.cover.take());
        }
        if changed || packet.track_changed || packet.position_available == Some(false) {
            self.preview = None;
            self.pending_control = None;
            self.confirmed_sample = None;
        }
        let had_prev_song = self.packet.is_some();
        let prev_positions = self
            .packet
            .as_ref()
            .filter(|_| song_changed)
            .map(|p| (p.position_ms, self.clock.local_pos_at(now)));
        if song_changed {
            self.clock = TransportClock::new();
            self.song_started_at = now;
            self.initial_zero_reanchored = false;
        }
        // Guard against stale SMTC timeline metadata when a new track starts:
        // players frequently keep the previous track's `last_updated_unix_ms`
        // (or momentarily still carry its high `position_ms`) right at track swap.
        let effective_start = if self.song_started_at > 0.0 {
            self.song_started_at
        } else {
            now
        };
        if song_changed || packet.track_changed {
            let mut clamped_prev_pos = false;
            if let Some((prev_raw, prev_clock)) = prev_positions {
                let matches_prev = |prev: f64| {
                    prev > 5000.0 && (packet.position_ms - prev).abs() < 2500.0
                };
                if packet.position_ms > 5000.0
                    && (matches_prev(prev_raw) || matches_prev(prev_clock))
                {
                    packet.position_ms = 0.0;
                    packet.position_discontinuity = true;
                    clamped_prev_pos = true;
                }
            }
            let age_ms = now - packet.last_updated_unix_ms;
            let stale_ts = clamped_prev_pos
                || packet.last_updated_unix_ms <= 0.0
                || (had_prev_song
                    && (age_ms > 1500.0 || (packet.position_ms <= 500.0 && age_ms > 150.0)))
                || (!had_prev_song && packet.position_ms <= 500.0 && age_ms > 2500.0);
            if stale_ts {
                packet.last_updated_unix_ms = now;
            }
        } else {
            if packet.position_ms <= 1000.0
                && packet.last_updated_unix_ms > 0.0
                && packet.last_updated_unix_ms < effective_start - 1500.0
            {
                packet.last_updated_unix_ms = effective_start;
            }
            if packet.position_ms > 250.0 {
                self.initial_zero_reanchored = true;
            } else if !self.initial_zero_reanchored && (now - effective_start) <= 6000.0 {
                let incoming_now =
                    packet.position_ms + (now - packet.last_updated_unix_ms).max(0.0);
                if self.clock.local_pos_at(now) > incoming_now + 150.0
                    && self.packet.as_ref().is_some_and(|old| {
                        old.position_ms <= 250.0
                            && packet.last_updated_unix_ms > old.last_updated_unix_ms
                    })
                {
                    packet.position_discontinuity = true;
                }
            }
        }
        let display_packet = self.confirmed_sample.as_ref().and_then(|sample| {
            (packet.position_ms == sample.raw
                && packet.timeline_source == sample.source
                && packet.status == sample.status
                && !packet.position_discontinuity)
                .then(|| {
                    let mut display = packet.clone();
                    display.position_ms = sample.displayed;
                    display
                })
        });
        if display_packet.is_none() {
            self.confirmed_sample = None;
        }
        self.clock.feed(Some(display_packet.as_ref().unwrap_or(&packet)), now);
        let track = Track {
            title: packet.title.clone(),
            artist: Some(packet.artist.clone()),
            album: Some(packet.album.clone()),
            album_artists: Some(vec![packet.album_artist.clone()]),
            duration_ms: Some(packet.duration_ms),
            ..Default::default()
        };
        if let Some(preview) = self.preview.as_mut() {
            preview.confirmed = false;
            if packet.timeline_source == preview.source && packet.status.is_some() {
                let matching = packet.is_playing() == preview.playing;
                preview.saw_opposite |= !matching;
                // Settle coalesced back-to-back toggles that land back on the same state.
                preview.confirmed =
                    matching && (preview.saw_opposite || now - preview.at >= 2000.0);
            }
        }
        self.packet = Some(packet);
        self.finish_preview(now);
        if changed {
            self.search_key = key;
            self.generation += 1;
            Some(track)
        } else {
            None
        }
    }
    pub fn idle(&mut self) {
        self.confirmed_sample = None;
        self.preview = None;
        self.pending_control = None;
        if self.packet.is_some() {
            self.generation += 1;
        }
        self.packet = None;
        self.search_key.clear();
        self.song_started_at = 0.0;
        self.initial_zero_reanchored = false;
        self.clock = TransportClock::new();
    }
    pub fn tick(&mut self, now: f64) {
        self.expire_preview(now);
        self.clock.tick(now, None);
    }
    pub fn position(&self, now: f64) -> f64 {
        self.preview.as_ref().map_or_else(|| self.clock.lyric_position(now), |p| p.position(now) + p.lyric_offset)
    }
}

fn candidate_percent_value(c: &Candidate) -> Option<f64> {
    c.match_info
        .map(|m| m.percent)
        .or(c.percent)
        .filter(|p| p.is_finite())
}

fn candidate_duration_ms_value(c: &Candidate) -> Option<f64> {
    c.duration_sec
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| d * 1000.0)
        .or_else(|| c.duration_ms.filter(|d| d.is_finite() && *d > 0.0))
}

fn is_qualified_pick(c: &Candidate, track: &Track, threshold: f64) -> bool {
    let pct_ok = c.rescue == Some(true)
        || candidate_percent_value(c).is_some_and(|p| p >= threshold);
    if !pct_ok {
        return false;
    }
    if let (Some(song_ms), Some(cand_ms)) = (
        track.duration_ms.filter(|d| d.is_finite() && *d > 0.0),
        candidate_duration_ms_value(c),
    ) {
        if (cand_ms - song_ms).abs() > 8000.0 {
            return false;
        }
    }
    true
}

/// Late collection upgrades when a candidate is strictly higher content tier,
/// or when an unqualified early pick is superseded by a qualified / much
/// higher-match candidate in the same tier.
fn prefer_progressive_pick(
    current: Option<Candidate>,
    incoming: Option<Candidate>,
    track: &Track,
    threshold: f64,
) -> Option<Candidate> {
    match (current, incoming) {
        (Some(current), Some(incoming)) => {
            let cur_ord = oly_engine::picker::group_order(&current);
            let inc_ord = oly_engine::picker::group_order(&incoming);
            if inc_ord < cur_ord {
                Some(incoming)
            } else if inc_ord > cur_ord {
                Some(current)
            } else {
                let cur_ok = is_qualified_pick(&current, track, threshold);
                let inc_ok = is_qualified_pick(&incoming, track, threshold);
                if !cur_ok && inc_ok {
                    Some(incoming)
                } else if cur_ok == inc_ok
                    && candidate_percent_value(&incoming).unwrap_or(0.0)
                        >= candidate_percent_value(&current).unwrap_or(0.0) + 15.0
                {
                    Some(incoming)
                } else {
                    Some(current)
                }
            }
        }
        (_, Some(incoming)) => Some(incoming),
        (current, None) => current,
    }
}

pub struct SearchAnswer {
    pub selected:Option<(String,String)>,
    pub provider:String,
    pub generation: u64,
    epoch: u64,
    pub auto_picked: bool,
    pub manual: bool,
    pub searching: bool,
    pub candidates: Vec<Candidate>,
    pub doc: Option<LyricDoc>,
    pub error: String,
}
struct Job {
    generation: u64,
    epoch: u64,
    track: Track,
    candidate: Option<Candidate>,
}
#[derive(Default)]
struct Pending {
    job: Option<Job>,
    promote: Option<(Track, String, String)>,
    stop: bool,
}
/// One in-flight request and one replaceable pending job. Dropping the view
/// signals shutdown; a currently blocked HTTP call exits on its own timeout.
pub fn resolve_track_artist_alias(config: &serde_json::Value, track: &Track) -> Track {
    let Some(aliases) = config
        .get("lyrics")
        .and_then(|l| l.get("artistAliases"))
        .and_then(|v| v.as_object())
    else {
        return track.clone();
    };
    let primary = oly_engine::strings::primary_artist(track);
    let raw = primary.trim();
    if raw.is_empty() {
        return track.clone();
    }
    let mapped = aliases.iter().find_map(|(k, v)| {
        let key = k.trim();
        let val = v.as_str().unwrap_or("").trim();
        if !key.is_empty() && !val.is_empty() && key.eq_ignore_ascii_case(raw) {
            Some(val.to_string())
        } else {
            None
        }
    });
    if let Some(mapped) = mapped {
        let mut out = track.clone();
        out.artist = Some(mapped.clone());
        out.artists = Some(vec![mapped]);
        out
    } else {
        track.clone()
    }
}

pub struct SearchWorker {
    service: Arc<LyricsService>,
    pending: Arc<(Mutex<Pending>, Condvar)>,
    answer: Arc<Mutex<Option<SearchAnswer>>>,
}
impl SearchWorker {
    pub fn start(service: Arc<LyricsService>, transport: Arc<dyn Transport>) -> Self {
        Self::start_inner(service, transport, None)
    }
    pub fn start_with_config(
        service: Arc<LyricsService>,
        transport: Arc<dyn Transport>,
        config: Arc<oly_core::config::ConfigStore>,
    ) -> Self {
        Self::start_inner(service, transport, Some(config))
    }
    fn start_inner(
        service: Arc<LyricsService>,
        transport: Arc<dyn Transport>,
        config: Option<Arc<oly_core::config::ConfigStore>>,
    ) -> Self {
        let pending = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let answer = Arc::new(Mutex::new(None));
        let (tasks, out, engine) = (pending.clone(), answer.clone(), service.clone());
        std::thread::Builder::new()
            .name("lyrics-live".into())
            .spawn(move || {
                loop {
                    let (job, promote) = {
                        let (lock, wake) = &*tasks;
                        let mut p = lock.lock().unwrap_or_else(|e| e.into_inner());
                        while p.job.is_none() && p.promote.is_none() && !p.stop {
                            p = wake.wait(p).unwrap_or_else(|e| e.into_inner());
                        }
                        if p.stop {
                            break;
                        }
                        (p.job.take(), p.promote.take())
                    };
                    if let Some((track, source, id)) = promote {
                        let resolved = config
                            .as_ref()
                            .map(|c| resolve_track_artist_alias(&c.get(), &track))
                            .unwrap_or(track);
                        engine.promote_source_memory(&resolved, &source, &id);
                    }
                    let Some(mut job) = job else {
                        continue;
                    };
                    let raw_artist = oly_engine::strings::primary_artist(&job.track);
                    if let Some(c) = config.as_ref() {
                        job.track = resolve_track_artist_alias(&c.get(), &job.track);
                    }
                    if engine.epoch() != job.epoch {
                        continue;
                    }
                    let mut rows = Vec::new();
                    let manual = job.candidate.is_some();
                    let threshold = engine.threshold();
                    let mut delivered_pick: Option<Candidate> = None;
                    let mut cached_doc_on_screen = false;
                    let pick = if let Some(c) = job.candidate {
                        Some(c)
                    } else {
                        let remembered = engine.remembered_manual_source(&job.track);
                        if let Some((ref source, Some(ref cached_cand))) = remembered {
                            if let Some(value) = engine.load_cached_doc(&job.track, source, &cached_cand.id) {
                                let provider = value.get("provider").and_then(|v| v.as_str()).unwrap_or(source).to_string();
                                let id = value.get("id").and_then(|v| v.as_str()).unwrap_or(&cached_cand.id).to_string();
                                if let Ok(doc) = serde_json::from_value(value) {
                                    let mut actual_pick = cached_cand.clone();
                                    actual_pick.id = id.clone();
                                    delivered_pick = Some(actual_pick.clone());
                                    cached_doc_on_screen = true;
                                    if engine.epoch() == job.epoch {
                                        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(SearchAnswer {
                                            generation: job.generation,
                                            epoch: job.epoch,
                                            auto_picked: false,
                                            manual: false,
                                            searching: true,
                                            provider,
                                            selected: Some((source.clone(), id)),
                                            candidates: vec![actual_pick],
                                            doc: Some(doc),
                                            error: String::new(),
                                        });
                                    }
                                }
                            }
                        }
                        let mut progress = |pool: &[Candidate]| {
                            if engine.epoch() != job.epoch {
                                return;
                            }
                            let best_by_tier = oly_engine::arbiter::judge_with_threshold(pool, &job.track, threshold).pick;
                            let candidate = if let Some((source, old)) = &remembered {
                                let rem = pool
                                    .iter()
                                    .find(|c| {
                                        &c.source == source
                                            && old.as_ref().is_none_or(|old| old.id == c.id)
                                            && c.verified.is_some()
                                            && c.probe_failed != Some(true)
                                    })
                                    .cloned();
                                match (&delivered_pick, rem, best_by_tier) {
                                    (Some(cur), _, Some(best))
                                        if oly_engine::picker::group_order(&best)
                                            < oly_engine::picker::group_order(cur) =>
                                    {
                                        Some(best)
                                    }
                                    (None, Some(r), Some(best))
                                        if oly_engine::picker::group_order(&best)
                                            < oly_engine::picker::group_order(&r) =>
                                    {
                                        Some(best)
                                    }
                                    (None, Some(r), _) => Some(r),
                                    (None, None, best) => best,
                                    (Some(_), _, _) => None,
                                }
                            } else {
                                best_by_tier
                            };
                            let Some(candidate) = candidate else { return; };
                            let is_remembered_hit = remembered.as_ref().is_some_and(|(s, o)| {
                                s == &candidate.source && o.as_ref().is_none_or(|old| old.id == candidate.id)
                            });
                            if !is_remembered_hit && !is_qualified_pick(&candidate, &job.track, threshold) {
                                return;
                            }
                            if let Some(cur) = delivered_pick.as_ref() {
                                let cur_ord = oly_engine::picker::group_order(cur);
                                let new_ord = oly_engine::picker::group_order(&candidate);
                                if new_ord > cur_ord {
                                    return;
                                }
                                if new_ord == cur_ord {
                                    let cur_ok = is_qualified_pick(cur, &job.track, threshold);
                                    let new_ok = is_qualified_pick(&candidate, &job.track, threshold);
                                    let much_better = candidate_percent_value(&candidate).unwrap_or(0.0)
                                        >= candidate_percent_value(cur).unwrap_or(0.0) + 15.0;
                                    if !((!cur_ok && new_ok) || much_better) {
                                        return;
                                    }
                                }
                            }
                            let auto_picked = remembered.is_none()
                                || remembered.as_ref().is_some_and(|(s, o)| {
                                    s != &candidate.source || o.as_ref().is_some_and(|old| old.id != candidate.id)
                                });
                            let chosen = engine.use_candidate(
                                transport.as_ref(),
                                &job.track,
                                &candidate.source,
                                &candidate,
                                job.epoch,
                                auto_picked,
                                false,
                            );
                            if chosen.stale || !chosen.ok {
                                return;
                            }
                            if let Some(value) = chosen.doc {
                                let provider = value.get("provider").and_then(|v| v.as_str()).unwrap_or(&candidate.source).to_string();
                                let id = value.get("id").and_then(|v| v.as_str()).unwrap_or(&candidate.id).to_string();
                                if let Ok(doc) = serde_json::from_value(value) {
                                    let mut actual_pick = candidate.clone();
                                    actual_pick.id = id.clone();
                                    let mut snap_candidates = pool.to_vec();
                                    if !snap_candidates.iter().any(|c| c.source == actual_pick.source && c.id == actual_pick.id) {
                                        snap_candidates.push(actual_pick.clone());
                                    }
                                    delivered_pick = Some(actual_pick);
                                    cached_doc_on_screen = false;
                                    if engine.epoch() == job.epoch {
                                        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(SearchAnswer {
                                            generation: job.generation,
                                            epoch: job.epoch,
                                            auto_picked,
                                            manual: false,
                                            searching: true,
                                            provider,
                                            selected: Some((candidate.source, id)),
                                            candidates: snap_candidates,
                                            doc: Some(doc),
                                            error: String::new(),
                                        });
                                    }
                                }
                            }
                        };
                        let found = engine.search_all_with_progress(transport.as_ref(), &job.track, SearchFlags::default(), job.epoch, &mut progress);
                        if found.cancelled || engine.epoch() != job.epoch {
                            continue;
                        }
                        rows = found.list;
                        let initial_judge = oly_engine::arbiter::judge_with_threshold(&rows, &job.track, threshold);
                        let needs_rescue = initial_judge.best_grade < oly_engine::arbiter::GRADE_WORD
                            && (!oly_engine::crosslang::CJK_RE.is_match(&job.track.title)
                                || !oly_engine::crosslang::CJK_RE.is_match(&raw_artist));
                        if needs_rescue {
                            let rescue = engine.cross_lang_rescue(
                                transport.as_ref(),
                                &job.track,
                                Some(&raw_artist),
                                &rows,
                                job.epoch,
                            );
                            if rescue.cancelled || engine.epoch() != job.epoch {
                                continue;
                            }
                            if !rescue.injected.is_empty() {
                                for r in rescue.injected {
                                    if !rows.iter().any(|e| e.source == r.source && e.id == r.id) {
                                        rows.push(r);
                                    }
                                }
                                rows.sort_by(oly_engine::picker::cand_sort);
                            }
                        }
                        let remembered = engine.remembered_manual_source(&job.track);
                        let rem = remembered.as_ref().and_then(|(source, old)| {
                            rows.iter()
                                .find(|c| {
                                    &c.source == source
                                        && old.as_ref().is_none_or(|o| o.id == c.id)
                                })
                                .cloned()
                        });
                        let judged = oly_engine::arbiter::judge_with_threshold(&rows, &job.track, threshold).pick;
                        match (rem, judged) {
                            (Some(r), Some(j))
                                if oly_engine::picker::group_order(&j)
                                    < oly_engine::picker::group_order(&r) =>
                            {
                                Some(j)
                            }
                            (Some(r), _) => Some(r),
                            (None, j) => j,
                        }
                    };
                    let pick = if manual {
                        pick
                    } else {
                        match (&delivered_pick, &pick) {
                            (Some(d), Some(p)) if d.source == p.source && d.id == p.id => Some(d.clone()),
                            (Some(d), Some(p)) => prefer_progressive_pick(Some(d.clone()), Some(p.clone()), &job.track, threshold),
                            (None, Some(p)) => Some(p.clone()),
                            (Some(d), None) if cached_doc_on_screen || rows.iter().any(|r| r.source == d.source && r.id == d.id) => Some(d.clone()),
                            _ => prefer_progressive_pick(None, pick, &job.track, threshold),
                        }
                    };
                    let mut provider = String::new();
                    let mut selected = None;
                    let remembered_after = engine.remembered_manual_source(&job.track);
                    let auto_picked = !manual
                        && match (&remembered_after, &pick) {
                            (Some((s, o)), Some(p)) => {
                                s != &p.source || o.as_ref().is_some_and(|old| old.id != p.id)
                            }
                            _ => remembered_after.is_none(),
                        };
                    let (doc, error) = if let Some(c) = pick {
                        let same_cached_as_delivered = !manual
                            && cached_doc_on_screen
                            && delivered_pick
                                .as_ref()
                                .is_some_and(|d| d.source == c.source && d.id == c.id);
                        if same_cached_as_delivered {
                            if !rows.iter().any(|r| r.source == c.source && r.id == c.id) {
                                rows.push(c.clone());
                            }
                            selected = Some((c.source.clone(), c.id.clone()));
                            provider = c.source.clone();
                            (
                                engine
                                    .load_cached_doc(&job.track, &c.source, &c.id)
                                    .and_then(|v| serde_json::from_value(v).ok()),
                                String::new(),
                            )
                        } else {
                            let chosen = engine.use_candidate(
                                transport.as_ref(),
                                &job.track,
                                &c.source,
                                &c,
                                job.epoch,
                                auto_picked,
                                manual,
                            );
                            if chosen.stale {
                                continue;
                            }
                            match chosen.doc {
                                Some(value) if chosen.ok => {
                                    let chosen_id = value.get("id").and_then(|v| v.as_str()).unwrap_or(&c.id).to_string();
                                    selected = Some((c.source.clone(), chosen_id.clone()));
                                    provider = value.get("provider").and_then(|v| v.as_str()).unwrap_or(&c.source).to_string();
                                    if !rows.iter().any(|r| r.source == c.source && r.id == chosen_id) {
                                        let mut kept = c.clone();
                                        kept.id = chosen_id;
                                        rows.push(kept);
                                    }
                                    match serde_json::from_value(value) {
                                        Ok(doc) => (Some(doc), String::new()),
                                        Err(e) => (None, format!("歌词格式错误：{e}")),
                                    }
                                }
                                _ => {
                                    if let Some(prev) = delivered_pick.as_ref().filter(|_| !manual) {
                                        if !rows.iter().any(|r| r.source == prev.source && r.id == prev.id) {
                                            rows.push(prev.clone());
                                        }
                                    }
                                    (None, chosen.error)
                                }
                            }
                        }
                    } else {
                        (None, "未找到匹配歌词".into())
                    };
                    if engine.epoch() == job.epoch {
                        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(SearchAnswer {
                            generation: job.generation,
                            epoch: job.epoch,
                            auto_picked,
                            manual,
                            searching: false,
                            provider,
                            selected,
                            candidates: rows,
                            doc,
                            error,
                        });
                    }
                }
            })
            .expect("start lyric worker");
        Self {
            service,
            pending,
            answer,
        }
    }
    pub fn clear_cache(&self) {
        self.service.clear_search_cache();
    }

    pub fn request(&self, generation: u64, track: Track, candidate: Option<Candidate>) {
        let epoch = self.service.invalidate();
        *self.answer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let (lock, wake) = &*self.pending;
        lock.lock().unwrap_or_else(|e| e.into_inner()).job = Some(Job {
            generation,
            epoch,
            track,
            candidate,
        });
        wake.notify_one();
    }
    pub fn cancel(&self) {
        self.service.invalidate();
        self.pending.0.lock().unwrap_or_else(|e| e.into_inner()).job = None;
        *self.answer.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    pub fn promote_pick(&self, track: Track, source: String, id: String) {
        let (lock, wake) = &*self.pending;
        lock.lock().unwrap_or_else(|e| e.into_inner()).promote = Some((track, source, id));
        wake.notify_one();
    }
    pub fn take(&self) -> Option<SearchAnswer> {
        self.answer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .filter(|a| a.epoch == self.service.epoch())
    }
}
impl Drop for SearchWorker {
    fn drop(&mut self) {
        self.cancel();
        self.pending
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stop = true;
        self.pending.1.notify_one();
    }
}

#[cfg(windows)]
const PLAYER_VOLUME_PS1: &str = include_str!("../assets/PlayerVolume.ps1");

/// Resolve `PlayerVolume.ps1` beside the binary first, or materialize our bundled copy
/// under the isolated experiment data root so dev/release builds always support UIA volume.
#[cfg(windows)]
pub fn resolve_player_volume_script(data_root: &std::path::Path) -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for rel in ["resources/PlayerVolume.ps1", "PlayerVolume.ps1"] {
                let candidate = dir.join(rel);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let dir = data_root.join("resources");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("player volume resources dir: {e}");
        return None;
    }
    let dest = dir.join("PlayerVolume.ps1");
    let up_to_date = std::fs::read(&dest)
        .map(|existing| existing == PLAYER_VOLUME_PS1.as_bytes())
        .unwrap_or(false);
    if !up_to_date {
        if let Err(e) = std::fs::write(&dest, PLAYER_VOLUME_PS1.as_bytes()) {
            log::warn!("write PlayerVolume.ps1: {e}");
            return dest.is_file().then_some(dest);
        }
    }
    Some(dest)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionAnswer {
    pub generation: u64,
    pub track_key: String,
    pub file: Option<std::path::PathBuf>,
    pub collection_id: Option<String>,
    pub album_hit: bool,
}

#[derive(Clone)]
struct MotionJob {
    generation: u64,
    track_key: String,
    title: String,
    artist: String,
    album: String,
    enabled: bool,
    max_cache_mb: u64,
}

#[derive(Default)]
struct MotionState {
    job: Option<MotionJob>,
    stop: bool,
}

/// Bounded latest-wins background resolver for Apple Music animated album covers.
#[cfg(windows)]
pub struct MotionWorker {
    pending: Arc<(Mutex<MotionState>, Condvar)>,
    answer: Arc<Mutex<Option<MotionAnswer>>>,
}

#[cfg(windows)]
impl MotionWorker {
    pub fn start(cache_dir: std::path::PathBuf) -> Self {
        let pending = Arc::new((Mutex::new(MotionState::default()), Condvar::new()));
        let answer = Arc::new(Mutex::new(None));
        let p = pending.clone();
        let out = answer.clone();
        std::thread::Builder::new()
            .name("motion-cover-live".into())
            .spawn(move || {
                let transport = oly_platform_win::http::NativeHttp;
                let store = oly_engine::motion_cover::DiskMotionStore::new(
                    oly_platform_win::http::download_motion_mp4,
                );
                let mut resolver =
                    oly_engine::motion_cover::MotionCoverResolver::new(cache_dir, store);
                loop {
                    let job = {
                        let (lock, wake) = &*p;
                        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
                        while !state.stop && state.job.is_none() {
                            state = wake.wait(state).unwrap_or_else(|e| e.into_inner());
                        }
                        if state.stop {
                            return;
                        }
                        state.job.take().expect("motion job")
                    };
                    let input = oly_engine::motion_cover::MotionTrackInput {
                        title: job.title,
                        artist: job.artist,
                        album: job.album,
                    };
                    let resolved = resolver.resolve(
                        &input,
                        job.enabled,
                        job.max_cache_mb.max(50),
                        &transport,
                    );
                    let superseded = p
                        .0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .job
                        .is_some();
                    if !superseded {
                        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(MotionAnswer {
                            generation: job.generation,
                            track_key: job.track_key,
                            file: resolved.file,
                            collection_id: resolved.collection_id,
                            album_hit: resolved.album_hit,
                        });
                    }
                }
            })
            .expect("start motion cover worker");
        Self { pending, answer }
    }

    pub fn request(
        &self,
        generation: u64,
        track_key: String,
        title: String,
        artist: String,
        album: String,
        enabled: bool,
        max_cache_mb: u64,
    ) {
        *self.answer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let (lock, wake) = &*self.pending;
        lock.lock().unwrap_or_else(|e| e.into_inner()).job = Some(MotionJob {
            generation,
            track_key,
            title,
            artist,
            album,
            enabled,
            max_cache_mb,
        });
        wake.notify_one();
    }

    pub fn cancel(&self) {
        self.pending
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .job = None;
        *self.answer.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn take(&self) -> Option<MotionAnswer> {
        self.answer.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

#[cfg(windows)]
impl Drop for MotionWorker {
    fn drop(&mut self) {
        self.cancel();
        self.pending
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stop = true;
        self.pending.1.notify_one();
    }
}

#[cfg(windows)]
pub struct LiveSession {
    pub config: Arc<oly_core::config::ConfigStore>,
    pub model: LiveModel,
    pub host: crate::commands::ShellHost,
    pub inbox: Arc<Mutex<crate::commands::Inbox>>,
    pub search: SearchWorker,
    pub track: Option<Track>,
    pub candidates: Vec<Candidate>,
    pub status: String,
    pub artwork: ArtworkWorker,
    pub motion: MotionWorker,
    pub custom_lyrics: oly_engine::custom_lyrics::CustomLyricsStore,
    pub media_sessions: Vec<oly_platform_win::smtc::MediaSessionInfo>,
    pub pinned_media_app: Option<String>,
    cover_requested: Option<(u64, Option<String>)>,
}
#[cfg(windows)]
impl LiveSession {
    pub fn import_custom_lyric(
        &mut self,
        path: &std::path::Path,
    ) -> Result<oly_engine::custom_lyrics::CustomLyricEntry, String> {
        let packet = self
            .model
            .packet
            .as_ref()
            .filter(|p| !p.is_idle_state())
            .ok_or_else(|| "当前没有正在播放的曲目".to_string())?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        self.custom_lyrics.import_file(
            &packet.track_key(),
            &packet.title,
            &packet.artist,
            (packet.duration_ms > 0.0).then_some(packet.duration_ms),
            path,
            now_ms,
        )
    }

    pub fn remove_custom_lyric(&mut self, id: &str) -> bool {
        self.custom_lyrics.remove(id)
    }

    pub fn request_artwork(&mut self) {
        let generation = self.model.generation();
        let source = self.model.packet.as_ref().and_then(|p| p.cover.as_ref());
        if self
            .cover_requested
            .as_ref()
            .is_some_and(|(g, value)| *g == generation && value.as_ref() == source)
        {
            return;
        }
        let source = source.cloned();
        let track = self.model.packet.as_ref().map(|p| oly_engine::hd_cover::CoverTrackInput {
            title: p.title.clone(),
            artist: p.artist.clone(),
            album: p.album.clone(),
        });
        if self.artwork.submit_with_track(generation, source.clone(), track) {
            self.cover_requested = Some((generation, source));
        }
    }
    pub fn start() -> anyhow::Result<Self> {
        let base = std::path::PathBuf::from(
            std::env::var_os("APPDATA").ok_or_else(|| anyhow::anyhow!("APPDATA unavailable"))?,
        );
        anyhow::ensure!(base.is_absolute(), "APPDATA must be absolute");
        let root = oly_core::config::DataRoot::new(base.join("OverLyrics"));
        let config = Arc::new(oly_core::config::ConfigStore::load(&root)?);
        let service_config = config.clone();
        let service = Arc::new(LyricsService::new(
            root.as_path(),
            Arc::new(move || service_config.get()),
            Arc::new(|message| log::info!("{message}")),
        ));
        service.init();
        let search = SearchWorker::start_with_config(
            service,
            Arc::new(oly_platform_win::http::NativeHttp),
            config.clone(),
        );
        oly_platform_win::smtc::volume::player::init(resolve_player_volume_script(root.as_path()));
        crate::updater::cleanup_old_binary();
        crate::updater::global().start_silent_check_once();
        let inbox = Arc::new(Mutex::new(crate::commands::Inbox::default()));
        let host = crate::commands::ShellHost::start(Arc::new(crate::commands::ShellSink::new(
            inbox.clone(),
        )))
        .ok_or_else(|| anyhow::anyhow!("SMTC worker already claimed"))?;
        Ok(Self {
            config,
            model: LiveModel::default(),
            host,
            inbox,
            search,
            track: None,
            candidates: Vec::new(),
            status: "等待播放器".into(),
            artwork: ArtworkWorker::start_with_hd(root.as_path().join("cover-cache")),
            motion: MotionWorker::start(root.as_path().join("motion-cache")),
            custom_lyrics: oly_engine::custom_lyrics::CustomLyricsStore::load(root.as_path()),
            media_sessions: Vec::new(),
            pinned_media_app: None,
            cover_requested: None,
        })
    }

    pub fn clear_lyrics_cache(&self) -> usize {
        self.search.clear_cache();
        remove_dir_contents(&self.config.data_dir().join("lyrics-cache"))
    }

    pub fn clear_cover_cache(&self) -> usize {
        let cover = remove_dir_contents(&self.config.data_dir().join("cover-cache"));
        let motion = remove_dir_contents(&self.config.data_dir().join("motion-cache"));
        cover + motion
    }

    pub fn reset_track_offsets(&self) -> anyhow::Result<()> {
        let store = oly_core::offsets::LyricOffsets::load(self.config.data_dir())?;
        store.clear()?;
        Ok(())
    }

    pub fn reset_all_settings(&self) -> anyhow::Result<serde_json::Value> {
        let fresh = self.config.reset()?;
        Ok(fresh)
    }
}

fn remove_dir_contents(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        } else if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

type ArtworkJob = (
    u64,
    Option<String>,
    Option<oly_engine::hd_cover::CoverTrackInput>,
);

/// Decode off the render thread. At most one queued job and one completed
/// image survive, even when covers change faster than they can be decoded.
pub struct ArtworkWorker {
    input: std::sync::mpsc::SyncSender<ArtworkJob>,
    output: Arc<Mutex<Option<(u64, Option<crate::player::scene::Cover>)>>>,
}
impl ArtworkWorker {
    pub fn start() -> Self {
        Self::start_inner(None)
    }

    pub fn start_with_hd(cache_dir: std::path::PathBuf) -> Self {
        Self::start_inner(Some(cache_dir))
    }

    fn start_inner(hd_cache_dir: Option<std::path::PathBuf>) -> Self {
        let (input, receiver) = std::sync::mpsc::sync_channel::<ArtworkJob>(1);
        let output = Arc::new(Mutex::new(None));
        let hd_sender = hd_cache_dir.map(|cache_dir| {
            let (hd_tx, hd_rx) =
                std::sync::mpsc::sync_channel::<(u64, oly_engine::hd_cover::CoverTrackInput, u32)>(1);
            let hd_out = output.clone();
            std::thread::Builder::new()
                .name("hd-cover-live".into())
                .spawn(move || {
                    let mut resolver = oly_engine::hd_cover::HdCoverResolver::new(cache_dir);
                    #[cfg(windows)]
                    let transport = oly_platform_win::http::NativeHttp;
                    while let Ok((generation, track, smtc_min_dim)) = hd_rx.recv() {
                        #[cfg(windows)]
                        {
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_millis() as u64);
                            let res = resolver.resolve(&track, now_ms, &transport);
                            if let Some(bytes) = res.bytes {
                                if let Ok(hd_cover) = crate::player::scene::decode_cover(&bytes) {
                                    if hd_cover.width.min(hd_cover.height) > smtc_min_dim {
                                        let mut guard =
                                            hd_out.lock().unwrap_or_else(|e| e.into_inner());
                                        if guard
                                            .as_ref()
                                            .is_none_or(|(g, _)| *g <= generation)
                                        {
                                            *guard = Some((generation, Some(hd_cover)));
                                        }
                                    }
                                }
                            }
                        }
                        #[cfg(not(windows))]
                        let _ = (&mut resolver, generation, track, smtc_min_dim);
                    }
                })
                .expect("start hd cover worker");
            hd_tx
        });
        let out = output.clone();
        std::thread::Builder::new()
            .name("artwork-live".into())
            .spawn(move || {
                while let Ok((generation, source, track)) = receiver.recv() {
                    let cover = source.and_then(|source| {
                        use base64::Engine;
                        let result = if source.starts_with("data:") {
                            source
                                .split_once(',')
                                .ok_or_else(|| anyhow::anyhow!("invalid cover URI"))
                                .and_then(|(_, data)| {
                                    Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
                                })
                                .and_then(|bytes| crate::player::scene::decode_cover(&bytes))
                        } else {
                            crate::player::scene::decode_cover_file(std::path::Path::new(&source))
                        };
                        match result {
                            Ok(cover) => Some(cover),
                            Err(e) => {
                                log::warn!("live artwork: {e:#}");
                                None
                            }
                        }
                    });
                    let smtc_min_dim = cover.as_ref().map_or(0, |c| c.width.min(c.height));
                    *out.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, cover));
                    if smtc_min_dim < 400 {
                        if let (Some(tx), Some(track)) = (&hd_sender, track) {
                            let _ = tx.try_send((generation, track, smtc_min_dim));
                        }
                    }
                }
            })
            .expect("start artwork worker");
        Self { input, output }
    }
    pub fn submit(&self, generation: u64, source: Option<String>) -> bool {
        self.submit_with_track(generation, source, None)
    }
    pub fn submit_with_track(
        &self,
        generation: u64,
        source: Option<String>,
        track: Option<oly_engine::hd_cover::CoverTrackInput>,
    ) -> bool {
        self.input.try_send((generation, source, track)).is_ok()
    }
    pub fn take(&self) -> Option<(u64, Option<crate::player::scene::Cover>)> {
        self.output.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

#[cfg(test)]
mod control_preview_tests {
    use super::*;
    use oly_contracts::{PlaybackStatus, TransportControls};
    use oly_platform_win::smtc::control::Action;
    fn packet(playing: bool, position_ms: f64) -> PlayerPacket {
        PlayerPacket { app: "AppleMusic".into(), title: "Song".into(),
            position_ms, duration_ms: 180_000.0, rate: 1.0,
            status: Some(PlaybackStatus::Code(if playing {4} else {5})),
            ctrl: TransportControls {playpause: true, ..Default::default()}, ..Default::default() }
    }
    fn click(m: &mut LiveModel, action: Action, now: f64) -> i64 {
        let request = m.prepare_control(action, 1000).unwrap();
        m.preview_control(action, now);
        request.request_id
    }
    #[test]
    fn pause_freezes_both_clocks_through_ten_seconds_of_stale_packets() {
        let mut m = LiveModel::default();
        m.feed(packet(true, 1000.0), 1.0);
        click(&mut m, Action::Pause, 501.0);
        let media = m.media_position(501.0);
        let lyric = m.position(501.0);
        for now in [1001.0, 5001.0, 10501.0] {
            m.feed(packet(true, now + 999.0), now); m.tick(now);
            assert!(!m.effective_playing());
            assert_eq!(m.media_position(now), media);
            assert_eq!(m.position(now), lyric);
            assert!(m.packet.as_ref().unwrap().is_playing());
        }
    }
    #[test]
    fn resume_advances_without_replaying_pause_or_jumping_on_confirmation() {
        let mut m = LiveModel::default();
        m.feed(packet(true, 1000.0), 1.0);
        let pause = click(&mut m, Action::Pause, 501.0);
        m.control_result(pause, true);
        m.feed(packet(false, 1500.0), 10501.0);
        let frozen = m.media_position(10501.0);
        let play = click(&mut m, Action::Play, 11001.0);
        m.control_result(play, true);
        m.feed(packet(false, 1500.0), 11501.0);
        assert_eq!(m.media_position(11501.0), frozen + 500.0);
        m.feed(packet(true, 1500.0), 12001.0);
        assert_eq!(m.media_position(12001.0), frozen + 1000.0);
        assert_eq!(m.media_position(12101.0), frozen + 1100.0);
        m.feed(packet(true, 1500.0), 12201.0);
        assert_eq!(m.media_position(12201.0), frozen + 1200.0);
        let mut seek = packet(true, 1500.0);
        seek.position_discontinuity = true;
        m.feed(seek, 12301.0);
        assert_eq!(m.media_position(12301.0), 1500.0);
    }
    #[test]
    fn refusal_and_timeout_restore_source() {
        let mut m = LiveModel::default(); m.feed(packet(true, 1000.0), 1.0);
        let id = click(&mut m, Action::Pause, 501.0);
        assert_eq!(m.control_result(id, false), Some(false));
        assert!(m.effective_playing()); assert_eq!(m.media_position(601.0), 1600.0);
        click(&mut m, Action::Pause, 601.0);
        m.feed(packet(true, 16000.0), 15001.0); m.tick(15601.0);
        assert!(m.effective_playing()); assert!(!m.control_pending());
    }
    #[test]
    fn track_change_and_idle_invalidate_preview_and_receipt() {
        let mut m = LiveModel::default(); m.feed(packet(true, 1000.0), 1.0);
        let id = click(&mut m, Action::Pause, 501.0);
        let mut next = packet(true, 0.0); next.title = "Next".into();
        m.feed(next, 1001.0);
        assert!(m.effective_playing()); assert_eq!(m.control_result(id, false), None);
        click(&mut m, Action::Pause, 1101.0); m.idle();
        assert!(!m.effective_playing()); assert!(!m.control_pending());
    }
    #[test]
    fn rapid_toggle_ignores_old_receipt_and_old_matching_status() {
        let mut m = LiveModel::default(); m.feed(packet(true, 1000.0), 1.0);
        let pause = click(&mut m, Action::Pause, 501.0);
        let play = click(&mut m, Action::Play, 1001.0);
        assert_eq!(m.control_result(pause, false), None); m.control_result(play, true);
        m.feed(packet(true, 3000.0), 1501.0);
        assert_eq!(m.media_position(1501.0), 2000.0);
        m.feed(packet(false, 1500.0), 2001.0);
        assert!(m.effective_playing()); assert_eq!(m.media_position(2001.0), 2500.0);
        m.feed(packet(true, 1500.0), 2501.0);
        assert_eq!(m.media_position(2501.0), 3000.0);
    }
    #[test]
    fn only_matching_source_and_accepted_receipt_confirm_preview() {
        let mut m = LiveModel::default(); m.feed(packet(true, 1000.0), 1.0);
        let id = click(&mut m, Action::Pause, 501.0);
        m.feed(packet(false, 1500.0), 1001.0);
        assert!(m.preview.is_some());
        let mut other = packet(false, 6000.0); other.timeline_source = "another timeline".into();
        m.feed(other, 1501.0); m.control_result(id, true);
        assert!(m.preview.is_some()); assert_eq!(m.media_position(1501.0), 1500.0);
        m.feed(packet(false, 1500.0), 2001.0);
        assert!(m.preview.is_none()); assert_eq!(m.media_position(2001.0), 1500.0);
    }
    #[test]
    fn new_click_after_expired_preview_keeps_its_own_receipt() {
        let mut m = LiveModel::default(); m.feed(packet(true, 1000.0), 1.0);
        click(&mut m, Action::Pause, 501.0);
        let id = click(&mut m, Action::Pause, 16001.0);
        assert!(!m.effective_playing());
        assert_eq!(m.control_result(id, false), Some(false));
        assert!(m.effective_playing());
    }
    #[test]
    fn track_change_clamps_stale_timestamp_and_reanchors_buffering_zero() {
        let mut m = LiveModel::default();
        m.feed(packet(true, 5_000.0), 10_000.0);

        let mut next = packet(true, 115_000.0);
        next.title = "Song B".into();
        next.last_updated_unix_ms = 119_400.0;
        m.feed(next, 120_000.0);
        assert_eq!(m.media_position(120_000.0), 0.0);

        // Even with an intermediate cover refresh packet during buffering,
        // the real post-buffer start packet re-anchors back to 0.
        let mut cover_tick = packet(true, 0.0);
        cover_tick.title = "Song B".into();
        cover_tick.last_updated_unix_ms = 120_400.0;
        m.feed(cover_tick, 120_400.0);

        let mut buffered = packet(true, 0.0);
        buffered.title = "Song B".into();
        buffered.last_updated_unix_ms = 120_650.0;
        m.feed(buffered, 120_650.0);
        assert_eq!(m.media_position(120_650.0), 0.0);
        assert_eq!(m.media_position(121_650.0), 1_000.0);
    }
    #[test]
    fn idle_placeholder_titles_and_closed_status_never_request_lyrics() {
        let mut m = LiveModel::default();
        let mut placeholder = packet(false, 0.0);
        placeholder.title = "未在播放".into();
        placeholder.artist = String::new();
        assert!(m.feed(placeholder, 100.0).is_none());
        assert!(m.packet.is_none());

        let mut en = packet(false, 0.0);
        en.title = "Not Playing".into();
        assert!(m.feed(en, 200.0).is_none());

        let mut closed = packet(false, 0.0);
        closed.status = Some(oly_contracts::PlaybackStatus::Code(0));
        assert!(m.feed(closed, 300.0).is_none());
    }

}

#[cfg(test)] mod progressive_selection_tests {
 use super::*;use oly_engine::types::Verified;
 fn candidate(source:&str,id:&str,word:bool,trans:bool,bg:bool)->Candidate{Candidate{source:source.into(),id:id.into(),verified:Some(Verified{word:Some(word),trans:Some(trans),bg:Some(bg),..Default::default()}),..Default::default()}}
 #[test] fn final_collection_only_upgrades_across_tiers_and_holds_same_tier(){
 let track = Track::default();
 assert_eq!(prefer_progressive_pick(Some(candidate("amlldb","word",true,false,false)),Some(candidate("lrclib","trans",false,true,false)),&track,0.72).unwrap().id,"trans");
 assert_eq!(prefer_progressive_pick(Some(candidate("amlldb","trans",false,true,false)),Some(candidate("lrclib","both",true,true,false)),&track,0.72).unwrap().id,"both");
 assert_eq!(prefer_progressive_pick(Some(candidate("qq","both",true,true,false)),Some(candidate("amlldb","both_bg",true,true,true)),&track,0.72).unwrap().id,"both_bg");
 assert_eq!(prefer_progressive_pick(Some(candidate("netease","n",true,true,false)),Some(candidate("qq","q",true,true,false)),&track,0.72).unwrap().id,"n");
 assert_eq!(prefer_progressive_pick(Some(candidate("qq","first",true,true,false)),Some(candidate("qq","second",true,true,false)),&track,0.72).unwrap().id,"first");
 }
}
