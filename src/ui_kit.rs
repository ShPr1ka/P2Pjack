//! Общий стиль P2PJack для окна программы и установщика: чёрная приборная панель,
//! прямые углы, тонкие линии, Bahnschrift для подписей, Consolas для адресов, один акцентный цвет.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Painter, Pos2, Rect, RichText, Sense,
    Stroke, Ui, Vec2, pos2, vec2,
};
use std::sync::Arc;

pub mod col {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x06, 0x06, 0x06);
    pub const PANEL: Color32 = Color32::from_rgb(0x0d, 0x0d, 0x0d);
    pub const RAISED: Color32 = Color32::from_rgb(0x15, 0x15, 0x15);
    pub const HOVER: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1b);
    pub const LINE: Color32 = Color32::from_rgb(0x20, 0x20, 0x1f);
    pub const LINE2: Color32 = Color32::from_rgb(0x34, 0x34, 0x32);
    pub const TEXT: Color32 = Color32::from_rgb(0xec, 0xea, 0xe4);
    pub const MUTED: Color32 = Color32::from_rgb(0x8c, 0x8a, 0x83);
    pub const DIM: Color32 = Color32::from_rgb(0x55, 0x54, 0x50);
    pub const ACCENT: Color32 = Color32::from_rgb(0xc8, 0xff, 0x2e);
    pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x5d, 0x75, 0x1a);
    pub const AMBER: Color32 = Color32::from_rgb(0xff, 0xb0, 0x2e);
    pub const RED: Color32 = Color32::from_rgb(0xff, 0x4d, 0x3d);
}

/// Bahnschrift: заголовки, подписи, кнопки.
pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

pub fn body(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Подпись капсом с разрядкой.
pub fn caps(s: &str, size: f32, color: Color32) -> RichText {
    RichText::new(s.to_uppercase()).font(display(size)).extra_letter_spacing(1.4).color(color)
}

pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let load = |file: &str| std::fs::read(format!(r"C:\Windows\Fonts\{file}")).ok();
    let add = |fonts: &mut egui::FontDefinitions, key: &str, file: &str, family: FontFamily| {
        if let Some(data) = load(file) {
            fonts.font_data.insert(key.into(), Arc::new(egui::FontData::from_owned(data)));
            fonts.families.entry(family).or_default().insert(0, key.into());
        }
    };
    add(&mut fonts, "segoe", "segoeui.ttf", FontFamily::Proportional);
    add(&mut fonts, "consolas", "consola.ttf", FontFamily::Monospace);
    // Семейство «display» — Bahnschrift, при его отсутствии обычный шрифт.
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    fonts.families.insert(FontFamily::Name("display".into()), fallback);
    add(&mut fonts, "bahnschrift", "bahnschrift.ttf", FontFamily::Name("display".into()));
    ctx.set_fonts(fonts);
}

pub fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        let v = &mut s.visuals;
        v.dark_mode = true;
        v.panel_fill = col::BG;
        v.window_fill = col::PANEL;
        v.window_stroke = Stroke::new(1.0, col::LINE2);
        v.window_corner_radius = CornerRadius::same(2);
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.menu_corner_radius = CornerRadius::same(2);
        v.extreme_bg_color = Color32::BLACK;
        v.faint_bg_color = col::PANEL;
        v.selection.bg_fill = col::ACCENT_DIM;
        v.selection.stroke = Stroke::new(1.0, col::ACCENT);
        v.hyperlink_color = col::ACCENT;
        v.text_cursor.stroke = Stroke::new(2.0, col::ACCENT);
        let w = &mut v.widgets;
        for (state, fill, stroke) in [
            (&mut w.noninteractive, col::PANEL, col::LINE),
            (&mut w.inactive, col::PANEL, col::LINE2),
            (&mut w.hovered, col::HOVER, col::MUTED),
            (&mut w.active, col::RAISED, col::ACCENT),
            (&mut w.open, col::HOVER, col::LINE2),
        ] {
            state.corner_radius = CornerRadius::same(2);
            state.weak_bg_fill = fill;
            state.bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, stroke);
            state.expansion = 0.0;
        }
        w.noninteractive.fg_stroke = Stroke::new(1.0, col::TEXT);
        w.inactive.fg_stroke = Stroke::new(1.0, col::TEXT);
        w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.icon_width = 16.0;
        s.spacing.icon_width_inner = 8.0;
        use egui::TextStyle::*;
        s.text_styles.insert(Body, body(14.0));
        s.text_styles.insert(Button, body(14.0));
        s.text_styles.insert(Small, body(12.0));
        s.text_styles.insert(Heading, display(22.0));
        s.text_styles.insert(Monospace, mono(13.0));
    });
}

