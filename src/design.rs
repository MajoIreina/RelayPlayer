use egui::{
    self, pos2, vec2, Align2, Color32 as C, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
    WidgetType,
};
use std::{fs, path::PathBuf, sync::Arc};

pub const BG: C = C::from_rgb(16, 18, 25);
pub const BAR: C = C::from_rgb(32, 34, 45);
pub const SIDE: C = C::from_rgb(30, 32, 42);
pub const TILE: C = C::from_rgb(47, 50, 65);
pub const LINE: C = C::from_rgb(59, 62, 80);
pub const TEXT: C = C::from_rgb(239, 240, 250);
pub const MUTED: C = C::from_rgb(177, 181, 203);
pub const DIM: C = C::from_rgb(143, 149, 175);
pub const ACCENT: C = C::from_rgb(172, 164, 255);
pub const INK: C = C::from_rgb(34, 28, 63);
pub const WARN: C = C::from_rgb(255, 174, 146);

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}
pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Interface semibold".into()))
}
pub fn setup(ctx: &egui::Context) {
    let fontsdir = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:/Windows".into()))
        .join("Fonts");
    let mut fonts = FontDefinitions::default();
    for (name, candidates) in [
        ("Segoe", &["segoeui.ttf"][..]),
        ("Segoe Semibold", &["seguisb.ttf", "segoeuib.ttf"][..]),
        ("CJK UI", &["msyh.ttc", "Deng.ttf", "simsun.ttc"][..]),
        ("CJK Semibold", &["msyhbd.ttc", "Dengb.ttf"][..]),
    ] {
        if let Some(data) = candidates
            .iter()
            .find_map(|f| fs::read(fontsdir.join(f)).ok())
        {
            fonts
                .font_data
                .insert(name.into(), Arc::new(FontData::from_owned(data)));
        }
    }
    let orig = fonts.families[&FontFamily::Proportional].clone();
    let make = |names: &[&str]| {
        names
            .iter()
            .filter(|n| fonts.font_data.contains_key(**n))
            .map(|s| s.to_string())
            .chain(orig.iter().cloned())
            .collect::<Vec<_>>()
    };
    let reg = make(&["Segoe", "CJK UI"]);
    let bold = make(&["Segoe Semibold", "CJK Semibold", "Segoe", "CJK UI"]);
    fonts.families.insert(FontFamily::Proportional, reg.clone());
    fonts
        .families
        .insert(FontFamily::Name("Interface semibold".into()), bold);
    if let Some(m) = fonts.families.get_mut(&FontFamily::Monospace) {
        m.extend(reg);
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut style = (*ctx.style()).clone();
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = SIDE;
    v.window_fill = C::from_rgb(39, 41, 55);
    v.window_stroke = Stroke::new(1.0_f32, LINE);
    v.extreme_bg_color = C::from_rgb(23, 25, 33);
    v.faint_bg_color = C::from_rgb(35, 37, 49);
    v.code_bg_color = BG;
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = C::from_rgb(75, 66, 119);
    v.selection.stroke = Stroke::new(1.2_f32, ACCENT);
    v.warn_fg_color = WARN;
    v.error_fg_color = C::from_rgb(255, 155, 166);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_corner_radius = CornerRadius::same(10);
    for (w, bg, fg) in [
        (&mut v.widgets.noninteractive, SIDE, MUTED),
        (&mut v.widgets.inactive, TILE, TEXT),
        (&mut v.widgets.hovered, C::from_rgb(62, 62, 85), TEXT),
        (&mut v.widgets.active, C::from_rgb(72, 65, 99), TEXT),
        (&mut v.widgets.open, C::from_rgb(63, 58, 87), TEXT),
    ] {
        w.bg_fill = bg;
        w.weak_bg_fill = bg;
        w.fg_stroke = Stroke::new(1.35_f32, fg);
        w.bg_stroke = Stroke::new(1.0_f32, LINE);
        w.corner_radius = CornerRadius::same(6);
        w.expansion = 0.;
    }
    style.visuals = v;
    style.spacing.item_spacing = vec2(8., 8.);
    style.spacing.button_padding = vec2(11., 6.);
    style.spacing.interact_size = vec2(38., 29.);
    style.spacing.slider_width = 118.;
    style.spacing.combo_width = 130.;
    style.text_styles.insert(egui::TextStyle::Body, font(14.));
    style.text_styles.insert(egui::TextStyle::Button, font(14.));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, strong(21.));
    style.text_styles.insert(egui::TextStyle::Small, font(12.));
    ctx.set_style(style);
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Icon {
    Monitor,
    Capture,
    Picture,
    Gamepad,
    Activity,
    Pin,
    Unpin,
    Speaker,
    Mute,
    Full,
    Restore,
    Play,
    Stop,
    Spark,
    Close,
    Retry,
}
pub fn glyph(p: &Painter, kind: Icon, center: Pos2, size: f32, color: C) {
    let s = size / 24.;
    let o = center - vec2(size / 2., size / 2.);
    let v = |x: f32, y: f32| o + vec2(x * s, y * s);
    let r = |x, y, x2, y2| Rect::from_min_max(v(x, y), v(x2, y2));
    let st = Stroke::new((s * 1.65).max(1.), color);
    let path = |pts: &[(f32, f32)]| {
        p.add(Shape::line(
            pts.iter().map(|(x, y)| v(*x, *y)).collect(),
            st,
        ))
    };
    let circ = |x, y, rad| p.circle_stroke(v(x, y), rad * s, st);
    match kind {
        Icon::Monitor => {
            p.rect_stroke(r(2.5, 4.5, 21.5, 17.5), 2, st, StrokeKind::Inside);
            path(&[(12., 17.5), (12., 20.)]);
            path(&[(7.5, 20.5), (16.5, 20.5)]);
        }
        Icon::Capture => {
            p.rect_stroke(r(2.5, 6., 17., 18.), 3, st, StrokeKind::Inside);
            path(&[(17., 10.), (21.5, 7.5), (21.5, 16.5), (17., 14.)]);
            p.circle_filled(v(8., 12.), 1.6 * s, color);
        }
        Icon::Picture => {
            for (y, x) in [(5., 8.), (12., 16.), (19., 10.)] {
                path(&[(3., y), (x - 2.2, y)]);
                path(&[(x + 2.2, y), (21., y)]);
                circ(x, y, 2.1);
            }
        }
        Icon::Gamepad => {
            path(&[
                (5., 8.),
                (7., 6.5),
                (16.5, 6.5),
                (19., 8.),
                (20.5, 13.),
                (21., 16.5),
                (20., 18.4),
                (18., 18.2),
                (15., 15.5),
                (9., 15.5),
                (6., 18.2),
                (3.8, 18.4),
                (3., 16.4),
                (3.6, 12.),
                (5., 8.),
            ]);
            path(&[(6., 11.), (10., 11.)]);
            path(&[(8., 9.), (8., 13.)]);
            p.circle_filled(v(15., 11.7), 1. * s, color);
            p.circle_filled(v(17.4, 10.), 1. * s, color);
        }
        Icon::Activity => {
            path(&[
                (2., 13.),
                (6., 13.),
                (9., 6.),
                (13., 18.),
                (16., 10.),
                (18., 13.),
                (22., 13.),
            ]);
        }
        Icon::Pin | Icon::Unpin => {
            path(&[
                (7., 4.),
                (17., 4.),
                (15.6, 6.6),
                (15.6, 10.),
                (18.5, 13.5),
                (5.5, 13.5),
                (8.4, 10.),
                (8.4, 6.6),
                (7., 4.),
            ]);
            path(&[(12., 14.), (12., 20.5)]);
            if kind == Icon::Unpin {
                path(&[(3., 3.), (21., 21.)]);
            }
        }
        Icon::Speaker | Icon::Mute => {
            path(&[
                (2.5, 9.4),
                (6.5, 9.4),
                (11.5, 5.),
                (11.5, 19.),
                (6.5, 14.6),
                (2.5, 14.6),
                (2.5, 9.4),
            ]);
            if kind == Icon::Speaker {
                path(&[
                    (15., 8.6),
                    (16.4, 10.3),
                    (16.8, 12.),
                    (16.4, 13.7),
                    (15., 15.4),
                ]);
                path(&[
                    (18.2, 6.),
                    (20.1, 8.4),
                    (20.9, 12.),
                    (20.1, 15.6),
                    (18.2, 18.),
                ]);
            } else {
                path(&[(15., 9.), (21., 15.)]);
                path(&[(21., 9.), (15., 15.)]);
            }
        }
        Icon::Full | Icon::Restore => {
            for (x, y, dx, dy) in [
                (4., 4., 1., 1.),
                (20., 4., -1., 1.),
                (4., 20., 1., -1.),
                (20., 20., -1., -1.),
            ] {
                if kind == Icon::Full {
                    path(&[(x + 5. * dx, y), (x, y), (x, y + 5. * dy)]);
                } else {
                    path(&[
                        (x, y + 5. * dy),
                        (x + 5. * dx, y + 5. * dy),
                        (x + 5. * dx, y),
                    ]);
                }
            }
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(
                vec![v(7., 4.), v(20., 12.), v(7., 20.)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Stop => {
            p.rect_filled(r(5., 5., 19., 19.), 3, color);
        }
        Icon::Spark => {
            path(&[
                (10., 3.),
                (11.8, 9.),
                (17.5, 11.),
                (11.8, 13.),
                (10., 19.),
                (8.2, 13.),
                (2.5, 11.),
                (8.2, 9.),
                (10., 3.),
            ]);
            path(&[
                (18., 13.8),
                (19., 17.),
                (22., 18.),
                (19., 19.),
                (18., 22.),
                (17., 19.),
                (14., 18.),
                (17., 17.),
                (18., 13.8),
            ]);
        }
        Icon::Close => {
            path(&[(6., 6.), (18., 18.)]);
            path(&[(18., 6.), (6., 18.)]);
        }
        Icon::Retry => {
            path(&[
                (20., 9.),
                (17., 6.),
                (13., 4.8),
                (9., 5.2),
                (6., 7.),
                (4.3, 10.),
                (4.2, 14.),
                (6., 17.),
                (9., 19.),
                (13., 19.4),
                (16.5, 18.),
                (18.5, 16.),
            ]);
            path(&[(20., 4.), (20., 9.5), (14.5, 9.5)]);
        }
    }
}

#[derive(Copy, Clone)]
pub enum ButtonStyle {
    Ghost,
    Selected,
    Secondary,
    Primary,
}
pub fn command(
    ui: &mut Ui,
    id: &str,
    rect: Rect,
    icon: Icon,
    label: &str,
    accessible: &str,
    style: ButtonStyle,
    enabled: bool,
) -> Response {
    let mut res = ui.interact(
        rect,
        ui.make_persistent_id(id),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    res.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, accessible));
    let p = ui.painter();
    let hover = res.hovered() && enabled;
    let focus = res.has_focus();
    let (fill, fg, outline) = match style {
        ButtonStyle::Primary => (
            if hover {
                C::from_rgb(192, 185, 255)
            } else {
                ACCENT
            },
            INK,
            C::TRANSPARENT,
        ),
        ButtonStyle::Selected => (
            if hover {
                C::from_rgb(64, 61, 86)
            } else {
                C::from_rgb(52, 49, 74)
            },
            ACCENT,
            C::from_rgb(77, 72, 101),
        ),
        ButtonStyle::Secondary => (
            if hover {
                C::from_rgb(53, 56, 72)
            } else {
                C::from_rgb(37, 40, 53)
            },
            TEXT,
            LINE,
        ),
        ButtonStyle::Ghost => (
            if hover { TILE } else { C::TRANSPARENT },
            if enabled { MUTED } else { DIM },
            C::TRANSPARENT,
        ),
    };
    p.rect(
        rect,
        7,
        fill,
        Stroke::new(
            if focus { 1.6_f32 } else { 1.0_f32 },
            if focus { ACCENT } else { outline },
        ),
        StrokeKind::Inside,
    );
    if label.is_empty() {
        glyph(p, icon, rect.center(), 19., fg);
    } else {
        let gal = p.layout_no_wrap(label.into(), font(14.), fg);
        let x = rect.center().x - (18. + 7. + gal.size().x) / 2.;
        glyph(p, icon, pos2(x + 9., rect.center().y), 18., fg);
        p.galley(pos2(x + 25., rect.center().y - gal.size().y / 2.), gal, fg);
    }
    if hover {
        res = res.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    res
}
pub fn label(p: &Painter, pos: Pos2, align: Align2, txt: impl ToString, id: FontId, c: C) {
    p.text(pos, align, txt, id, c);
}
pub fn badge(p: &Painter, rect: Rect, text: &str, c: C) {
    p.rect(
        rect,
        12,
        C::from_rgb(28, 30, 42),
        Stroke::new(1.0_f32, C::from_rgb(54, 57, 74)),
        StrokeKind::Inside,
    );
    p.circle_filled(pos2(rect.left() + 13., rect.center().y), 3., c);
    label(
        p,
        pos2(rect.left() + 23., rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font(12.),
        c,
    );
}
pub fn brand(p: &Painter, c: Pos2) {
    let r = Rect::from_center_size(c, vec2(32., 32.));
    p.rect_filled(r, 8, C::from_rgb(132, 117, 244));
    p.rect_stroke(
        r.shrink(6.5),
        4,
        Stroke::new(1.6_f32, C::WHITE),
        StrokeKind::Inside,
    );
    p.add(Shape::convex_polygon(
        vec![c + vec2(-2., -4.), c + vec2(4., 0.), c + vec2(-2., 4.)],
        C::WHITE,
        Stroke::NONE,
    ));
}
// Illustration belongs only to the waiting state. A live session reserves a native video surface here.
pub fn idle_art(p: &Painter, c: Pos2, k: f32) {
    let q = |x: f32, y: f32| c + vec2(x * k, y * k);
    let sc = |a: u8, b: u8, c: u8| C::from_rgb(a, b, c);
    let disk = |x, y, rad, color| p.circle_filled(q(x, y), rad * k, color);
    // A quiet grid of points keeps the source mark legible against the unlit screen.
    for row in -4_i32..=4 {
        for col in -8_i32..=8 {
            let d = (col.abs() as f32 / 8.).max(row.abs() as f32 / 4.);
            if d > 0.6 {
                let alpha = (48. - 19. * d) as u8;
                disk(
                    col as f32 * 13.,
                    row as f32 * 13.,
                    0.9,
                    C::from_rgba_unmultiplied(158, 158, 221, alpha),
                );
            }
        }
    }
    let plate = |dx: f32, dy: f32, a: f32, b: f32, fill: C, line: C| {
        let r = Rect::from_center_size(q(dx, dy), vec2(a * k, b * k));
        p.rect(
            r,
            (9. * k) as u8,
            fill,
            Stroke::new(1.3 * k, line),
            StrokeKind::Inside,
        );
    };
    plate(-11., -8., 91., 57., sc(27, 29, 45), sc(67, 68, 96));
    plate(-5., -3., 91., 57., sc(36, 37, 57), sc(90, 88, 133));
    plate(3., 4., 91., 57., sc(42, 42, 66), sc(155, 148, 218));
    p.rect_filled(
        Rect::from_center_size(q(3., 4.), vec2(76. * k, 43. * k)),
        5,
        sc(30, 31, 48),
    );
    p.add(Shape::convex_polygon(
        vec![q(-3., -6.), q(13., 4.), q(-3., 14.)],
        ACCENT,
        Stroke::NONE,
    ));
    p.line_segment(
        [q(-10., 38.), q(16., 38.)],
        Stroke::new(2. * k, sc(123, 118, 174)),
    );
}
pub fn elem(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(pos2(x, y), Vec2::new(w, h))
}
