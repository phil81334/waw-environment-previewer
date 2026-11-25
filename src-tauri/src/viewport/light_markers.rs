//! Project authored light origins into the native child viewport. The numbered
//! badges sit above the scene, so they never enter a bake or a reflection capture.
use super::{Bounds, Result};
use serde_json::Value;
use windows::{
    core::w,
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::{LibraryLoader::GetModuleHandleW, SystemServices::SS_WHITERECT},
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

// Keep the bulb close to its projected origin. If labels share a column, the
// nearest free space begins at a viewport edge or at the edge of another label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LabelRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

fn place_label(
    anchor: (i32, i32),
    size: (i32, i32),
    viewport: (i32, i32),
    inset: i32,
    gap: i32,
    occupied: &[LabelRect],
) -> Option<LabelRect> {
    let (width, height) = size;
    if width > viewport.0 || height > viewport.1 {
        return None;
    }
    let x = (anchor.0 - inset).clamp(0, viewport.0 - width);
    let desired_y = (anchor.1 - inset).clamp(0, viewport.1 - height);
    let neighbours = || {
        occupied
            .iter()
            .filter(|r| x < r.x + r.width + gap && r.x < x + width + gap)
    };
    let y = std::iter::once(desired_y)
        .chain(neighbours().flat_map(|r| [r.y + r.height + gap, r.y - height - gap]))
        .filter(|&y| y >= 0 && y + height <= viewport.1)
        .filter(|&y| neighbours().all(|r| y >= r.y + r.height + gap || y + height + gap <= r.y))
        .min_by_key(|&y| (y - desired_y).abs())?;
    Some(LabelRect {
        x,
        y,
        width,
        height,
    })
}

struct Badge {
    window: HWND,
    placement: Option<LabelRect>,
    // A separate, narrow child leaves the scene visible between the tag and light.
    leader: Option<HWND>,
    leader_placement: Option<LabelRect>,
}

impl Badge {
    unsafe fn update(
        &mut self,
        parent: HWND,
        label: Option<LabelRect>,
        anchor: Option<(i32, i32)>,
        line_width: i32,
        viewport_width: i32,
    ) -> Result<()> {
        if self.placement != label {
            move_overlay(self.window, label, false)?;
            self.placement = label;
        }
        let line = label.zip(anchor).and_then(|(label, (x, y))| {
            let edge = y.clamp(label.y, label.y + label.height - 1);
            (edge != y).then_some(LabelRect {
                x,
                y: y.min(edge),
                width: line_width.min(viewport_width - x),
                height: (y - edge).abs() + 1,
            })
        });
        if self.leader_placement != line {
            if self.leader.is_none() && line.is_some() {
                self.leader = Some(
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("STATIC"),
                        None,
                        WS_CHILD | WS_DISABLED | WS_CLIPSIBLINGS | WINDOW_STYLE(SS_WHITERECT.0),
                        0,
                        0,
                        0,
                        0,
                        Some(parent),
                        None,
                        None,
                        None,
                    )
                    .map_err(|e| e.to_string())?,
                );
            }
            if let Some(leader) = self.leader {
                move_overlay(leader, line, true)?;
            }
            self.leader_placement = line;
        }
        Ok(())
    }
}

unsafe fn move_overlay(
    window: HWND,
    placement: Option<LabelRect>,
    behind_labels: bool,
) -> Result<()> {
    if let Some(r) = placement {
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOCOPYBITS;
        if !behind_labels {
            flags |= SWP_NOZORDER;
        }
        SetWindowPos(
            window,
            Some(HWND_BOTTOM),
            r.x,
            r.y,
            r.width,
            r.height,
            flags,
        )
        .map_err(|e| e.to_string())?;
        // Moving across another child can expose only fragments of the old
        // client area. Always repaint the full badge from its own text.
        let _ = InvalidateRect(Some(window), None, false);
    } else {
        let _ = ShowWindow(window, SW_HIDE);
    }
    Ok(())
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
                if let Some(leader) = badge.leader {
                    let _ = DestroyWindow(leader);
                }
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
        let viewport = (width as i32, height as i32);
        let mut occupied = Vec::with_capacity(count);
        for (index, origin) in origins.iter().enumerate() {
            let origin = serde_json::from_value(origin.clone()).map_err(|e| e.to_string())?;
            if index == self.badges.len() {
                self.badges.push(Badge {
                    window: unsafe { create_badge(parent, index + 1)? },
                    placement: None,
                    leader: None,
                    leader_placement: None,
                });
            }
            let badge = &mut self.badges[index];
            let anchor = screen_position(origin, eye, &matrix, width, height)
                .map(|(x, y)| (x.min(viewport.0 - 1), y.min(viewport.1 - 1)));
            let size = (
                unit(60.0 + 7.0 * (index + 1).to_string().len() as f64).max(1),
                unit(20.0).max(1),
            );
            let position = anchor.and_then(|point| {
                place_label(
                    point,
                    size,
                    viewport,
                    unit(9.0),
                    unit(2.0).max(1),
                    &occupied,
                )
            });
            if let Some(label) = position {
                occupied.push(label);
            }
            unsafe { badge.update(parent, position, anchor, unit(2.0).max(1), viewport.0)? };
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
        WS_CHILD | WS_DISABLED | WS_CLIPSIBLINGS,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_separate_as_twelve_lights_line_up_at_the_viewport_edges() {
        for scale in [1, 2] {
            let viewport = (640 * scale, 360 * scale);
            for anchor in [
                (0, 0),
                (viewport.0 / 2, viewport.1 / 2),
                (viewport.0 - 1, viewport.1 - 1),
            ] {
                let mut occupied = Vec::new();
                for number in 1..=12 {
                    let size = (if number < 10 { 67 } else { 74 } * scale, 20 * scale);
                    let label =
                        place_label(anchor, size, viewport, 9 * scale, 2 * scale, &occupied)
                            .unwrap();
                    assert!(label.x >= 0 && label.x + label.width <= viewport.0);
                    assert!(label.y >= 0 && label.y + label.height <= viewport.1);
                    assert!(occupied
                        .iter()
                        .all(|r| r.y + r.height < label.y || label.y + label.height < r.y));
                    occupied.push(label);
                }
            }
        }
    }

    #[test]
    fn isolated_labels_stay_on_their_lights_and_tiny_viewports_are_safe() {
        let size = (67, 20);
        let first = place_label((70, 80), size, (640, 360), 9, 2, &[]).unwrap();
        let next = place_label((250, 80), size, (640, 360), 9, 2, &[first]).unwrap();
        assert_eq!((first.x, first.y), (61, 71));
        assert_eq!((next.x, next.y), (241, 71));
        assert_eq!(place_label((0, 0), size, (1, 1), 9, 2, &[]), None);
    }
}