#[derive(Clone, Copy, PartialEq)]
pub enum IconState {
    On,
    Off,
    Error,
}

/// Знак P2PJack: два узла, соединённые линией, на чёрном квадрате.
pub fn mark_icon(n: usize, state: IconState) -> Vec<u8> {
    let color = match state {
        IconState::On => col::ACCENT,
        IconState::Off => Color32::from_rgb(0x9a, 0x98, 0x92),
        IconState::Error => col::RED,
    };
    let inside = |x: f32, y: f32| -> Option<Color32> {
        // Координаты в долях стороны значка.
        let (ax, ay) = ((x - 0.5).abs(), (y - 0.5).abs());
        let r = 0.16;
        let in_bg = ax <= 0.5 && ay <= 0.5 && {
            let (cx, cy) = ((ax - (0.5 - r)).max(0.0), (ay - (0.5 - r)).max(0.0));
            cx * cx + cy * cy <= r * r
        };
        if !in_bg {
            return None;
        }
        let node = |nx: f32| (x - nx).abs() <= 0.12 && (y - 0.5).abs() <= 0.12;
        let link = (0.3..=0.7).contains(&x) && (y - 0.5).abs() <= 0.035;
        Some(if node(0.27) || node(0.73) || link { color } else { Color32::from_rgb(0x0a, 0x0a, 0x0a) })
    };
    let mut rgba = vec![0u8; n * n * 4];
    let ss = 4;
    for py in 0..n {
        for px in 0..n {
            let (mut acc, mut cover) = ([0f32; 3], 0f32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) / n as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) / n as f32;
                    if let Some(c) = inside(x, y) {
                        acc[0] += c.r() as f32;
                        acc[1] += c.g() as f32;
                        acc[2] += c.b() as f32;
                        cover += 1.0;
                    }
                }
            }
            let i = (py * n + px) * 4;
            if cover > 0.0 {
                for k in 0..3 {
                    rgba[i + k] = (acc[k] / cover) as u8;
                }
                rgba[i + 3] = (cover / (ss * ss) as f32 * 255.0) as u8;
            }
        }
    }
    rgba
}

/// Прямоугольная кнопка: основная — залитая акцентом, обычная — контур.
pub fn action_button(ui: &mut Ui, label: &str, width: f32, primary: bool) -> egui::Response {
    let button = if primary {
        egui::Button::new(caps(label, 12.5, Color32::BLACK)).fill(col::ACCENT).stroke(Stroke::NONE)
    } else {
        egui::Button::new(caps(label, 12.5, col::TEXT)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, col::LINE2))
    };
    ui.add(button.corner_radius(CornerRadius::same(1)).min_size(vec2(width, 36.0)))
}

pub fn text_button(ui: &mut Ui, label: &str, color: Color32) -> egui::Response {
    let r = ui.add(egui::Label::new(caps(label, 11.0, color)).sense(Sense::click()));
    if r.hovered() {
        let y = r.rect.bottom() + 1.0;
        ui.painter().line_segment([pos2(r.rect.left(), y), pos2(r.rect.right(), y)], Stroke::new(1.0, col::MUTED));
    }
    r
}

