# GPUI 迁移试验 · 配置兼容（CONFIG_COMPAT）

本文件回答一个问题：**现有用户的 `%APPDATA%\OverLyricsLite` 目录交给试验版后，每一个键会怎样？**

结论先行：

- `oly-core::config::ConfigStore` 是旧版 Tauri `src-tauri/src/store/mod.rs`（1321 行）的**逐行移植**，不是重写；
  `config_defaults.json` 与旧版 `store/defaults.json` **逐字节相同**（6128 B，SHA-256
  `afc862b11d1afee990ab783d82d3b053e6f6a473d9d9de30d0d992f1c1dac010`）。
- 三份用户文件（`config.json` / `lyric-offsets.json` / `source-memory.json`）全部有对应实现。
- 旧版配置里出现的**每一个键**都有明确处置：原样保留、迁移改写、或按旧版同款死键规则删除（见下表）。
- 未知键（含将来版本新增的键）**原样往返**，不被本版丢弃。
- 数据根是显式参数（`DataRoot`），默认指向 `%APPDATA%\OverLyrics`。

## 1. 数据根与文件

| 文件 | 归属 | 实现 | 说明 |
| --- | --- | --- | --- |
| `config.json` | `oly_core::config::ConfigStore` | 24 项 `config_compat` 测试 | 原子写：先写 `config.json.tmp` 再 rename；rename 失败删除临时文件并**返回错误** |
| `lyric-offsets.json` | `oly_core::offsets::LyricOffsets` | 11 项 `offset_scope` 测试 | 按专辑/来源共享；写失败回滚内存值 |
| `song-offsets.json` | 同上（旧版遗留 v1） | 同上 | 读取后并入内存，文件内容改写为 `{}`（**不删除文件**，与旧版一致） |
| `source-memory.json` | `oly_engine::service::LyricsService` | 服务层集成测试 | 只落盘**手选**来源；自动选择只活在本次会话 |

`ConfigStore::load(&DataRoot)` 的合成顺序与旧版一致：

```
defaults  ←  merge(sanitize(saved))  ←  adopt_type_profiles(saved)  ←  migrate(saved)  ←  scrub
```

- `sanitize`：section 不是对象时整段丢弃（半截写入的 `"display": 3` 不会污染合并）。
- `merge`：对象深合并，数组/标量整体替换；`__proto__` / `constructor` / `prototype` 每一层都丢弃。
- `adopt_type_profiles`：`typeProfiles` 三档（cover / nocover / mini）逐档采用保存值，
  不与默认值互相覆盖——这正是「切档往返不互相覆盖」的实现点。
- `migrate`：见 §3。
- `scrub`：再递归丢一次禁用键（防御合并后重新引入）。

损坏的 `config.json`（非法 JSON、读失败、根不是对象）→ 回退默认值并写回；
**不触碰同目录其他文件**（`lyric-offsets.json`、`source-memory.json`、封面缓存等全部原样）。

## 2. 键级处置表

「保留」= 值原样进入内存与下一次落盘；「迁移」= 满足条件时改写一次（marker 置位后不再触发）；
「删除」= 旧版同款死键清理，读完即丢。

### 2.1 `window`

| 键 | 处置 | 备注 |
| --- | --- | --- |
| `x` / `y` | 保留 | 可为 null（首次运行/显示器变化） |
| `width` / `height` | 保留 | 试验版 UI 读取 |
| `userSized` | 保留 | 用户是否手动调过尺寸 |
| `miniPos` | 保留 | mini 形态位置（对象） |
| `alwaysOnTop` | 保留 | `topDefaultMigrated` 一次性迁移见 §3 |
| `fullscreen` | 迁移 → 强制 `false` | 试验版不做 fullscreen 重启恢复 |
| `topDefaultMigrated` | 保留（marker） | `reset()` 不重置 |
| `scale` / `clickThrough` / `hideWhenPaused` | 删除 | 旧版死键 |

### 2.2 `display`

全部 **保留**，除下列三类：

| 键 | 处置 | 备注 |
| --- | --- | --- |
| 8 个迁移 marker（`anchor42Migrated` 等） | 保留 | `reset()` 不重置，保证迁移只发生一次 |
| `backdropStyle` | 迁移 | `ambient` → `neutral`；`acrylic`/`amll`/`fluid` → `none` |
| `glassRenderer` | 迁移 | 非 `css`/`svg` → `css` |
| `uiAccentColor` | 归一 | `#abc` → `#aabbcc`；缺 `#` 前缀补上；非法值 → `#7fd6c0` |
| 死键列表（`translationScale`、`romanizationScale`、`blurEffect`、`coverScale`、`coverSize`、`glowStrength`、`longSyllableScale`、`pushStrength`、`emp*`、`scroll*`（旧几何）、`distance*`、`layoutMode` 等） | 删除 | 完整列表见 `oly-core/src/config.rs` 的 `DEAD_DISPLAY_KEYS` |
| `typeProfiles.*.coverSize` | 删除 | 三档逐档清理 |

