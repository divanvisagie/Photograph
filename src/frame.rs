//! Photograph's own window frame (see ADR-0022): the window opens without
//! system decorations, and this draws a header bar holding the toolbar, the
//! window title and minimise/maximise/close buttons, plus a one-pixel border
//! and invisible resize handles along the edges. Everything is painted with
//! the active egui visuals, so the frame follows the desktop's light/dark
//! setting like the rest of the window. Ported from UnAmp's frame.
//!
//! Moving and resizing are handed to the compositor (`StartDrag`,
//! `BeginResize`), so snapping and tiling behave as with a native frame.

use egui::{Color32, CursorIcon, Id, Order, ResizeDirection, Sense, Stroke, ViewportCommand, vec2};

/// How far in from the window edge a press starts a resize.
const EDGE: f32 = 5.0;
/// Corners get a larger grab area than straight edges.
const CORNER: f32 = 12.0;
const BUTTON: egui::Vec2 = vec2(30.0, 22.0);

#[derive(Clone, Copy)]
enum Button {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        let v = i.viewport();
        v.maximized.unwrap_or(false) || v.fullscreen.unwrap_or(false)
    })
}

/// Corner rounding for the window right now: `radius`, or square while
/// maximised or full screen, as GNOME does.
pub fn corner_radius(ctx: &egui::Context, radius: u8) -> u8 {
    if maximized(ctx) { 0 } else { radius }
}

/// The header bar: `left` on the left, `title` centred, and with `custom`
/// set, window buttons on the far right and drag-to-move on the empty space.
/// `right` goes to the left of the window buttons, laid out right to left.
/// With `custom` off the desktop draws the frame and this is a plain toolbar.
pub fn title_bar(
    ui: &mut egui::Ui,
    title: &str,
    custom: bool,
    left: impl FnOnce(&mut egui::Ui),
    right: impl FnOnce(&mut egui::Ui),
) {
    let ctx = ui.ctx().clone();
    let bar = ui.max_rect();
    if custom {
        // Registered before the toolbar and buttons, so they win where they
        // overlap and only empty space moves the window.
        let resp = ui.interact(bar, Id::new("title_bar_drag"), Sense::click_and_drag());
        if resp.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if resp.double_clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized(&ctx)));
        }
        resp.context_menu(|ui| window_menu(ui, &ctx));
    }

    let mut left_width = 0.0;
    let mut right_width = 0.0;
    ui.horizontal(|ui| {
        left(ui);
        left_width = ui.cursor().left() - bar.left();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if custom {
                ui.spacing_mut().item_spacing.x = 4.0;
                if button(ui, Button::Close).on_hover_text("Close").clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                let max = maximized(&ctx);
                let (kind, tip) = if max { (Button::Restore, "Restore") } else { (Button::Maximize, "Maximise") };
                if button(ui, kind).on_hover_text(tip).clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
                }
                if button(ui, Button::Minimize).on_hover_text("Minimise").clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }
                ui.add_space(8.0);
                ui.spacing_mut().item_spacing.x = 8.0;
            }
            right(ui);
            right_width = bar.right() - ui.min_rect().left();
        });
    });

    // Centred on the window, not on the gap between the two sides, and
    // shortened with an ellipsis rather than running into either.
    let room = bar.width() - 2.0 * left_width.max(right_width) - 16.0;
    if custom && room > 40.0 {
        let galley = egui::WidgetText::from(egui::RichText::new(title).strong()).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            room,
            egui::TextStyle::Body,
        );
        let pos = bar.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, ui.visuals().text_color());
    }
}

fn window_menu(ui: &mut egui::Ui, ctx: &egui::Context) {
    if ui.button("Minimise").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
    let max = maximized(ctx);
    if ui.button(if max { "Restore" } else { "Maximise" }).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
    }
    ui.separator();
    if ui.button("Close").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
}

