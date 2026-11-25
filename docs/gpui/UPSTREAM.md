# GPUI 上游版本与最小 fork 记录

2026-11-16 补充：下文 683/−4 是 Task13 历史统计，当前又增加 Windows 歌词字形绘制扩展，见文末。旧结论“只补 surface”不再覆盖当前 fork。

更动时间：2026-11-14（Task 2）；2026-11-15 更新（Task 10 GPU 直通扩展、Task 13 Present 统计桥、
toolchain 实测版本）

## 1. 版本钉死

| 组件 | 版本 | 来源 |
| --- | --- | --- |
| `gpui-kit` | `0.9.1` | crates.io，HEAD `12945cca14f1172164f75f6b282097b94898713d` |
| `gpui-pre` | `=0.3.12`（精确钉死） | crates.io，由 gpui-kit 传递依赖 |
| `gpui-pre-platform` | `=0.3.12` | crates.io |
| `gpui-pre-windows` | `=0.3.12` | crates.io，`gpui-pre-platform` 的 Windows 目标依赖 |
| `gpui-fast` / `gpui-fast-platform` | `0.1.14`（未启用） | 仅作受控 A/B，不进入本 workspace |

`gpui-kit` 自己的 workspace 就把 GPUI 快照钉成精确版本（`version = "=0.3.12"`）而不是 caret，理由是
caret 需求曾把应用推到 `gpui-component` 编不过的新快照。本工程沿用同一策略，不做浮动升级。

`Cargo.lock` 中 `gpui-pre` / `gpui-pre-windows` 仍是 `0.3.12`，只是 source 被下列 patch 重定向到本地路径。

## 2. 重定向方式

`native/Cargo.toml`：

```toml
[patch.crates-io]
gpui-pre = { path = "vendor/gpui-pre" }
gpui-pre-windows = { path = "vendor/gpui-pre-windows" }
```

`[patch.crates-io]` 作用于整个依赖图，因此 `gpui-kit`、`gpui-pre-platform`、以及
`gpui-pre-windows` 自己对 `gpui-pre` 的依赖全部解析到同一份 path crate。这一点是硬要求：

- 图里只能有一份 `gpui` 类型，否则 `WindowOptions`、`Scene`、`CustomSurfaceHandle` 会出现
  两个不兼容的定义，编译器直接报类型不匹配；
- 只能有一个 `DirectXDevices`，否则会创建第二个 `ID3D11Device`，违反“同设备”约束。

验证方式：`cargo tree` 与 `Cargo.lock` 中 `gpui-pre` 只有一条记录，且 `gpui-pre-windows` 的
`gpui-pre` 依赖不产生第二个副本。

## 3. 为什么要 fork

G0 需要“在 GPUI 自己的场景里、用 GPUI 自己的 D3D11 设备、按 GPUI 的绘制次序画自定义 GPU 纹理”。
上游 0.3.12 在 Windows 上没有这条通路：

| 位置 | 上游状态 |
| --- | --- |
| `gpui-pre/src/window.rs:5405 paint_surface` | 整个函数体 `#[cfg(any(target_os = "macos", target_os = "ios"))]`，Windows 编译不到 |
| `gpui-pre/src/scene.rs:768 PaintSurface` | `image_buffer` 字段同样只在 macOS/iOS 存在 |
| `gpui-pre-windows/src/directx_renderer.rs:816 draw_surfaces` | `if surfaces.is_empty() { return Ok(()) } Ok(())` —— 空实现，surface 到不了帧缓冲 |

也就是说：`Scene.surfaces`、`PrimitiveBatch::Surfaces`、批次排序在 Windows 上都已存在且会被填充，
只有“提交到 GPU”这一步是空的。fork 只补这一步，不改 GPUI 的场景模型、布局、文字、输入或窗口管理。

## 4. 改动清单

全部改动集中在 7 个文件 + 1 处 Cargo.toml 特性，共 **683 行增 / 4 行删**（含注释），
不触碰任何 macOS/Linux 路径的行为。下表是 `git diff --numstat` 对上游 0.3.12 registry 源码的实测结果
（Task 10 GPU 直通与 Task 13 Present 统计桥都计入）：

