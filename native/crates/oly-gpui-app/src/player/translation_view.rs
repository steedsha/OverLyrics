//! Translation updates patch row text without replacing the playback scene.
use super::*;
use crate::player::translation::{self, TranslationPatch, TranslationWorker};
use serde_json::{Value, json};

fn remove_generated(doc: &mut oly_contracts::lyrics::LyricDoc, patches: &[TranslationPatch]) -> bool {
    let mut changed = false;
    for patch in patches {
        if let Some(line) = doc.lines.get_mut(patch.line_index) {
            if line.text == patch.original_text && line.trans == patch.translation {
                line.trans.clear();
                changed = true;
            }
        }
    }
    changed
}

impl PlayerView {
    pub(super) fn translation_preference_key(&self) -> String {
        let track_key = self.live.as_ref().and_then(|live| live.model.packet.as_ref())
            .map(|packet| packet.track_key()).unwrap_or_default();
        let (source, id) = self
            .doc_identity
            .as_ref()
            .map(|(s, id)| (s.as_str(), id.as_str()))
            .unwrap_or((self.doc_provider.as_str(), ""));
        oly_contracts::config::candidate_translation_key(&track_key, source, id)
    }

    pub(super) fn refresh_translated_rows(&mut self) {
        let Some(doc) = self.doc.as_ref() else { return; };
        self.lines = crate::player::playback::prepare_lines(doc);
        let updated = model::rows_from(&self.lines);
        for (row, replacement) in self.rows.iter_mut().zip(updated) {
            // Original row identity, timing, geometry and animation stay installed.
            if row.group == replacement.group && row.is_bg == replacement.is_bg && row.text == replacement.text {
                row.translation = replacement.translation;
            }
        }
        self.shaper.clear();
    }

    pub(super) fn sync_smart_translation(&mut self) {
        // Search workers read this same persisted snapshot. A settled change
        // invalidates old results without discarding the currently installed doc.
        let search_config = if let Some(live) = self.live.as_ref() {
            let cfg = live.config.get();
            json!({
                "enabledSources": cfg.pointer("/lyrics/enabledSources"),
                "matchThreshold": cfg.pointer("/lyrics/matchThreshold"),
            })
        } else {
            json!({
                "enabledSources": self.settings.config.pointer("/lyrics/enabledSources"),
                "matchThreshold": self.settings.config.pointer("/lyrics/matchThreshold"),
            })
        };
        let search_changed = self.search_config.as_ref().is_some_and(|old| old != &search_config);
        self.search_config = Some(search_config);
        if search_changed {
            if let Some(live) = self.live.as_mut() {
                live.search.clear_cache();
                live.candidates.clear();
                self.picked = usize::MAX;
                self.picker_scroll_spring.set_position(0.0);
                self.picker_scroll_spring.set_target_position(0.0);
                if let Some(track) = live.track.clone() {
                    live.search.request(live.model.generation(), track, None);
                    live.status = "搜索歌词中".into();
                } else {
                    live.search.cancel();
                }
            }
        }
        let Some(_) = self.doc.as_ref() else {
            self.translation_signature = None;
            if let Some(worker) = self.smart_translation_worker.as_ref() { worker.cancel(); }
            self.translated_rows.clear();
            return;
        };
        let generation = self.live.as_ref().map(|live| live.model.generation()).unwrap_or(0);
        let identity = serde_json::to_string(&(&self.doc_identity, &self.doc_provider)).unwrap_or_default();
        let lyrics = self.settings.config.get("lyrics").cloned().unwrap_or(Value::Null);
        let signature = (generation, identity.clone(), lyrics);
        let preference_key = self.translation_preference_key();
        let enabled = translation::translation_enabled(&self.settings.config, &preference_key);
        if self.translation_signature.as_ref() != Some(&signature) {
            // Only text that this view actually generated may be removed. A new
            // target/engine must see the original missing rows when requesting.
            let patches = std::mem::take(&mut self.translated_rows);
            if remove_generated(self.doc.as_mut().unwrap(), &patches) {
                self.refresh_translated_rows();
            }
            if enabled || self.smart_translation_worker.is_some() {
                let config = self.settings.config.clone();
                let worker = self.smart_translation_worker.get_or_insert_with(TranslationWorker::new);
                worker.request(generation, identity.clone(), preference_key, self.doc.as_ref().unwrap().clone(), config);
            }
            self.translation_signature = Some(signature);
        }
        // Request first: an edited setting fences an old completed result before
        // take can return it, including disable/re-enable and version changes.
        let result = self.smart_translation_worker.as_ref().and_then(TranslationWorker::take);
        let Some(result) = result.filter(|result| enabled && result.generation == generation && result.identity == identity) else { return; };
        let doc = self.doc.as_mut().unwrap();
        let applied: Vec<_> = result.patches.into_iter().filter(|patch| {
            doc.lines.get(patch.line_index).is_some_and(|line| line.text == patch.original_text
                && line.trans.trim().is_empty() && !patch.translation.trim().is_empty())
        }).collect();
        if translation::apply_patches(doc, &applied) > 0 {
            self.translated_rows.extend(applied);
            self.refresh_translated_rows();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oly_contracts::lyrics::{LyricDoc, LyricLine};

    #[::std::prelude::v1::test]
    fn disabling_only_removes_unchanged_generated_translations() {
        let patch = |line_index, original_text: &str| TranslationPatch {
            line_index, original_text: original_text.into(), translation: "generated".into(),
        };
        let mut doc = LyricDoc { lines: vec![
            LyricLine { text: "one".into(), trans: "generated".into(), ..Default::default() },
            LyricLine { text: "two".into(), trans: "provider".into(), ..Default::default() },
            LyricLine { text: "changed version".into(), trans: "generated".into(), ..Default::default() },
        ], ..Default::default() };
        assert!(remove_generated(&mut doc, &[patch(0, "one"), patch(1, "two"), patch(2, "old version")]));
        assert_eq!(doc.lines[0].trans, "");
        assert_eq!(doc.lines[1].trans, "provider");
        assert_eq!(doc.lines[2].trans, "generated");
    }
}
