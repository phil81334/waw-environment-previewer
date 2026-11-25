//! Private D3D9 child viewport. All HWND/COM state remains on Tauri's UI thread.
//! Only immutable scene Arcs and plain JSON settings cross thread boundaries.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::HashSet,
    ffi::c_void,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use waw_environment_previewer::{
    d3d9::Dx9Renderer, level_fx::PreparedLevelFx, scene_fx::PreparedEffect, BakedScene,
};
use windows::{
    core::w,
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
            InvalidateRect, SelectObject, SetBkMode, SetTextColor, ValidateRect, CLEARTYPE_QUALITY,
            CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_LEFT, DT_NOPREFIX, DT_TOP, HBRUSH, HGDIOBJ,
            OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus},
            WindowsAndMessaging::*,
        },
    },
};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Deserialize, Serialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    pub visible: bool,
}
impl Bounds {
    fn validate(&self) -> Result<()> {
        if ![self.x, self.y, self.width, self.height, self.scale]
            .iter()
            .all(|x| x.is_finite())
            || self.x.abs() > 32768.0
            || self.y.abs() > 32768.0
            || self.width < 0.0
            || self.height < 0.0
            || self.scale <= 0.0
            || self.scale > 8.0
            || self.width * self.scale > 8192.0
            || self.height * self.scale > 8192.0
        {
            return Err("Invalid native viewport bounds.".into());
        }
        Ok(())
    }
    fn pixels(&self) -> (i32, i32, u32, u32) {
        (
            (self.x * self.scale).round() as i32,
            (self.y * self.scale).round() as i32,
            (self.width * self.scale).round().max(1.0) as u32,
            (self.height * self.scale).round().max(1.0) as u32,
        )
    }
}
#[derive(Default)]
struct Input {
    dragging: bool,
    escape: bool,
    last: [i32; 2],
    rotate: [f64; 2],
    wheel: f64,
    keys: HashSet<u32>,
    scale: f64,
}
thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static INPUT: RefCell<Input> = RefCell::new(Input::default());
}
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let point = [
        lparam.0 as u16 as i16 as i32,
        (lparam.0 >> 16) as u16 as i16 as i32,
    ];
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let _ = ValidateRect(Some(window), None);
            return LRESULT(0);
        }
        WM_RBUTTONDOWN => {
            let _ = SetFocus(Some(window));
            SetCapture(window);
            INPUT.with(|input| {
                let mut input = input.borrow_mut();
                input.dragging = true;
                input.last = point;
            });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let _ = SetFocus(Some(window));
        }
        WM_RBUTTONUP | WM_CAPTURECHANGED => {
            INPUT.with(|input| input.borrow_mut().dragging = false);
            if message != WM_CAPTURECHANGED {
                let _ = ReleaseCapture();
            }
            return LRESULT(0);
        }
        WM_CONTEXTMENU => return LRESULT(0),
        WM_MOUSEMOVE => {
            INPUT.with(|input| {
                let mut input = input.borrow_mut();
                if input.dragging {
                    let scale = input.scale.max(0.1);
                    input.rotate[0] += (point[0] - input.last[0]) as f64 / scale;
                    input.rotate[1] += (point[1] - input.last[1]) as f64 / scale;
                }
                input.last = point;
            });
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let _ = SetFocus(Some(window));
            let delta = (wparam.0 >> 16) as u16 as i16 as f64 / 120.0;
            INPUT.with(|input| input.borrow_mut().wheel += delta);
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let code = wparam.0 as u32;
            if code == 0x1b {
                INPUT.with(|input| input.borrow_mut().escape = true);
                return LRESULT(0);
            }
            if [0x57, 0x41, 0x53, 0x44, 0x51, 0x45, 0x10].contains(&code) {
                INPUT.with(|input| {
                    input.borrow_mut().keys.insert(code);
                });
                return LRESULT(0);
            }
        }
        WM_KEYUP | WM_SYSKEYUP => {
            INPUT.with(|input| {
                input.borrow_mut().keys.remove(&(wparam.0 as u32));
            });
        }
        WM_KILLFOCUS => {
            let _ = ReleaseCapture();
            INPUT.with(|input| {
                let scale = input.borrow().scale;
                *input.borrow_mut() = Input {
                    scale,
                    ..Input::default()
                };
            });
        }
        _ => {}
    }
    DefWindowProcW(window, message, wparam, lparam)
}
unsafe fn create_child(parent: HWND) -> Result<HWND> {
    let module = GetModuleHandleW(None).map_err(|e| e.to_string())?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: HINSTANCE(module.0),
        hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())?,
        hbrBackground: HBRUSH::default(),
        lpszClassName: w!("WaWEnvironmentViewport"),
        ..Default::default()
    };
    RegisterClassW(&class);
    let style = GetWindowLongPtrW(parent, GWL_STYLE);
    SetWindowLongPtrW(parent, GWL_STYLE, style | WS_CLIPCHILDREN.0 as isize);
    let mut sibling = GetWindow(parent, GW_CHILD).ok();
    while let Some(window) = sibling {
        let style = GetWindowLongPtrW(window, GWL_STYLE);
        SetWindowLongPtrW(window, GWL_STYLE, style | WS_CLIPSIBLINGS.0 as isize);
        sibling = GetWindow(window, GW_HWNDNEXT).ok();
    }
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("WaWEnvironmentViewport"),
        w!("Native Direct3D 9 scene"),
        WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
        0,
        0,
        2,
        2,
        Some(parent),
        None,
        Some(HINSTANCE(module.0)),
        None,
    )
    .map_err(|e| e.to_string())
}
// Native child windows sit above the WebView. A small non-interactive native
// legend therefore stays visible without changing scene/shader output or GPU
// capture pixels. Its paint handler never borrows HOST (Win32 may re-enter it).
unsafe extern "system" fn info_overlay_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let packed = GetWindowLongPtrW(window, GWLP_USERDATA);
            let dirty = packed & 1 != 0;
            let size = ((packed >> 1) as i32).clamp(10, 96);
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut paint);
            let mut rect = RECT::default();
            let _ = GetClientRect(window, &mut rect);
            let brush = CreateSolidBrush(COLORREF(0x002a2219));
            let _ = FillRect(dc, &rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let font = CreateFontW(
                -size,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let old_font = SelectObject(dc, HGDIOBJ(font.0));
            SetTextColor(dc, COLORREF(0x00e8dacf));
            SetBkMode(dc, TRANSPARENT);
            let state = if dirty { "settings changed" } else { "current" };
            let mut text: Vec<u16> = format!("Live: Vision, fog, visibility, sky playback\nBake: lighting, worldspawn, probe capture\nBake: {state}").encode_utf16().collect();
            let margin = (size * 2 / 3).max(4);
            rect.left += margin;
            rect.top += margin;
            rect.right -= margin;
            rect.bottom -= margin;
            DrawTextW(dc, &mut text, &mut rect, DT_LEFT | DT_TOP | DT_NOPREFIX);
            SelectObject(dc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = EndPaint(window, &paint);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(window, message, wparam, lparam)
}

unsafe fn create_info_overlay(parent: HWND) -> Result<HWND> {
    let module = GetModuleHandleW(None).map_err(|e| e.to_string())?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(info_overlay_proc),
        hInstance: HINSTANCE(module.0),
        lpszClassName: w!("WaWEnvironmentInfoOverlay"),
        ..Default::default()
    };
    RegisterClassW(&class);
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("WaWEnvironmentInfoOverlay"),
        w!("Preview information"),
        WS_CHILD | WS_DISABLED,
        0,
        0,
        2,
        2,
        Some(parent),
        None,
        Some(HINSTANCE(module.0)),
        None,
    )
    .map_err(|e| e.to_string())
}

