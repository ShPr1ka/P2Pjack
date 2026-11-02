//! Заставка при запуске: по краю окна от середины верхней стороны в обе стороны медленно бежит
//! лаймовая рамка, в центре — знак P2PJack, между узлами которого летает пакет. Когда рамка
//! замыкается, она вспыхивает и заставка растворяется, открывая приложение.

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use p2pjack::ui_kit::*;
use std::time::Instant;

/// Рамка начинает рисоваться и замыкается (секунды от запуска).
const TRACE: (f32, f32) = (0.25, 2.15);
/// Вспышка замкнувшейся рамки.
const FLASH: f32 = 0.3;
/// Растворение заставки.
const FADE: f32 = 0.45;
/// Растворение, если заставку пропустили щелчком.
const SKIP_FADE: f32 = 0.25;

pub struct Splash {
    start: Instant,
    /// Остановленное время (`--splash-at мс`) — для скриншотов отдельных кадров.
    frozen: Option<f32>,
    skipped_at: Option<f32>,
}

impl Splash {
    pub fn new(frozen_ms: Option<u32>) -> Self {
        Self { start: Instant::now(), frozen: frozen_ms.map(|ms| ms as f32 / 1000.0), skipped_at: None }
    }

    fn time(&self) -> f32 {
        self.frozen.unwrap_or_else(|| self.start.elapsed().as_secs_f32())
    }

    /// Рисует заставку поверх всего. `false` — заставка закончилась.
    pub fn show(&mut self, ctx: &egui::Context, status: (&str, Color32)) -> bool {
        let t = self.time();
        let fade_start = TRACE.1 + FLASH;
        let alpha = match self.skipped_at {
            Some(s) => 1.0 - (t - s) / SKIP_FADE,
            None => 1.0 - ((t - fade_start) / FADE).max(0.0),
        };
        if alpha <= 0.0 {
            return false;
        }
        let alpha = ease_in_out(alpha.clamp(0.0, 1.0));
        ctx.request_repaint();

        let screen = ctx.content_rect();
        egui::Area::new(Id::unique("splash"))
            .order(egui::Order::Debug)
            .fixed_pos(screen.min)
            .constrain(false)
            .fade_in(false)
            .show(ctx, |ui| {
                // Перехватываем мышь, чтобы под заставкой ничего не нажималось; щелчок — пропустить.
                let resp = ui.allocate_rect(screen, Sense::click());
                if resp.clicked() && self.skipped_at.is_none() && self.frozen.is_none() {
                    self.skipped_at = Some(t);
                }
                let p = ui.painter();
                p.rect_filled(screen, CornerRadius::ZERO, col::BG.gamma_multiply(alpha));
                paint_frame(p, screen.shrink(12.0), t, alpha);
                paint_center(p, screen.center() - vec2(0.0, 18.0), t, alpha, status);
                text(
                    p,
                    screen.right_bottom() - vec2(26.0, 24.0),
                    Align2::RIGHT_BOTTOM,
                    concat!("v", env!("CARGO_PKG_VERSION")),
                    mono(10.5),
                    col::DIM.gamma_multiply(alpha),
                    80.0,
                );
            });
        true
    }
}

fn ease_in_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x < 0.5 { 4.0 * x * x * x } else { 1.0 - (-2.0 * x + 2.0).powi(3) / 2.0 }
}

fn ease_sine(x: f32) -> f32 {
    -((std::f32::consts::PI * x.clamp(0.0, 1.0)).cos() - 1.0) / 2.0
}

/// Правая половина периметра длиной `d`: от середины верха по часовой стрелке к середине низа.
fn half_path(r: Rect, d: f32) -> Vec<Pos2> {
    let segments = [
        (pos2(r.center().x, r.top()), r.right_top()),
        (r.right_top(), r.right_bottom()),
        (r.right_bottom(), pos2(r.center().x, r.bottom())),
    ];
    let mut pts = vec![segments[0].0];
    let mut left = d;
    for (a, b) in segments {
        let len = a.distance(b);
        if left >= len {
            pts.push(b);
            left -= len;
        } else {
            pts.push(a + (b - a) * (left / len));
            break;
        }
    }
    pts
}

fn mirror(r: Rect, pts: &[Pos2]) -> Vec<Pos2> {
    pts.iter().map(|p| pos2(2.0 * r.center().x - p.x, p.y)).collect()
}

/// Дорожка по краю окна и бегущая по ней с двух сторон рамка.
fn paint_frame(p: &egui::Painter, r: Rect, t: f32, alpha: f32) {
    let track = Stroke::new(1.0, col::LINE.gamma_multiply(alpha));
    p.add(egui::Shape::closed_line(vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()], track));

    let progress = ease_in_out((t - TRACE.0) / (TRACE.1 - TRACE.0));
    if progress <= 0.0 {
        return;
    }
    let half = r.width() + r.height();
    let right = half_path(r, half * progress);
    let left = mirror(r, &right);
    // Вспышка, когда рамка замкнулась.
    let flash = if t > TRACE.1 { 1.0 - ((t - TRACE.1) / FLASH).clamp(0.0, 1.0) } else { 0.0 };
    let glow = Stroke::new(6.0 + 6.0 * flash, col::ACCENT.gamma_multiply((0.12 + 0.25 * flash) * alpha));
    let line = Stroke::new(2.0, col::ACCENT.gamma_multiply(alpha));
    for path in [&right, &left] {
        p.add(egui::Shape::line(path.clone(), glow));
        p.add(egui::Shape::line(path.clone(), line));
    }
    // Светящиеся «головки» на концах, пока рамка не замкнулась.
    if progress < 1.0 {
        for head in [right.last(), left.last()].into_iter().flatten() {
            p.rect_filled(Rect::from_center_size(*head, Vec2::splat(12.0)), CornerRadius::ZERO, col::ACCENT.gamma_multiply(0.18 * alpha));
            p.rect_filled(Rect::from_center_size(*head, Vec2::splat(5.0)), CornerRadius::ZERO, Color32::WHITE.gamma_multiply(alpha));
        }
    }
}