| 文件 | 增 / 删 | 内容 |
| --- | --- | --- |
| `gpui-pre/src/scene.rs` | +16 / −0 | 新增 `CustomSurfaceHandle(u64)`；`PaintSurface` 增加 `#[cfg(target_os="windows")] custom_surface` 字段 |
| `gpui-pre/src/window.rs` | +104 / −1 | 新增 `paint_custom_surface(bounds, handle)`、`create_custom_surface` / `update_custom_surface` / `destroy_custom_surface` / `live_custom_surfaces`、`gpu_device_handle`、`register_gpu_surface`、`present_statistics`；原有 macOS `paint_surface` 未动 |
| `gpui-pre/src/platform.rs` | +109 / −1 | `PlatformWindow` trait 增加 7 个 `#[cfg(target_os="windows")]` 方法（surface 五件套 + `gpu_device_handle` + `register_gpu_surface` + `present_statistics`），默认实现返回 unsupported / 0；新增 `PresentStatistics` 结构体 |
| `gpui-pre-windows/src/directx_renderer.rs` | +359 / −0 | `SurfaceTextureRegistry`（RGBA8 上传 / `register_gpu` 同设备校验 / 设备丢失清空）、`SurfaceInstance`、`surface_pipeline`、`ShaderModule::Surface`、真正的 `draw_surfaces`、`present_statistics` 读 `IDXGISwapChain1::GetFrameStatistics` |
| `gpui-pre-windows/src/shaders.hlsl` | +46 / −0 | 新增 `surface` 模块：`surface_vertex` / `surface_fragment` |
| `gpui-pre-windows/build.rs` | +1 / −0 | release 预编译模块列表加入 `"surface"` |
| `gpui-pre-windows/src/window.rs` | +47 / −2 | `PlatformWindow for WindowsWindow` 转发到 renderer（含设备句柄、surface 注册、present 统计） |
| `gpui-pre/Cargo.toml` | +1 / −0 | `windows` crate 特性补 `Win32_Graphics_Direct3D11`（`gpu_device_handle` 的返回类型需要） |

### 4.4 Task 10 / Task 13 扩展

Task 2 只补了「surface 到帧缓冲」这一步。后续两个任务在同一 fork 上加了三条窄接口：

- **`gpu_device_handle()`**（Task 10）——把 GPUI 渲染器自己的 `ID3D11Device` / `ID3D11DeviceContext`
  借给调用方，使 Media Foundation 解码器与 NV12→BGRA 渲染器能开在同一设备上，避免跨设备拷贝。
- **`register_gpu_surface(&ID3D11Texture2D)`**（Task 10）——注册调用方**已有**的纹理（零 CPU 回读路径），
  注册时用 `GetDevice()` 做 COM 身份校验，拒绝跨设备纹理；返回单调递增的 `CustomSurfaceHandle`。
- **`present_statistics()`**（Task 13）——读 `IDXGISwapChain1::GetFrameStatistics`，返回
  `PresentStatistics { present_count, refresh_count, sync_qpc_time, ... }`。这是 G4「240 Hz」结论的唯一可信来源：
  UI 侧帧计数只能证明应用请求了多少帧，不能证明显示栈接受/观察到了多少帧。

### 4.1 场景层

`PaintSurface` 在 Windows 上携带 `CustomSurfaceHandle`，与 `order`、`bounds`、`content_mask` 一起进入
既有排序。`paint_custom_surface` 与上游 `paint_surface` 做同样的 `snap_bounds` / `snapped_content_mask`，
所以逻辑像素到设备像素的换算、裁剪、层级合并在两条路径上完全一致。

### 4.2 渲染层

- `SurfaceTextureRegistry`：`HashMap<u64, SurfaceTexture>`，id 单调递增不复用；纹理格式
  `DXGI_FORMAT_R8G8B8A8_UNORM`，`D3D11_USAGE_DEFAULT` + `D3D11_BIND_SHADER_RESOURCE`，
  上传走 `UpdateSubresource`。