保留键全表（与 defaults 一致）：
`fontSize` `transFontSize` `fontFamily` `transFontFamily` `fontWeight` `transFontWeight` `letterSpacing`
`playingLineTopOffset` `unplayedOriginalOpacity` `idleLineOpacity` `translatedLyricsOpacity` `phoneticLyricsOpacity`
`blurMax` `edgeFeather` `align` `lineBreakMode` `lineGap` `innerGap` `longSyllableThreshold`
`scrollEasingMode` `scrollTopDuration` `scrollBottomDuration` `scrollTopDelay` `scrollBottomDelay`
`lineRadius` `lineWidthAspect` `linePaddingX` `romanFontScale` `wordFadeWidth` `brightAlpha` `maskFadeMs`
`enableScale` `bgLineScale` `bgLineOpacity` `transCurrentAlpha` `hidePassedLines` `enableSpring` `springFeel`
`hoverBgAlpha` `dotScale` `advTryAdvance` `advCleanOverlaps` `advSyncBg` `showCover` `showLyrics` `showMeta`
`showTranslation` `showRomanization` `showProgress` `showLyricFps` `accentColor` `uiAccentColor`
`backdropStyle` `glassRenderer` `windowBackground` `immersionCompact` `coverRadius` `coverShadow`
`cornerRadius` `motionCover` `motionCoverCacheMB` `typeProfiles`。

### 2.3 `lyrics`

| 键 | 处置 | 备注 |
| --- | --- | --- |
| `enabledSources` | 迁移 | 删 `apple`；`sourcesPruned < 3` 时一次性补 `qq: true` 并置 `sourcesPruned = 3` |
| `sourcesPruned` | 保留（marker） | `reset()` 不重置 |
| `matchThreshold` | 迁移 | `0 < v <= 1` 视为比例 → `70`；数字字符串 → 取整 |
| `autoTranslate` | 保留 | |
| `translationOverrides` | 保留 | 按 `[track, source]` 键的布尔覆盖（见 `oly-contracts::config::is_translation_enabled`） |
| `translateEngine` | 保留 | `free` / `ai` / `ai-first` |
| `translateTarget` | 保留 | 白名单外的值在 `translation_settings` 回落 `zh-CHS` |
| `libreUrl` | 保留 | 仅 http(s) 生效；为空走内置端点 |
| `aiPreset` `aiBaseUrl` `aiKey` `aiModel` | 保留 | 7 个内置服务商预设逐字段移植（见 §4）；`aiKey` 只在预设已知且 baseUrl 未改时转发 |
| `aiLadderOn` | 迁移 | 旧版 `0` → `1`（ladder 默认开启） |
| `enabledProviders` `minScore` `localLyricsDir` `amllEnabled` `tlsStrict` `autoSearch` `offsetMs` | 删除 | 旧版死键 |

### 2.4 顶层

| 键 | 处置 |
| --- | --- |
| `migratedFrom` | 保留（`import_legacy` 写入 `"overlyric@1"`） |
| `hotkeys` | 删除（旧版已移除热键 section） |
| 其他未知键 | 保留（递归原样往返） |

## 3. 一次性迁移（marker 语义）

每个 marker 只在**该键尚未置位**时触发一次，随后写回 `true`；`reset()` 恢复默认值时这些 marker
**不回退**，否则老用户每次重置都会被重新迁移一遍。

| marker | 触发条件 | 动作 |
| --- | --- | --- |
| `window.topDefaultMigrated` | 首次 | `alwaysOnTop` 为真时改为 `false` |
| `display.anchor42Migrated` | 首次 | `playingLineTopOffset` 50 → 37 |
| `display.fontPretendardMigrated` | 首次 | `fontFamily` `""` → `"Pretendard"` |
| `display.feather56Migrated` | 首次 | `edgeFeather` 16 → 56 |
| `display.tuning68Migrated` | 首次 | `fontSize` 90 → 68、`letterSpacing` 0.2 → 0.16、`wordFadeWidth` 0.2 → 0.08 |
| `display.lyricGapUnitMigrated` | 首次 | `lineGap` 0.16 → 0、`innerGap` 0.08 → 0.3 |
| `display.innerGapAnchorMigrated` | 首次且 gap-unit 已迁移 | `innerGap` 0.3 → 0 |
| `display.empMeasuredMigrated` | 首次 | `emp*` 旧值 50 → 65、40 → 25（随后死键清理） |
| `lyrics.sourcesPruned` | `< 3` | 补 `qq: true`，置 3 |