struct Backup {
    scene: Option<Arc<BakedScene>>,
    prepared_fx: Option<Arc<PreparedEffect>>,
    fx_source_report: Value,
    cubes: Value,
    view: Value,
}

fn hidden_objects(view: &Value) -> impl Iterator<Item = &str> {
    view["objectVisibility"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, visible)| visible.as_bool() == Some(false))
        .map(|(id, _)| id.as_str())
}

struct Host {
    window: HWND,
    info_overlay: Option<HWND>,
    parent: HWND,
    game: PathBuf,
    owner: std::thread::ThreadId,
    bounds: Bounds,
    renderer: Option<Dx9Renderer>,
    scene: Option<Arc<BakedScene>>,
    prepared_fx: Option<Arc<PreparedEffect>>,
    fx_source_report: Value,
    view: Value,
    cubes: Value,
    previous: Option<Backup>,
    last_tick: Instant,
    last_error: Option<String>,
}
impl Host {
    fn update_info_overlay(&mut self) -> Result<()> {
        self.assert_owner();
        let enabled = self.view["infoOverlay"]["enabled"]
            .as_bool()
            .unwrap_or(false);
        if !enabled || !self.bounds.visible {
            if let Some(window) = self.info_overlay {
                unsafe {
                    let _ = ShowWindow(window, SW_HIDE);
                }
            }
            return Ok(());
        }
        let overlay = if let Some(window) = self.info_overlay {
            window
        } else {
            let window = unsafe { create_info_overlay(self.window)? };
            self.info_overlay = Some(window);
            window
        };
        let (_, _, width, height) = self.bounds.pixels();
        let scale = self.bounds.scale;
        let padding = (8.0 * scale).round() as i32;
        let overlay_width = (440.0 * scale).round().min(width.saturating_sub(16) as f64) as i32;
        let overlay_height = (74.0 * scale).round().min(height.saturating_sub(16) as f64) as i32;
        if overlay_width < 120 || overlay_height < 40 {
            unsafe {
                let _ = ShowWindow(overlay, SW_HIDE);
            }
            return Ok(());
        }
        let dirty = self.view["infoOverlay"]["dirty"].as_bool().unwrap_or(false);
        let font_size = (12.0 * scale).round().clamp(10.0, 96.0) as isize;
        unsafe {
            SetWindowLongPtrW(
                overlay,
                GWLP_USERDATA,
                (font_size << 1) | isize::from(dirty),
            );
            SetWindowPos(
                overlay,
                Some(HWND_TOP),
                padding,
                padding,
                overlay_width,
                overlay_height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .map_err(|e| e.to_string())?;
            let _ = InvalidateRect(Some(overlay), None, false);
        }
        Ok(())
    }
    fn assert_owner(&self) {
        assert_eq!(self.owner, std::thread::current().id());
    }
    fn renderer(&mut self) -> Result<&mut Dx9Renderer> {
        self.assert_owner();
        if self.renderer.is_none() {
            let mut renderer = Dx9Renderer::new(self.window.0 as isize, &self.game)?;
            let (_, _, width, height) = self.bounds.pixels();
            renderer.resize(width, height)?;
            renderer.set_view(self.view.clone())?;
            if let Some(scene) = &self.scene {
                renderer.load_scene(scene)?;
                // Uploading a scene clears the renderer's visibility overrides.
                // Reapply the committed choices when recovering a previous bake.
                for id in hidden_objects(&self.view) {
                    renderer.set_object_visibility(id, false);
                }
            }
            if !self.cubes.is_null() {
                renderer.set_probe_cubes(&self.cubes)?;
            }
            if let Some(prepared) = &self.prepared_fx {
                renderer.load_fx(prepared)?;
            }
            self.renderer = Some(renderer);
        }
        self.renderer
            .as_mut()
            .ok_or_else(|| "The native renderer is unavailable.".into())
    }
    fn matrix(&mut self) {
        let camera = &self.view["camera"];
        let yaw = camera["yaw"].as_f64().unwrap_or(0.0);
        let pitch = camera["pitch"].as_f64().unwrap_or(0.0);
        let fov = camera["fov"].as_f64().unwrap_or(51.0771934667);
        let (_, _, width, height) = self.bounds.pixels();
        let aspect = width as f64 / height.max(1) as f64;
        let forward = [
            pitch.cos() * yaw.cos(),
            pitch.cos() * yaw.sin(),
            pitch.sin(),
        ];
        let right = [yaw.sin(), -yaw.cos(), 0.0];
        let up = [
            -pitch.sin() * yaw.cos(),
            -pitch.sin() * yaw.sin(),
            pitch.cos(),
        ];
        let a = 2047.0 / 2048.0;
        let f = a / (fov * std::f64::consts::PI / 360.0).tan();
        let projection = [
            f / aspect,
            0.0,
            0.0,
            0.0,
            0.0,
            f,
            0.0,
            0.0,
            0.0,
            0.0,
            a,
            1.0,
            0.0,
            0.0,
            -4.0 * a,
            0.0,
        ];
        let basis = [
            right[0], up[0], forward[0], 0.0, right[1], up[1], forward[1], 0.0, right[2], up[2],
            forward[2], 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let mut matrix = [0.0; 16];
        for column in 0..4 {
            for row in 0..4 {
                for inner in 0..4 {
                    matrix[column * 4 + row] +=
                        projection[inner * 4 + row] * basis[column * 4 + inner];
                }
            }
        }
        self.view["viewProjection"] = json!(matrix);
    }
    fn input(&mut self, dt: f64) -> Option<Value> {
        let (rotate, wheel, keys) = INPUT.with(|input| {
            let mut input = input.borrow_mut();
            let result = (input.rotate, input.wheel, input.keys.clone());
            input.rotate = [0.0; 2];
            input.wheel = 0.0;
            result
        });
        if rotate == [0.0; 2] && wheel == 0.0 && keys.is_empty() {
            return None;
        }
        let mut camera = self.view["camera"].clone();
        let yaw = camera["yaw"].as_f64().unwrap_or(0.0) - rotate[0] * 0.004;
        let pitch =
            (camera["pitch"].as_f64().unwrap_or(0.0) - rotate[1] * 0.004).clamp(-1.55, 1.55);
        let forward = [
            pitch.cos() * yaw.cos(),
            pitch.cos() * yaw.sin(),
            pitch.sin(),
        ];
        let right = [yaw.sin(), -yaw.cos(), 0.0];
        let mut move_by = [0.0; 3];
        for axis in 0..3 {
            if keys.contains(&0x57) {
                move_by[axis] += forward[axis];
            }
            if keys.contains(&0x53) {
                move_by[axis] -= forward[axis];
            }
            if keys.contains(&0x44) {
                move_by[axis] += right[axis];
            }
            if keys.contains(&0x41) {
                move_by[axis] -= right[axis];
            }
        }
        if keys.contains(&0x45) {
            move_by[2] += 1.0;
        }
        if keys.contains(&0x51) {
            move_by[2] -= 1.0;
        }
        let length = move_by.iter().map(|v| v * v).sum::<f64>().sqrt();
        let speed = 160.0 * if keys.contains(&0x10) { 3.0 } else { 1.0 };
        let mut position = [0.0; 3];
        for axis in 0..3 {
            position[axis] = camera["position"][axis].as_f64().unwrap_or(0.0)
                + if length > 1e-12 {
                    move_by[axis] / length * speed * dt.min(0.1)
                } else {
                    0.0
                }
                + forward[axis] * wheel * 24.0;
        }
        camera["position"] = json!(position);
        camera["yaw"] = json!(yaw);
        camera["pitch"] = json!(pitch);
        self.view["camera"] = camera.clone();
        self.matrix();
        Some(camera)
    }
    fn draw(&mut self) -> Result<Option<Value>> {
        self.assert_owner();
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        if self.last_error.is_some() {
            return Ok(None);
        }
        if !self.bounds.visible
            || self.scene.is_none()
            || unsafe { IsIconic(self.parent).as_bool() }
        {
            return Ok(None);
        }
        let camera = self.input(dt);
        // UI changes and resizes already update the renderer. Only native input
        // changes the camera here. Avoid copying the full view on idle ticks.
        if camera.is_some() {
            let view = self.view.clone();
            self.renderer()?.set_view(view)?;
        }
        self.renderer()?.render()?;
        self.last_error = None;
        Ok(camera)
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.assert_owner();
        self.renderer.take();
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }
}

pub fn set_bounds(parent: isize, game: &Path, bounds: Bounds) -> Result<()> {
    bounds.validate()?;
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|h| h.game != game || h.parent.0 as isize != parent)
        {
            slot.take();
        }
        if slot.is_none() && bounds.width >= 2.0 && bounds.height >= 2.0 {
            let parent = HWND(parent as *mut c_void);
            let window = unsafe { create_child(parent)? };
            *slot = Some(Host {
                window,
                info_overlay: None,
                parent,
                game: game.to_owned(),
                owner: std::thread::current().id(),
                bounds: bounds.clone(),
                renderer: None,
                scene: None,
                prepared_fx: None,
                fx_source_report: Value::Null,
                view: json!({}),
                cubes: Value::Null,
                previous: None,
                last_tick: Instant::now(),
                last_error: None,
            });
        }
        if let Some(host) = slot.as_mut() {
            host.assert_owner();
            let (_, _, oldw, oldh) = host.bounds.pixels();
            let (x, y, width, height) = bounds.pixels();
            host.bounds = bounds.clone();
            if bounds.visible && width >= 2 && height >= 2 {
                unsafe {
                    SetWindowPos(
                        host.window,
                        Some(HWND_TOP),
                        x,
                        y,
                        width as i32,
                        height as i32,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    )
                    .map_err(|e| e.to_string())?;
                }
                if oldw != width || oldh != height {
                    host.matrix();
                    let view = host.view.clone();
                    if let Some(renderer) = host.renderer.as_mut() {
                        renderer.resize(width, height)?;
                        renderer.set_view(view)?;
                    }
                }
            } else {
                unsafe {
                    let _ = ShowWindow(host.window, SW_HIDE);
                }
                INPUT.with(|input| {
                    *input.borrow_mut() = Input {
                        scale: bounds.scale,
                        ..Input::default()
                    };
                });
            }
            INPUT.with(|input| input.borrow_mut().scale = bounds.scale);
            host.last_tick = Instant::now();
            host.update_info_overlay()?;
        }
        Ok(())
    })
}
pub fn load_scene(scene: Arc<BakedScene>) -> Result<Value> {
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let host = slot.as_mut().ok_or("The native viewport is not ready.")?;
        if host.previous.is_some() {
            return Err("Finish or cancel the pending native scene first.".into());
        }
        host.renderer()?.load_scene(&scene)?;
        host.renderer()?.clear_fx()?;
        host.previous = Some(Backup {
            scene: host.scene.replace(scene),
            prepared_fx: host.prepared_fx.take(),
            fx_source_report: host.fx_source_report.take(),
            cubes: host.cubes.take(),
            view: host.view.clone(),
        });
        if let Some(view) = host.view.as_object_mut() {
            view.remove("objectVisibility");
        }
        host.last_error = None;
        Ok(host.renderer()?.diagnostics())
    })
}
pub fn capture_face(origin: [f32; 3], matrix: [f32; 16]) -> Result<Vec<u8>> {
    if origin.iter().chain(matrix.iter()).any(|v| !v.is_finite()) {
        return Err("Invalid reflection capture view.".into());
    }
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let host = slot.as_mut().ok_or("The native viewport is not ready.")?;
        host.renderer()?.capture_face(origin, matrix)
    })
}
pub fn load_level_fx(scene_id: &str, prepared: PreparedLevelFx) -> Result<Value> {
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let host = slot
            .as_mut()
            .ok_or("The native level viewport is not ready.")?;
        if host.previous.is_some()
            || host
                .scene
                .as_ref()
                .is_none_or(|scene| scene.manifest.id != scene_id)
        {
            return Err("The committed level changed while its FX were preparing.".into());
        }
        if let Some(effect) = prepared.effect {
            host.renderer()?.load_fx(&effect)?;
            host.prepared_fx = Some(Arc::new(effect));
        } else {
            host.renderer()?.clear_fx()?;
            host.prepared_fx = None;
        }
        host.fx_source_report = prepared.report;
        Ok(host.fx_source_report.clone())
    })
}
pub fn set_probe_cubes(cubes: Value) -> Result<()> {
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let host = slot.as_mut().ok_or("The native viewport is not ready.")?;
        host.renderer()?.set_probe_cubes(&cubes)?;
        host.cubes = cubes;
        host.previous = None;
        Ok(())
    })
}
pub fn abort_scene() -> Result<()> {
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let host = slot.as_mut().ok_or("The native viewport is not ready.")?;
        let Some(backup) = host.previous.take() else {
            return Ok(());
        };
        host.scene = backup.scene;
        host.prepared_fx = backup.prepared_fx;
        host.fx_source_report = backup.fx_source_report;
        host.cubes = backup.cubes;
        host.view = backup.view;
        host.renderer.take();
        host.last_error = None;
        if host.scene.is_some() {
            host.renderer()?;
        }
        Ok(())
    })
}
pub fn set_view(view: Value) -> Result<()> {
    if !view.is_object() {
        return Err("Invalid native viewport settings.".into());
    }
    if let Some(camera) = view.get("camera") {
        if camera["position"].as_array().is_none_or(|p| {
            p.len() != 3 || p.iter().any(|v| v.as_f64().is_none_or(|n| !n.is_finite()))
        }) || ["yaw", "pitch", "fov"]
            .iter()
            .any(|key| camera[key].as_f64().is_none_or(|n| !n.is_finite()))
            || !(1.0..179.0).contains(&camera["fov"].as_f64().unwrap_or(0.0))
        {
            return Err("Invalid native camera.".into());
        }
    }
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let host = slot.as_mut().ok_or("The native viewport is not ready.")?;
        for (key, value) in view.as_object().unwrap() {
            host.view[key] = value.clone();
        }
        if view.get("infoOverlay").is_some() {
            host.update_info_overlay()?;
        }
        let next = host.view.clone();
        if let Some(renderer) = host.renderer.as_mut() {
            renderer.set_view(next)?;
        }
        Ok(())
    })
}
pub fn set_object_visibility(id: &str, visible: bool) -> Result<()> {
    if id.is_empty() || id.len() > 128 {
        return Err("Invalid object identifier.".into());
    }
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let host = slot.as_mut().ok_or("The native viewport is not ready.")?;
        host.view["objectVisibility"][id] = json!(visible);
        if let Some(renderer) = host.renderer.as_mut() {
            renderer.set_object_visibility(id, visible);
        }
        Ok(())
    })
}
pub fn diagnostics() -> Value {
    HOST.with(|slot| {
        let slot = slot.borrow();
        let Some(host) = slot.as_ref() else {
            return json!({"native": true, "api": "Direct3D 9", "ready": false});
        };
        json!({
            "native": true,
            "api": "Direct3D 9",
            "bounds": host.bounds,
            "sceneId": host.scene.as_ref().map(|scene| &scene.manifest.id),
            "camera": host.view["camera"],
            "renderer": host.renderer.as_ref().map(Dx9Renderer::diagnostics),
            "error": host.last_error,
            "pendingScene": host.previous.is_some(),
            "infoOverlay": {
                "enabled": host.view["infoOverlay"]["enabled"],
                "dirty": host.view["infoOverlay"]["dirty"],
                "created": host.info_overlay.is_some()
            }
        })
    })
}
pub fn capture() -> Result<Value> {
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let host = slot.as_mut().ok_or("The native viewport is not ready.")?;
        host.draw()?;
        let (width, height, rgba) = host.renderer()?.readback()?;
        Ok(json!({"width":width,"height":height,"rgba":rgba}))
    })
}
pub fn take_escape() -> bool {
    INPUT.with(|input| std::mem::take(&mut input.borrow_mut().escape))
}
pub fn tick() -> (Option<Value>, Option<String>) {
    HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(host) = slot.as_mut() else {
            return (None, None);
        };
        match host.draw() {
            Ok(camera) => (camera, None),
            Err(error) => {
                let changed = host.last_error.as_ref() != Some(&error);
                host.last_error = Some(error.clone());
                (None, changed.then_some(error))
            }
        }
    })
}
pub fn shutdown() {
    HOST.with(|slot| {
        slot.borrow_mut().take();
    });
    INPUT.with(|input| *input.borrow_mut() = Input::default());
}