/// Плашка-уведомление: цветная полоса слева, текст и необязательное действие.
pub fn notice(ui: &mut Ui, color: Color32, msg: &str, action: Option<&str>) -> bool {
    let mut clicked = false;
    let resp = Frame::new()
        .fill(col::PANEL)
        .inner_margin(Margin { left: 14, right: 12, top: 10, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(msg).size(13.0).color(col::TEXT));
            if let Some(a) = action {
                clicked = text_button(ui, a, color).clicked();
            }
        });
    let r = resp.response.rect;
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(3.0, r.height())), CornerRadius::ZERO, color);
    ui.add_space(8.0);
    clicked
}

/// Знак P2PJack в интерфейсе — тот же, что в трее.
pub fn paint_mark(p: &Painter, r: Rect, color: Color32) {
    let c = r.center();
    let s = r.width() * 0.26;
    p.rect_filled(Rect::from_center_size(c - vec2(r.width() * 0.27, 0.0), Vec2::splat(s)), CornerRadius::ZERO, color);
    p.rect_filled(Rect::from_center_size(c + vec2(r.width() * 0.27, 0.0), Vec2::splat(s)), CornerRadius::ZERO, color);
    p.line_segment([c - vec2(r.width() * 0.2, 0.0), c + vec2(r.width() * 0.2, 0.0)], Stroke::new(1.5, color));
}

/// Текст в одну строку с многоточием, если не влезает.
pub fn text(p: &Painter, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32, max_w: f32) {
    let job = egui::text::LayoutJob::single_section(s.to_owned(), egui::TextFormat::simple(font, color));
    text_job(p, pos, anchor, job, max_w);
}

pub fn text_job(p: &Painter, pos: Pos2, anchor: Align2, mut job: egui::text::LayoutJob, max_w: f32) {
    job.wrap.max_width = max_w.max(10.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = p.layout_job(job);
    let rect = anchor.anchor_size(pos, galley.size());
    p.galley(rect.min, galley, col::TEXT);
}

pub fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let [w, h] = img.size;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
    enc.write_header()?.write_image_data(&data)?;
    Ok(())
}

// ---------- своя рамка окна ----------
//
// Системная рамка Windows отключена (`with_decorations(false)`): заголовок, кнопки окна,
// перетаскивание и изменение размера рисуются здесь, в стиле приложения.

#[derive(Clone, Copy, PartialEq)]
enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

fn is_maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// Верхняя панель окна: знак и название слева, кнопки окна справа. Окно тащится за панель,
/// двойной щелчок разворачивает его (если `maximize`). В `extra` можно добавить элементы
/// слева от кнопок окна (раскладка справа налево).
pub fn title_bar(ui: &mut Ui, maximize: bool, extra: impl FnOnce(&mut Ui)) {
    egui::Panel::top("title_bar").exact_size(36.0).frame(Frame::new().fill(col::BG)).show(ui, |ui| {
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        let drag = ui.interact(rect, ui.scope_id().with("drag"), Sense::click_and_drag());
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if maximize && drag.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized(&ctx)));
        }

        let p = ui.painter();
        p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, col::LINE));
        let mark = Rect::from_center_size(pos2(rect.left() + 25.0, rect.center().y), Vec2::splat(18.0));
        paint_mark(p, mark, col::ACCENT);
        let mut job = egui::text::LayoutJob::default();
        job.append(
            "P2PJACK",
            0.0,
            egui::text::TextFormat { font_id: display(13.0), color: col::MUTED, extra_letter_spacing: 3.0, ..Default::default() },
        );
        text_job(p, pos2(rect.left() + 42.0, rect.center().y), Align2::LEFT_CENTER, job, 120.0);

        let mut kinds = vec![WindowButton::Close];
        if maximize {
            kinds.push(WindowButton::Maximize);
        }
        kinds.push(WindowButton::Minimize);
        let w = 44.0;
        let mut right = rect.right();
        for kind in kinds {
            let r = Rect::from_min_max(pos2(right - w, rect.top()), pos2(right, rect.bottom() - 1.0));
            right -= w;
            if window_button(ui, r, kind).clicked() {
                let cmd = match kind {
                    WindowButton::Minimize => egui::ViewportCommand::Minimized(true),
                    WindowButton::Maximize => egui::ViewportCommand::Maximized(!is_maximized(&ctx)),
                    WindowButton::Close => egui::ViewportCommand::Close,
                };
                ctx.send_viewport_cmd(cmd);
            }
        }
        let extra_rect = Rect::from_min_max(pos2(rect.left() + 160.0, rect.top()), pos2(right - 10.0, rect.bottom()));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(extra_rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
        extra(&mut child);
    });
}

