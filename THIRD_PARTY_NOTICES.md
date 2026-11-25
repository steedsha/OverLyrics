# Third-Party Notices

OverLyrics is licensed under [AGPL-3.0](LICENSE). The third-party libraries, algorithms, and assets below retain their respective licenses.

## Ports & References

| Project | Usage | License |
| --- | --- | --- |
| [Apple Music-like Lyrics](https://github.com/amll-dev/applemusic-like-lyrics) | Word-by-word emphasis curves, timing, and visual reference | [AGPL-3.0](docs/licenses/amll-AGPL-3.0.txt) |
| [AMLL TTML Database](https://github.com/amll-dev/amll-ttml-db) | Community-contributed TTML word-by-word lyrics & metadata | — |
| [Lyricify.Lyrics.Helper](https://github.com/jayfunc/Lyricify-Lyrics-Helper) | Match scoring and QRC/KRC decryption ports | [Apache-2.0](docs/licenses/Lyricify-Apache-2.0.txt) |
| [opencc-js](https://github.com/nk2028/opencc-js) | Traditional-to-Simplified character normalization table | [MIT](docs/licenses/opencc-js-MIT.txt) |
| [OpenCC](https://github.com/BYVoid/OpenCC) | Upstream conversion dictionary source | [Apache-2.0](docs/licenses/OpenCC-Apache-2.0.txt) |

## Fonts

- **Pretendard** ([orioncactus/pretendard](https://github.com/orioncactus/pretendard)) — [SIL Open Font License 1.1](docs/licenses/Pretendard-OFL.txt)

## Rust & UI Dependencies

- **GPUI** (`gpui-pre`, `gpui-pre-windows` under `native/vendor/`) — **Apache-2.0** (upstream Zed/GPUI snapshot with local modifications for custom D3D11 surface composition and lyric rasterization).
- Standard Rust ecosystem crates (`windows`, `serde`, `serde_json`, `image`, `regex`, `flate2`, `sha2`, `log`, `env_logger`, `anyhow`) — MIT / Apache-2.0.

## Online Content

Lyrics, translations, and cover artwork are fetched on demand from third-party providers and are not owned by this project. This repository does not bundle user lyrics or media caches.