#[cfg(test)]
mod tests {
    use super::Bounds;
    #[test]
    fn renderer_recovery_replays_hidden_objects_only() {
        let saved = serde_json::json!({"objectVisibility": {
            "hidden-model": false, "visible-model": true
        }});
        assert_eq!(
            super::hidden_objects(&saved).collect::<Vec<_>>(),
            ["hidden-model"]
        );
        assert_eq!(super::hidden_objects(&serde_json::json!({})).count(), 0);
    }
    fn bounds() -> Bounds {
        Bounds {
            x: 24.5,
            y: 42.0,
            width: 1024.0,
            height: 576.0,
            scale: 1.5,
            visible: true,
        }
    }
    #[test]
    fn css_bounds_convert_to_native_client_pixels() {
        let b = bounds();
        assert!(b.validate().is_ok());
        assert_eq!(b.pixels(), (37, 63, 1536, 864));
    }
    #[test]
    fn dpi_preserves_game_frame_aspect() {
        let b = bounds();
        let (_, _, w, h) = b.pixels();
        assert_eq!(w * 9, h * 16);
    }
    #[test]
    fn invalid_physical_extent_is_rejected_before_window_calls() {
        let mut b = bounds();
        b.width = 8193.0;
        assert!(b.validate().is_err());
        b = bounds();
        b.scale = 0.0;
        assert!(b.validate().is_err());
        b = bounds();
        b.height = -1.0;
        assert!(b.validate().is_err());
    }
    #[test]
    fn nonfinite_coordinates_are_rejected_before_window_calls() {
        let mut b = bounds();
        b.x = f64::NAN;
        assert!(b.validate().is_err());
        b = bounds();
        b.scale = f64::INFINITY;
        assert!(b.validate().is_err());
    }
    #[test]
    fn temporarily_hidden_zero_area_is_valid() {
        let mut b = bounds();
        b.width = 0.0;
        b.height = 0.0;
        b.visible = false;
        assert!(b.validate().is_ok());
    }
}