## 4. AI 服务商预设（`oly-contracts::config::AI_PRESETS`）

逐字段来自 `src/shared/translation-settings.ts`，7 个：

| preset | baseUrl | models | request 附加体 |
| --- | --- | --- | --- |
| `deepseek` | `https://api.deepseek.com/v1` | `deepseek-flash`, `deepseek-v4-pro` | `{"thinking":{"type":"disabled"}}` |
| `zhipu` | `https://open.bigmodel.cn/api/paas/v4` | `glm-4.7-flash` | `{"thinking":{"type":"disabled"}}` |
| `siliconflow` | `https://api.siliconflow.cn/v1` | `deepseek-ai/DeepSeek-V3.2`, `Qwen/Qwen3.6-27B` | `{"enable_thinking":false}` |
| `dashscope` | `https://dashscope.aliyuncs.com/compatible-mode/v1` | `qwen3.8-flash`, `qwen3.7-plus` | `{"enable_thinking":false}` |
| `moonshot` | `https://api.moonshot.cn/v1` | `kimi-k3.1-pro`, `kimi-k3` | `{"temperature":1}` |
| `openrouter` | `https://openrouter.ai/api/v1` | `deepseek/deepseek-v4.1-flash` | `{"reasoning":{"effort":"none"}}` |
| `openai` | `https://api.openai.com/v1` | `gpt-4o-mini` | `{}` |

`translation_settings` 的三条规则（7 项测试钉住）：

1. **密钥只在预设已知时转发**：`aiPreset` 不在表内 → 返回空 key（防止自建端点偷拿内置密钥）。
2. **改了 baseUrl 就不再送 key**：`aiBaseUrl` 规范化（去尾斜杠）后与预设不同 → key 置空。
3. **模型白名单**：`aiModel` 不在该预设 models 内 → 回落默认模型；`translateTarget` 不在
   `zh-CHS/zh-CHT/en/ja/ko` 内 → 回落 `zh-CHS`。

## 5. 原子写与失败语义

`ConfigStore::patch` 的顺序是**先落盘、后换内存**：

1. 取当前内存快照，深合并补丁，重跑归一化；
2. 写 `config.json.tmp`（同目录），`fs::rename` 覆盖 `config.json`；
3. 只有 rename 成功才 `*guard = candidate`。

因此磁盘写失败时（目录占位、权限、磁盘满）：`patch` 返回 `ConfigError::Write`，
**内存仍是旧值**——UI 上看到的设置不会领先于磁盘。测试
`a_failed_write_is_reported_and_memory_never_runs_ahead_of_disk` 用一个同名目录占住 `config.json.tmp`
制造失败，解除占位后同一进程再 patch 成功。

`LyricOffsets::nudge` 同一模式：先算新值、写盘失败则恢复原值并返回错误（±5000 ms 钳制在写之前）。

## 6. 旧数据导入

`ConfigStore::import_legacy(dir, legacy_dir)`：

- 只在目标 `config.json` 不存在时工作；读取旧目录 `config.json` → 合成 → 落盘，写 `migratedFrom: "overlyric@1"`。
- **全程只读旧目录**：测试用 13 份原始 fixture 的 SHA-256 对照证明旧目录字节未变。
- 第二次调用返回 `false`（幂等）。
- 旧目录缺失/损坏 → `false`，不写半成品。

`import_legacy_offsets(dir, legacy_dir)`：`lyric-offsets.json` 存在才拷；做 JSON 合法性校验；
目标已存在则不覆盖。

已完成的兼容验证（`cargo test -p oly-core --test config_compat`）覆盖：
新装默认值、显式数据根隔离、深合并、半截 section、损坏恢复不伤邻文件、未知键往返、
三类迁移、marker 只触发一次、`typeProfiles` 三档独立、禁用键各层清理、写失败不领先磁盘、
旧数据导入只读且幂等。

## 变更记录

| 日期 | 变更 |
| --- | --- |
| 2026-11-15 | 初版：ConfigStore 移植完成、offsets、translation settings 与键级处置表 |
| 2026-11-24 | 移除已过时的迁移期“未实现”清单，同步当前 `%APPDATA%\OverLyrics` 数据根说明 |
