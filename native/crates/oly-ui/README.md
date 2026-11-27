# `oly-ui` — Reusable Liquid Glass GPUI Component Toolkit

`oly-ui` packages the domain-independent UI controls, motion physics, Liquid Glass D3D11 surface hooks, geometry/color utilities, and Win32 overlay window helpers extracted from OverLyrics so other GPUI desktop applications can reuse them directly.

## Dependency Boundary

- **Allowed**: `gpui-kit`, `oly-core` (spring solver), `oly-render` (glass shaders), `raw-window-handle`, `windows` (Win32/D3D11/DWM).
- **Forbidden**: `oly-engine`, `oly-contracts`, `oly-platform-win`, or any music/lyrics domain state.

## Modules

| Module | Contents |
| :--- | :--- |
| `theme` | Explicit `Theme` value object (dark/light mode, accent color, surface/border/text tokens) |
| `chrome` | Liquid Glass controls (`paint_slider`, `paint_switch*`, `SegmentedLens`, `paint_stepper`, `paint_choice_pill*`, `paint_list_item`, `paint_glass_button`, `paint_glass_input`, `progress_rail*`, text & rounded-rect primitives) |
| `glass` | `GlassPanel` D3D11 backdrop-sampling paint hook (`PaintHook` integration) |
| `elastic` | `Elastic` rubber-band pointer deformation & `PressGesture` drag-out cancellation |
| `panel_motion` | Frame-rate-independent (60–240Hz) `PanelMotion` spring & specular light tracking |
| `interaction` | `Feedback` (hover/press/flash/hop), `panel_pixels`, `state_icon_spring` |
| `details` | Compact control geometry (`SwitchDrag`, `switch_knob`, `side_panel_sized`) |
| `scrollbar` | Spring-deformed glass scrollbar capsule geometry |
| `geometry` | `Box4` 2D rectangle primitive |
| `color` | `rgb_to_hsv`, `hsv_to_rgb`, `is_dark_color`, `smoothstep01` |
| `win32_window` | Borderless transparent overlay setup (`DWMWCP_DONOTROUND`, `DWMWA_COLOR_NONE`), click-through toggle, physical-pixel move/resize, global cursor polling & hotkey listener |