- `draw_surfaces`：把 surface 映射成 `SurfaceInstance` 数组（`order` / `_padding` / `bounds` /
  `content_mask`，与 HLSL 结构体逐字段对齐），一次 `update_buffer`，然后按**连续相同 handle** 切 run，
  每个 run 绑定一张 `ID3D11ShaderResourceView` 并 `DrawInstanced(4, n)`。
  - 为什么按 run 切：像素着色器不能索引任意 `Texture2D` 数组，纹理身份留在 CPU 侧；
  - run 内仍是同一批实例，`batch_start_index` 偏移保证 `surface_items[id]` 对得上；
  - 未注册的 handle 直接 `with_context` 报错，不静默跳过。
- `handle_device_lost_impl` 里 `self.surfaces.clear()`：所有纹理都活在丢失的设备上，不重建。

### 4.3 着色器

```hlsl
struct SurfaceInstance { uint order; uint pad; Bounds bounds; Bounds content_mask; };
Texture2D<float4> t_surface : register(t0);
SamplerState s_surface : register(s0);
```

顶点着色器用 `unit_vertex = float2(vertex_id & 1, 0.5 * (vertex_id & 2))` 生成三角带，
`to_device_position` 与 `distance_from_clip_rect` 复用上游既有函数，因此裁剪语义与普通 quad 完全一致。
片元着色器只做一次采样，不引入额外色彩空间转换——颜色空间由调用方在纹理内容里决定。

## 5. 许可证与分发

`native/vendor/gpui-pre/LICENSE-APACHE` 与 `native/vendor/gpui-pre-windows/LICENSE-APACHE` 随包保留。
两个 crate 均为 Apache-2.0。fork 只在本试验工程内使用，不发布到 crates.io、不改包名、不改版本号，
因此上游无法据此产生版本冲突。

`vendor/` 内共 129 个文件（`gpui-pre` 3.5 MB、`gpui-pre-windows` 580 KB），保留了原始 `Cargo.toml.orig`，
便于用 `cargo vendor` 之外的方式复核来源。

## 6. 复现 fork

```bash
# 从 crates.io 取回 0.3.12 原始源码
cargo vendor --versioned-dirs native/vendor   # 或在临时工程 cargo fetch 后取 registry src

# 与本仓库的 fork 比较（应只列出下列 7 个文件）
diff -r <upstream>/gpui-pre-0.3.12 native/vendor/gpui-pre
diff -r <upstream>/gpui-pre-windows-0.3.12 native/vendor/gpui-pre-windows
```

实测差异行数（2026-11-15，Task 13 提交 `652eda8` 时点，含 Task 2 / 10 / 13 全部改动）：

```
gpui-pre/src/platform.rs                    +109  -1
gpui-pre/src/scene.rs                        +16  -0
gpui-pre/src/window.rs                      +104  -1
gpui-pre/Cargo.toml                           +1  -0
gpui-pre-windows/build.rs                     +1  -0
gpui-pre-windows/src/directx_renderer.rs    +359  -0
gpui-pre-windows/src/shaders.hlsl            +46  -0
gpui-pre-windows/src/window.rs               +47  -2
```

`diff -r` 还会报出上游有、vendor 没有的目录（`benches/`、`docs/`、`examples/`、`tests/`、
`.cargo-ok`、`Cargo.lock`）——那是 `cargo vendor` 的裁剪行为，不是 fork 改动。

## 7. 变更记录

