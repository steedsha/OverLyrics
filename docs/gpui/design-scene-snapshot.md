# Scene Snapshot Hook — fork 第二扩展设计（Task 10）

目的：让场景内的玻璃 pass 能读取「面板之前已绘制内容」做整帧合成，替代 Task 8 的 CPU 场景字节路径（RESULTS Task 8 未证明项「玻璃 pass 与 GPUI 场景树整帧合成」归 Task 10）。

## 数据流

```
GPUI 绘制批次顺序执行
  ... (背景/歌词/封面等 primitives 已画进 BGRA8 RT) ...
  draw_surfaces(surfaces) 遇到第一个带 scene_snapshot 的 PaintSurface:
    1. CopySubresourceRegion(scene_tex, 0, x,y,0, render_target, 0, &box)
       —— RT 是 BGRA8 (RENDER_TARGET_FORMAT)，快照纹理也建 BGRA8，不做 swizzle
    2. 调用 app 注册的回调: pre_draw(user_data, &HookContext{device, ctx, snapshot_srv})
       app 在回调内跑玻璃 pass（oly-render GlassRenderer GPU 直通入口），
       输出写进 app 自己注册的 output GPU 表面（register_gpu_surface）
    3. 继续用 surface_pipeline 把 output 表面合成到 RT（现有 draw_surfaces 路径）
  ... (后续 primitives 继续画) ...
```

## fork 端新增（gpui-pre / gpui-pre-windows）

1. `PaintSurface` 加字段：
   ```rust
   #[cfg(target_os = "windows")]
   pub scene_snapshot: Option<CustomSurfaceHandle>,  // 该表面绘制前对 RT 的快照纹理句柄
   #[cfg(target_os = "windows")]
   pub pre_draw: Option<PaintHook>,
   ```
   ```rust
   #[cfg(target_os = "windows")]
   pub struct PaintHook {
       pub callback: unsafe fn(*mut std::ffi::c_void, *mut HookContext),
       pub user_data: *mut std::ffi::c_void,
   }
   ```
   （fn 指针非 trait object：PaintSurface 进 Scene，Scene 需 Debug/Clone 边界最小化；Task 10 只有一处调用方，fn 指针足够。）

2. `Window` 新增包装：
   ```rust
   pub fn paint_custom_surface_with_scene(
       &mut self, bounds, handle,
       scene_snapshot: CustomSurfaceHandle,
       pre_draw: Option<PaintHook>,
   )
   ```
   复用 `paint_custom_surface` 的 snap/mask 逻辑，插入两字段。

3. `directx_renderer.rs` 的 `draw_surfaces` 改造：
   - 实例收集循环里，遇 `scene_snapshot: Some(snap)` 且 `pre_draw: Some(hook)`：
     先 `copy_region_to_surface(snap, &rt, bounds)`（把 surface.bounds 对应 RT 区域拷进 app 注册的快照纹理——快照纹理本身也是 SurfaceTexture，尺寸=bounds 物理像素），
     再构造 `HookContext{ device, ctx, snapshot_srv: view(snap) }` 调 `hook.callback`，
     然后正常把 `custom_surface`（app 的 output 纹理）画出去。
   - 快照纹理由 app 通过 `register_gpu_surface` 注册（BGRA8、D3D11_BIND_SHADER_RESOURCE，app 自建），fork 只写不建。

4. 设备丢失：`self.surfaces.clear()` 已覆盖快照纹理（app 注册的），回调/output/快照句柄全部失效——与 GPU 注册表面同语义，调用方重注册。

## app 端（oly-render + oly-gpui-app，Task 10 实现）

- oly-render 加 `GlassRenderer::render_from_scene_texture(device, ctx, snapshot_srv, panel_px) -> ID3D11Texture2D`（GPU 直通玻璃：三 pass h→scratch0、v→scratch1、composite→output，全部同设备纹理，无 CPU 往返）。scratch 纹理复用/按 panel 尺寸缓存。
- oly-gpui-app 每帧：`let (dev,ctx) = window.gpu_device_handle()?` → 玻璃面板区域建/复用快照纹理 + output 纹理（`register_gpu_surface`）→ `paint_custom_surface_with_scene(...)`。

## 契约与边界

- 回调只在 paint 阶段、主线程、immediate context 顺序执行；回调内禁止 Present、禁止递归调 GPUI。
- 快照区域 = PaintSurface.bounds（物理像素，含 content_mask 裁剪前的 RT 全区域；mask 由 surface 绘制阶段处理）。
- RT BGRA8：快照纹理必须 BGRA8，否则 CopySubresourceRegion 失败。玻璃 pass 输入输出均 BGRA8 或按 shader swizzle。
- 本文档是设计记录；实现拆两步：fork 端（本仓 vendor，含编译验证）+ app 端（oly-render/oly-gpui-app，TDD）。