fn window_button(ui: &mut Ui, r: Rect, kind: WindowButton) -> egui::Response {
    let resp = ui.interact(r, ui.scope_id().with(("window_button", kind as u8)), Sense::click());
    let hover = resp.hovered();
    let p = ui.painter();
    let (bg, fg) = match (kind, hover, resp.is_pointer_button_down_on()) {
        (WindowButton::Close, true, _) => (Some(col::RED), Color32::WHITE),
        (_, true, true) => (Some(col::RAISED), Color32::WHITE),
        (_, true, false) => (Some(col::HOVER), col::TEXT),
        _ => (None, col::MUTED),
    };
    if let Some(bg) = bg {
        p.rect_filled(r, CornerRadius::ZERO, bg);
    }
    let c = r.center();
    let s = Stroke::new(1.0, fg);
    match kind {
        WindowButton::Minimize => {
            p.line_segment([c + vec2(-5.0, 0.5), c + vec2(5.0, 0.5)], s);
        }
        WindowButton::Maximize if is_maximized(ui.ctx()) => {
            // «Восстановить»: два квадрата друг за другом.
            p.rect_stroke(Rect::from_center_size(c + vec2(-1.5, 1.5), Vec2::splat(8.0)), CornerRadius::ZERO, s, egui::StrokeKind::Inside);
            p.line_segment([c + vec2(-3.5, -4.5), c + vec2(4.5, -4.5)], s);
            p.line_segment([c + vec2(4.5, -4.5), c + vec2(4.5, 3.5)], s);
        }
        WindowButton::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, Vec2::splat(9.0)), CornerRadius::ZERO, s, egui::StrokeKind::Inside);
        }
        WindowButton::Close => {
            p.line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], s);
            p.line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], s);
        }
    }
    resp
}

/// Изменение размера за края окна (вызывать в конце кадра, чтобы курсор не перебили виджеты)
/// и тонкая обводка вокруг окна вместо системной рамки.
pub fn window_frame(ctx: &egui::Context, resizable: bool) {
    if is_maximized(ctx) {
        return;
    }
    let screen = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("window_border"))).rect_stroke(
        screen,
        CornerRadius::ZERO,
        Stroke::new(1.0, col::LINE2),
        egui::StrokeKind::Inside,
    );
    if !resizable {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let m = 5.0;
    let (l, r) = (pos.x - screen.left() < m, screen.right() - pos.x < m);
    let (t, b) = (pos.y - screen.top() < m, screen.bottom() - pos.y < m);
    use egui::{CursorIcon as C, ResizeDirection as D};
    let (dir, icon) = match (l, r, t, b) {
        (true, _, true, _) => (D::NorthWest, C::ResizeNwSe),
        (_, true, _, true) => (D::SouthEast, C::ResizeNwSe),
        (_, true, true, _) => (D::NorthEast, C::ResizeNeSw),
        (true, _, _, true) => (D::SouthWest, C::ResizeNeSw),
        (true, ..) => (D::West, C::ResizeHorizontal),
        (_, true, ..) => (D::East, C::ResizeHorizontal),
        (_, _, true, _) => (D::North, C::ResizeVertical),
        (_, _, _, true) => (D::South, C::ResizeVertical),
        _ => return,
    };
    ctx.set_cursor_icon(icon);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
    }
}
