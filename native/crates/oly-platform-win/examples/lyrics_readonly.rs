//! Opt-in online read-only smoke check. Never controls a media session.
use oly_engine::{
    service::{LyricsService, SearchFlags},
    types::Track,
};
use std::sync::Arc;
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let track = Track {
        title: args.next().unwrap_or_else(|| "Hard Times".into()),
        artist: args.next().or(Some("Paramore".into())),
        duration_ms: Some(182000.0),
        ..Default::default()
    };
    let root = std::path::PathBuf::from(
        std::env::var_os("APPDATA").ok_or_else(|| anyhow::anyhow!("APPDATA missing"))?,
    )
    .join("OverLyrics");
    let service = LyricsService::new(
        root,
        Arc::new(
            || serde_json::json!({"lyrics":{"enabledSources":{"netease":false,"kugou":false,"qq":false,"amlldb":false}}}),
        ),
        Arc::new(|message| eprintln!("{message}")),
    );
    let http = oly_platform_win::http::NativeHttp;
    let epoch = service.epoch();
    let found = service.search_all_at(&http, &track, SearchFlags::default(), epoch);
    let ranked = oly_engine::arbiter::judge(&found.list, &track);
    println!(
        "candidates={} verified={} cancelled={}",
        found.list.len(),
        ranked.ranked.len(),
        found.cancelled
    );
    let picked = ranked
        .pick
        .ok_or_else(|| anyhow::anyhow!("no verified matching candidate"))?;
    let doc = service.use_candidate(&http, &track, &picked.source, &picked, epoch, true, false);
    anyhow::ensure!(doc.ok && !doc.stale, "{}", doc.error);
    let parsed: oly_contracts::LyricDoc = serde_json::from_value(doc.doc.unwrap())?;
    println!(
        "source={} rows={} durationSec={}",
        picked.source,
        parsed.lines.len(),
        parsed.duration_sec
    );
    anyhow::ensure!(!parsed.lines.is_empty(), "empty lyric document");
    Ok(())
}
