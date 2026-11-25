# Project Status & Architecture Notes (v1.0.0)

## Current State

- **Active workspace**: `native/` (Rust edition 2024, 7 crates + vendored `gpui-pre` / `gpui-pre-windows`).
- **Release Binary**: Standalone zero-dependency `overlyrics.exe` (statically linked CRT, embedded Win32 icon & Pretendard font fallback, no external runtime assets required).
- **Build & Verification**:
  - `cargo check --workspace --all-targets`: 0 errors, 0 warnings.
  - `cargo test --workspace`: all unit & integration tests pass.
  - `cargo build --release -p oly-gpui-app --bin overlyrics`: 0 warnings.

## Highlights in v1.0.0

- **Immersion Background & WASAPI Beat Response**: Linear-light D3D11 6-pass shader pipeline with OKLCH/Oklab palette flow, drifting light clouds, adaptive scrim, dual-frequency IGN dithering, and optional WASAPI loopback 140Hz beat flash & elastic blob wobble.
- **Liquid Glass UI & Four Window Modes**: `Mini`, `Album`, `Lyrics`, and `Window` modes with DWM VSync pacing (`DwmFlush`), proportional fullscreen layout scaling, idle window-mode auto-collapse with monochrome music-note cover, and unified Settings alignment.
- **Lyrics Engine**: Kugou (native KRC word durations), NetEase Cloud Music, QQ Music, LRCLIB, and AMLL TTML DB (with per-source probe budget & cross-language rescue), plus local `.lrc`/`.ttml`/`.qrc`/`.yrc`/`.txt` import and bilingual translation.

## Reference Docs

- [docs/gpui/CONFIG_COMPAT.md](gpui/CONFIG_COMPAT.md) — `ConfigStore`, profile keys, AI presets, and atomic write semantics
- [docs/gpui/UPSTREAM.md](gpui/UPSTREAM.md) — `gpui-pre` / `gpui-pre-windows` fork inventory
- [docs/gpui/design-scene-snapshot.md](gpui/design-scene-snapshot.md) — D3D11 `scene_snapshot` + `PaintHook` glass pipeline
