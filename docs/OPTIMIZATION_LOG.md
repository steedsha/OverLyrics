# OverLyrics Architecture & Performance Optimization Audit Log

- **Base anchor**: `main` / `v1.1.0` (`6b20a61`) — untouched for instant rollback.
- **Working branch**: `refactor/v1.2-arch-perf`
- **Verification gate**: Every milestone passed `cargo test --workspace --lib`, and final build passed `cargo build --release -p oly-gpui-app --bin overlyrics` with zero warnings.

## Commit-by-Commit Trace

| Phase | Commit | Scope | Summary & Risk Profile |
| :--- | :--- | :--- | :--- |
| **P0** | `3498552` | `player/desktop_lyrics.rs` | `SharedDesktopBridge::snapshot` switched to `Arc<DesktopSnapshot>` (zero-copy per-frame reads) + on-demand frame scheduling (idle/paused drops from 60–240Hz to 120ms background poll; pointer events call `cx.notify()` for 0ms wakeup). |
| **P1-A** | `cf006e5` | `crates/oly-ui`, `player/{elastic,panel_motion,scrollbar,win32_window,glass,interaction,model}.rs` | Created independent `oly-ui` workspace crate (zero music/lyrics deps) with base motion, geometry, color, D3D11 glass, and Win32 overlay modules; kept thin `pub use` re-exports in `oly-gpui-app`. |
| **P1-A2** | `15c6593` | `crates/oly-ui/{chrome,details,theme}.rs`, `oly-core/src/config.rs`, `player/{chrome,details,model,settings}.rs` | Moved generic Liquid Glass controls & geometry to `oly-ui`, added explicit `oly_ui::theme::Theme`, kept player-specific `cover_blank` in `player/chrome.rs`, and unified `config_defaults.json` parsing via cached `oly_core::config::default_config()`. |
| **P1-B1** | `62ed329` | `player/desktop_lyrics/` | Split 4,858-line monolith into `mod.rs`, `config.rs`, `bridge.rs`, `vertical.rs`, `view.rs`, and `popovers/{mod,volume,picker,more,color}.rs`. Pure structural split. |
| **P1-B2** | `2c22f07` | `player/settings/` | Split 3,349-line monolith into `mod.rs`, `schema.rs`, `paint.rs`, and `tests.rs`. Pure structural split. |
| **P1-B3** | `8893030` | `player/view/` | Split 8,691-line monolith into `mod.rs`, `types.rs`, `render.rs`, `handlers.rs`, `frame_paint.rs`, and `tests.rs`. Pure structural split. |
| **P2** | `57fe647` | `player/view/{mod,types,render,frame_paint}.rs` | Replaced CPU per-pixel `cover_blend_buf` loop & UI-thread `imageops::resize` with GPU dual-surface opacity crossfade (`prev_cover_surface` + `paint_custom_surface_rounded_opacity`), reducing track-switch uploads from ~33 frames to 1. (Audited `GlassPanel`: already destroyed on close in `render.rs`.) |

## Troubleshooting & Rollback Guide

- **Full rollback to v1.1.0**: `git checkout main`
- **Revert a single optimization** (e.g. if desktop lyrics idle polling or GPU cover crossfade shows any visual quirk in `--live`):
  - Revert P0 only: `git revert 3498552`
  - Revert P2 only: `git revert 57fe647`
- **Pinpoint any regression**: `git bisect start HEAD 6b20a61` (every commit builds and passes all unit tests).

## Deferred Optional Items (Not Touched to Preserve Zero Risk)

- **`Pretendard.ttf` (6.74 MB) font subsetting**: requires external `fonttools`/`pyftsubset` verification against CJK/Korean/Latin lyrics glyphs; deferred to a dedicated asset PR.
- **`oly-engine/src/data/{pinyin,t2s}.rs` binary table compilation**: low runtime urgency; can be done later if `oly-engine` compile times become a bottleneck.
- **Legacy `oly-gpui-app/src/ui/{picker,popover,settings}.rs`**: preserved untouched so existing integration tests (`tests/ui_flows.rs`, `tests/popover_behavior.rs`, `tests/glass_entrance.rs`) remain 100% green.