fn button(ui: &mut egui::Ui, kind: Button) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(BUTTON, Sense::click());
    let visuals = ui.visuals();
    let fg = if resp.hovered() || resp.has_focus() {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    let (fg, bg) = match kind {
        Button::Close if resp.hovered() => (Color32::WHITE, Some(Color32::from_rgb(0xc0, 0x1c, 0x28))),
        _ if resp.is_pointer_button_down_on() => (fg, Some(visuals.widgets.active.weak_bg_fill)),
        _ if resp.hovered() => (fg, Some(visuals.widgets.hovered.weak_bg_fill)),
        _ => (fg, None),
    };
    let painter = ui.painter();
    if let Some(bg) = bg {
        painter.rect_filled(rect, visuals.widgets.hovered.corner_radius, bg);
    }
    let stroke = Stroke::new(1.2, fg);
    let c = rect.center();
    let r = 4.5;
    match kind {
        Button::Minimize => {
            painter.line_segment([c + vec2(-r, r), c + vec2(r, r)], stroke);
        }
        Button::Maximize => {
            painter.rect_stroke(egui::Rect::from_center_size(c, vec2(2.0 * r, 2.0 * r)), 0.0, stroke, egui::StrokeKind::Middle);
        }
        Button::Restore => {
            let back = egui::Rect::from_center_size(c + vec2(1.5, -1.5), vec2(2.0 * r - 1.5, 2.0 * r - 1.5));
            let front = egui::Rect::from_center_size(c + vec2(-1.5, 1.5), vec2(2.0 * r - 1.5, 2.0 * r - 1.5));
            painter.line_segment([back.left_top(), back.right_top()], stroke);
            painter.line_segment([back.right_top(), back.right_bottom()], stroke);
            painter.line_segment([back.left_top(), egui::pos2(back.left(), front.top())], stroke);
            painter.line_segment([back.right_bottom(), egui::pos2(front.right(), back.bottom())], stroke);
            painter.rect_stroke(front, 0.0, stroke, egui::StrokeKind::Middle);
        }
        Button::Close => {
            painter.line_segment([c + vec2(-r, -r), c + vec2(r, r)], stroke);
            painter.line_segment([c + vec2(-r, r), c + vec2(r, -r)], stroke);
        }
    }
    resp
}

/// Which edge or corner of `window` `pos` is on, if any.
fn edge_at(window: egui::Rect, pos: egui::Pos2) -> Option<ResizeDirection> {
    if !window.contains(pos) {
        return None;
    }
    let near = |d: f32, zone: f32| d < zone;
    let (l, r) = (pos.x - window.left(), window.right() - pos.x);
    let (t, b) = (pos.y - window.top(), window.bottom() - pos.y);
    let corner = |a: f32, b: f32| near(a, CORNER) && near(b, CORNER) && (near(a, EDGE) || near(b, EDGE));
    Some(match () {
        _ if corner(t, l) => ResizeDirection::NorthWest,
        _ if corner(t, r) => ResizeDirection::NorthEast,
        _ if corner(b, l) => ResizeDirection::SouthWest,
        _ if corner(b, r) => ResizeDirection::SouthEast,
        _ if near(t, EDGE) => ResizeDirection::North,
        _ if near(b, EDGE) => ResizeDirection::South,
        _ if near(l, EDGE) => ResizeDirection::West,
        _ if near(r, EDGE) => ResizeDirection::East,
        _ => return None,
    })
}

fn cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

/// The border and resize handles. Call after everything else is drawn, so
/// the handles sit above the floating windows along the edges.
pub fn edges(ctx: &egui::Context, radius: u8) {
    if maximized(ctx) {
        return;
    }
    let window = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(Order::Foreground, Id::new("window_border")));
    let stroke = ctx.global_style().visuals.window_stroke;
    painter.rect_stroke(window, radius, Stroke::new(1.0, stroke.color), egui::StrokeKind::Inside);

    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let Some(direction) = edge_at(window, pos) else {
        return;
    };
    // An area over just the edge under the pointer, on top, so the press
    // goes to the resize and not to whatever is drawn beneath it.
    let grab = egui::Rect::from_center_size(pos, vec2(2.0 * EDGE, 2.0 * EDGE)).intersect(window);
    egui::Area::new(Id::new("window_resize"))
        .order(Order::Foreground)
        .fixed_pos(grab.min)
        .constrain(false)
        .show(ctx, |ui| {
            let (_, resp) = ui.allocate_exact_size(grab.size(), Sense::drag());
            let resp = resp.on_hover_cursor(cursor(direction));
            if resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0))
    }

    #[test]
    fn edges_and_corners_resize_the_right_way() {
        let w = window();
        assert_eq!(edge_at(w, egui::pos2(400.0, 2.0)), Some(ResizeDirection::North));
        assert_eq!(edge_at(w, egui::pos2(400.0, 598.0)), Some(ResizeDirection::South));
        assert_eq!(edge_at(w, egui::pos2(1.0, 300.0)), Some(ResizeDirection::West));
        assert_eq!(edge_at(w, egui::pos2(799.0, 300.0)), Some(ResizeDirection::East));
        assert_eq!(edge_at(w, egui::pos2(2.0, 10.0)), Some(ResizeDirection::NorthWest));
        assert_eq!(edge_at(w, egui::pos2(790.0, 598.0)), Some(ResizeDirection::SouthEast));
    }

    #[test]
    fn the_inside_and_the_outside_do_not_resize() {
        let w = window();
        assert_eq!(edge_at(w, egui::pos2(400.0, 300.0)), None);
        assert_eq!(edge_at(w, egui::pos2(10.0, 10.0)), None);
        assert_eq!(edge_at(w, egui::pos2(-1.0, 300.0)), None);
    }
}