/// Знак с летающим пакетом, уголки видоискателя, название и строка состояния.
fn paint_center(p: &egui::Painter, c: Pos2, t: f32, alpha: f32, status: (&str, Color32)) {
    let appear = ease_in_out(t / 0.5);
    let a = alpha * appear;
    let lift = (1.0 - appear) * 10.0;
    let c = c + vec2(0.0, lift);

    // Уголки видоискателя «дышат».
    let s = 54.0 + 3.0 * (t * 2.2).sin();
    let arm = 13.0;
    let bracket = Stroke::new(1.5, col::MUTED.gamma_multiply(a));
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let corner = c + vec2(sx * s, sy * s);
        p.line_segment([corner, corner - vec2(sx * arm, 0.0)], bracket);
        p.line_segment([corner, corner - vec2(0.0, sy * arm)], bracket);
    }

    // Узлы и линия между ними.
    let gap = 24.0;
    let node = 18.0;
    let (ln, rn) = (c - vec2(gap, 0.0), c + vec2(gap, 0.0));
    let (from, to) = (ln.x + node / 2.0, rn.x - node / 2.0);
    p.line_segment([pos2(from, c.y), pos2(to, c.y)], Stroke::new(2.0, col::ACCENT_DIM.gamma_multiply(a)));

    // Пакет летает туда-обратно; узел, в который он прилетел, вспыхивает.
    let phase = (t * 0.85) % 2.0;
    let forward = phase < 1.0;
    let k = ease_sine(if forward { phase } else { 2.0 - phase });
    let x = from + (to - from) * k;
    let dir = if forward { -1.0 } else { 1.0 };
    for i in 0..5 {
        let tail = x + dir * i as f32 * 2.5;
        if (from..=to).contains(&tail) {
            let fade = 1.0 - i as f32 / 5.0;
            p.rect_filled(
                Rect::from_center_size(pos2(tail, c.y), Vec2::splat(6.0 - i as f32 * 0.6)),
                CornerRadius::ZERO,
                col::ACCENT.gamma_multiply(fade * 0.7 * a),
            );
        }
    }
    p.rect_filled(Rect::from_center_size(pos2(x, c.y), Vec2::splat(6.0)), CornerRadius::ZERO, Color32::WHITE.gamma_multiply(a));
    for (n, near) in [(ln, 1.0 - k), (rn, k)] {
        let boost = ((near - 0.8) / 0.2).clamp(0.0, 1.0);
        if boost > 0.0 {
            p.rect_filled(
                Rect::from_center_size(n, Vec2::splat(node + 10.0 * boost)),
                CornerRadius::ZERO,
                col::ACCENT.gamma_multiply(0.22 * boost * a),
            );
        }
        p.rect_filled(Rect::from_center_size(n, Vec2::splat(node)), CornerRadius::ZERO, col::ACCENT.gamma_multiply(a));
    }

    // Название набирается по буквам.
    let word = "P2PJACK";
    let shown = (((t - 0.35) / 0.09).max(0.0) as usize).min(word.len());
    let mut job = egui::text::LayoutJob::default();
    let fmt = |color: Color32| egui::text::TextFormat {
        font_id: display(18.0),
        color,
        extra_letter_spacing: 6.0,
        ..Default::default()
    };
    job.append(&word[..shown], 0.0, fmt(col::TEXT.gamma_multiply(a)));
    job.append(&word[shown..], 0.0, fmt(Color32::TRANSPARENT));
    let galley = p.layout_job(job);
    p.galley(c + vec2(-galley.size().x / 2.0, s + 22.0), galley, col::TEXT);

    // Состояние запуска и проценты рамки.
    let pct = (ease_in_out((t - TRACE.0) / (TRACE.1 - TRACE.0)) * 100.0).round() as u32;
    let line = format!("{}   {:>3}%", status.0, pct);
    let mut job = egui::text::LayoutJob::default();
    job.append(&line, 0.0, egui::text::TextFormat::simple(mono(11.5), col::MUTED.gamma_multiply(a)));
    let galley = p.layout_job(job);
    let pos = c + vec2(-galley.size().x / 2.0, s + 54.0);
    p.galley(pos, galley, col::MUTED);
    p.rect_filled(
        Rect::from_center_size(pos - vec2(10.0, -7.0), Vec2::splat(5.0)),
        CornerRadius::ZERO,
        status.1.gamma_multiply(a),
    );
}