| 日期 | 变更 |
| --- | --- |
| 2026-11-14 | 初版：Task 2 建立 fork，补 Windows 自定义 surface 通路 |
| 2026-11-15 | Task 10：新增 `gpu_device_handle()` 与 `register_gpu_surface(&ID3D11Texture2D)` 两条窄接口（借出 GPUI 自己的设备；注册已有纹理、COM 身份校验、单调 handle），`directx_renderer.rs` 的 `SurfaceTextureRegistry` 随之扩到 +359 行 |
| 2026-11-15 | Task 13：新增 `present_statistics()`（`IDXGISwapChain1::GetFrameStatistics` → `PresentStatistics`），G4 240 Hz 帧时的唯一可信来源；`platform.rs` / `window.rs` / `directx_renderer.rs` 同步加转发 |
| 2026-11-15 | Task 14：记录实测 8 项 numstat（合计 +683/−4）、上游 0.3.12 关键文件 sha256 与 `cargo vendor` 裁剪说明；toolchain 更正为 rustc 1.100.0（2026-11-09） |


## 2026-11-16 歌词字形扩展

- `gpui-pre/src/window.rs`：新增 Windows 专用 `paint_lyric_glyph`；复用现有 text system / sprite atlas，GPU 矩阵处理缩放和平移，固定字号且固定 subpixel variant，不随动画重栅格化。模糊覆盖在 atlas miss 时做带透明边距的可分离 Gaussian，缓存键含原始 glyph 参数和量化固定半径。
- `gpui-pre/src/platform.rs`：Windows 条件编译 `AtlasKey::BlurredGlyph`，使用 monochrome atlas。
- `gpui-pre/src/scene.rs`：Windows 下 MonochromeSprite 与 SubpixelSprite 同步追加两个 float4（32 bytes）；旧字形/SVG 初始化为零，保持共用 shader 的结构一致。
- `gpui-pre-windows/src/shaders.hlsl`：相同结构布局；fragment 以 SV_Position 的实际像素坐标采样横向歌词遮罩和上下羽化。普通 UI 字形 effects=0，沿用原路径。
- 验证：锁定依赖 release workspace 607 passed / 0 failed / 7 ignored，实际 D3D11 PNG 与字形内部像素探针；详见 `results/lyric-repair/`。没有声称跨平台字形扩展、完整 AMLL 保真或 C2 通过。升级上游必须同时重放 Rust sprite 与 HLSL ABI 变更，并核对实际画面。

### 歌词采样护边修复（同日）

- 新增 `gpui-pre/src/lyric_raster.rs`，从 `window.rs` 分离原纯 CPU Gaussian 函数；普通歌词（半径 0）和辉光（半径 >0）均在滤波支持范围外保留两层全透明像素。
- `paint_lyric_glyph` 对全部歌词使用独立 `BlurredGlyph(params, radius)` 缓存，半径 0 也不与普通 UI 的无护边字形共用。绘制时 atlas 子区域四边各内缩 1 texel，quad 原点相应补偿，确保双线性采样不会跨进相邻字形，同时保留原字号、字距、扫光与辉光。
- 只加一层边距但仍绘制完整 tile，边缘仍可能读到邻居，不能作为等价替代。不要用 point sampler 或关闭歌词缩放/辉光替代此修复。
- 直接编译纯函数测试可避开 vendor 的独立测试依赖图；证据见 `results/lyric-border/`。

### 液态玻璃光学与图标（同日）

本轮光学变化位于应用与 `oly-render`，没有新增 GPUI sprite ABI 修改。`glass/gpu.rs` 与 `shaders/glass.hlsl` 同步扩展 cbuffer 为 8 个 float4（128 bytes）；默认关闭新光学以保留旧参考。t1 只在最终合成时临时绑定原场景 SRV，draw 后解绑。复用图标位于 `oly-gpui-app/assets/icons/`，出处为旧项目 SVG；不得丢弃此前歌词采样护边。详见 `results/liquid-controls/`。

### 指针打光与弹性输入（同日续）

cbuffer 现为 9 个 float4（144 bytes），追加局部光源位置与能量；默认能量 0 保留参考输出，应用显式设置跟随指针的弹簧光源。高光限定在边缘，中心保持原采样。SVG 弹性使用已有 GPU TransformationMatrix，保持栅格尺寸与 atlas key 固定。歌词滚轮按 GPUI 屏幕位移直接传入引擎，不能再按 DOM wheel 符号取反。证据见 `results/wheel-liquid-input/`。
