//! Project authored light origins into the native child viewport. The numbered
//! badges sit above the scene, so they never enter a bake or a reflection capture.
use super::{Bounds, Result};
use serde_json::Value;
use windows::{
    core::w,
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
};

fn screen_position(
    origin: [f64; 3],
    eye: [f64; 3],
    matrix: &[f64; 16],
    width: u32,
    height: u32,
) -> Option<(i32, i32)> {
    let [x, y, z] = std::array::from_fn(|i| origin[i] - eye[i]);
    let clip =
        |row: usize| matrix[row] * x + matrix[4 + row] * y + matrix[8 + row] * z + matrix[12 + row];
    let w = clip(3);
    if w <= 0.0 || clip(2) < 0.0 {
        return None;
    }
    let (u, v) = (clip(0) / w, clip(1) / w);
    if u.abs() > 1.0 || v.abs() > 1.0 {
        return None;
    }
    Some((
        ((1.0 + u) * 0.5 * width as f64).round() as i32,
        ((1.0 - v) * 0.5 * height as f64).round() as i32,
    ))
}

struct Badge {
    window: HWND,
    placement: Option<(i32, i32, i32, i32)>,
}

#[derive(Default)]
pub(super) struct LightMarkers {
    badges: Vec<Badge>,
}

impl LightMarkers {
    pub(super) fn update(
        &mut self,
        parent: HWND,
        view: &Value,
        bounds: &Bounds,
        has_scene: bool,
    ) -> Result<()> {
        let origins = view["lightMarkers"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let count = if has_scene { origins.len() } else { 0 };
        // The viewport owns the children. Destroy surplus badges when toggled off.
        for badge in self.badges.drain(count.min(self.badges.len())..) {
            unsafe {
                let _ = DestroyWindow(badge.window);
            }
        }
        if count == 0 {
            return Ok(());
        }
        let eye: [f64; 3] = serde_json::from_value(view["camera"]["position"].clone())
            .map_err(|e| e.to_string())?;
        let matrix: [f64; 16] =
            serde_json::from_value(view["viewProjection"].clone()).map_err(|e| e.to_string())?;
        let (_, _, width, height) = bounds.pixels();
        let unit = |n: f64| (n * bounds.scale).round() as i32;
        for (index, origin) in origins.iter().enumerate() {
            let origin = serde_json::from_value(origin.clone()).map_err(|e| e.to_string())?;
            if index == self.badges.len() {
                self.badges.push(Badge {
                    window: unsafe { create_badge(parent, index + 1)? },
                    placement: None,
                });
            }
            let badge = &mut self.badges[index];
            let position = screen_position(origin, eye, &matrix, width, height).map(|(x, y)| {
                (
                    x - unit(9.0),
                    y - unit(9.0),
                    unit(60.0 + 7.0 * (index + 1).to_string().len() as f64),
                    unit(20.0),
                )
            });
            if badge.placement == position {
                continue;
            }
            unsafe {
                if let Some((x, y, w, h)) = position {
                    SetWindowPos(
                        badge.window,
                        Some(HWND_BOTTOM),
                        x,
                        y,
                        w,
                        h,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    )
                    .map_err(|e| e.to_string())?;
                } else {
                    let _ = ShowWindow(badge.window, SW_HIDE);
                }
            }
            badge.placement = position;
        }
        Ok(())
    }
}

unsafe fn create_badge(parent: HWND, number: usize) -> Result<HWND> {
    let module = GetModuleHandleW(None).map_err(|e| e.to_string())?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(badge_proc),
        hInstance: HINSTANCE(module.0),
        lpszClassName: w!("WaWLightMarker"),
        ..Default::default()
    };
    RegisterClassW(&class);
    let title = windows::core::HSTRING::from(format!("Light {number}"));
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class.lpszClassName,
        &title,
        WS_CHILD | WS_DISABLED,
        0,
        0,
        0,
        0,
        Some(parent),
        None,
        Some(class.hInstance),
        None,
    )
    .map_err(|e| e.to_string())
}

unsafe extern "system" fn badge_proc(
    window: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut paint);
            let mut rect = RECT::default();
            let _ = GetClientRect(window, &mut rect);
            let unit = |n: i32| (n * rect.bottom + 10) / 20;
            let brush = GetStockObject(DC_BRUSH);
            let old_brush = SelectObject(dc, brush);
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            SetDCBrushColor(dc, COLORREF(0x00231e19));
            FillRect(dc, &rect, HBRUSH(brush.0));
            SetDCBrushColor(dc, COLORREF(0x0067dcff));
            let _ = Ellipse(dc, unit(4), unit(3), unit(15), unit(14));
            let _ = Rectangle(dc, unit(7), unit(12), unit(12), unit(17));
            let font = CreateFontW(
                -unit(13),
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
            let mut text = [0u16; 32];
            let len = GetWindowTextW(window, &mut text) as usize;
            rect.left = unit(20);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00e8dacf));
            DrawTextW(
                dc,
                &mut text[..len],
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
            SelectObject(dc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            let _ = EndPaint(window, &paint);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(window, message, wp, lp)
}
