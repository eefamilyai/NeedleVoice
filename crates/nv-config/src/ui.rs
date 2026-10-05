//! The look of the settings app: palette, spacing, type scale, and the handful
//! of widgets every page is built from.
//!
//! Pages should not set colours or sizes themselves — they call these. That is
//! what keeps nine tabs looking like one application, and it means the whole
//! look can be changed here instead of in two thousand lines of layout code.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Id, Margin, Response, RichText, Sense, Stroke, StrokeKind,
    TextStyle, Ui, Vec2,
};

// ── palette ──────────────────────────────────────────────────────────────

/// Every colour in the app, for one appearance mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub accent: Color32,
    /// Text and icons drawn *on* the accent.
    pub on_accent: Color32,
    /// Window background.
    pub bg: Color32,
    /// The navigation column.
    pub sidebar: Color32,
    /// Cards and popups.
    pub surface: Color32,
    /// A second surface inside a card (inputs, segmented controls).
    pub inset: Color32,
    /// Hover state for rows and ghost buttons.
    pub hover: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub danger: Color32,
    pub warn: Color32,
    pub ok: Color32,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::new(true, Color32::from_rgb(0xB6, 0xFF, 0x2E))
    }
}

impl Theme {
    pub fn new(dark: bool, accent: Color32) -> Theme {
        let on_accent = if luminance(accent) > 0.55 { Color32::from_rgb(12, 14, 18) } else { Color32::WHITE };
        if dark {
            Theme {
                dark,
                accent,
                on_accent,
                bg: Color32::from_rgb(9, 11, 15),
                sidebar: Color32::from_rgb(12, 15, 20),
                surface: Color32::from_rgb(17, 20, 27),
                inset: Color32::from_rgb(11, 13, 18),
                hover: Color32::from_rgb(24, 28, 37),
                border: Color32::from_rgb(31, 36, 46),
                border_strong: Color32::from_rgb(44, 51, 64),
                text: Color32::from_rgb(233, 237, 244),
                muted: Color32::from_rgb(150, 159, 176),
                faint: Color32::from_rgb(104, 113, 130),
                danger: Color32::from_rgb(255, 107, 117),
                warn: Color32::from_rgb(255, 184, 77),
                ok: Color32::from_rgb(87, 217, 163),
            }
        } else {
            Theme {
                dark,
                accent,
                on_accent,
                bg: Color32::from_rgb(244, 246, 249),
                sidebar: Color32::from_rgb(252, 253, 255),
                surface: Color32::WHITE,
                inset: Color32::from_rgb(243, 245, 249),
                hover: Color32::from_rgb(238, 241, 246),
                border: Color32::from_rgb(226, 230, 237),
                border_strong: Color32::from_rgb(206, 212, 222),
                text: Color32::from_rgb(22, 25, 31),
                muted: Color32::from_rgb(94, 103, 117),
                faint: Color32::from_rgb(139, 147, 160),
                danger: Color32::from_rgb(206, 47, 61),
                warn: Color32::from_rgb(168, 106, 0),
                ok: Color32::from_rgb(16, 138, 96),
            }
        }
    }

    /// The accent at low alpha, for selected backgrounds and glows.
    pub fn accent_soft(&self, alpha: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(self.accent.r(), self.accent.g(), self.accent.b(), alpha)
    }

    /// A one-pixel separator.
    pub fn hairline(&self) -> Stroke {
        Stroke::new(1.0, self.border)
    }

    /// Used when a colour is needed over the accent, e.g. a disabled primary.
    pub fn primary_fill(&self, enabled: bool) -> Color32 {
        if enabled {
            self.accent
        } else if self.dark {
            self.accent.linear_multiply(0.35)
        } else {
            self.accent.linear_multiply(0.55)
        }
    }
}

fn luminance(c: Color32) -> f32 {
    (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
}

/// The theme for this frame, put there by [`set`].
pub fn theme(ui: &Ui) -> Theme {
    ui.ctx()
        .data(|d| d.get_temp::<Theme>(Id::new("nv-theme")))
        .unwrap_or_default()
}

pub fn set(ctx: &egui::Context, theme: Theme) {
    ctx.data_mut(|d| d.insert_temp(Id::new("nv-theme"), theme));
}

// ── fonts ────────────────────────────────────────────────────────────────

/// True once an icon font was found on this machine.
static HAS_ICONS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn has_icons() -> bool {
    HAS_ICONS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Segoe UI for text, Segoe Fluent Icons (or MDL2) for glyphs, with the Windows
/// symbol and emoji fonts behind them for anything neither covers.
pub fn install_fonts(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let mut fonts = egui::FontDefinitions::default();
    let mut proportional: Vec<String> = Vec::new();
    let mut icons: Vec<String> = Vec::new();

    let mut load = |name: &str, file: &str, family: &mut Vec<String>, front: bool| -> bool {
        let Ok(bytes) = std::fs::read(format!(r"{windir}\Fonts\{file}")) else { return false };
        fonts.font_data.insert(name.into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        if front {
            family.insert(0, name.into());
        } else {
            family.push(name.into());
        }
        true
    };

    load("segoe", "segoeui.ttf", &mut proportional, true);
    load("segoe-symbol", "seguisym.ttf", &mut proportional, false);
    load("segoe-emoji", "seguiemj.ttf", &mut proportional, false);

    // Windows 11 ships Segoe Fluent Icons; Windows 10 has MDL2 Assets. The
    // glyphs used here exist in both.
    if load("icons", "SegoeIcons.ttf", &mut icons, true) || load("icons", "segmdl2.ttf", &mut icons, true) {
        HAS_ICONS.store(true, std::sync::atomic::Ordering::Relaxed);
        fonts.families.insert(FontFamily::Name("icons".into()), icons);
    }

    if !proportional.is_empty() {
        fonts.families.insert(FontFamily::Proportional, proportional);
    }
    ctx.set_fonts(fonts);
}

// ── the look of stock widgets ────────────────────────────────────────────

/// Wire the palette into egui itself, so combo boxes, sliders, text fields and
/// scroll bars match the cards they sit in.
pub fn style(ctx: &egui::Context, t: &Theme) {
    let mut v = if t.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = t.bg;
    v.window_fill = t.surface;
    v.window_stroke = Stroke::new(1.0, t.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = t.inset;
    v.faint_bg_color = if t.dark { Color32::from_rgb(22, 26, 34) } else { Color32::from_rgb(238, 241, 246) };
    v.code_bg_color = t.inset;
    v.selection.bg_fill = t.accent_soft(60);
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.hyperlink_color = t.accent;
    v.slider_trailing_fill = true;

    let radius = CornerRadius::same(8);
    let widget = |w: &mut egui::style::WidgetVisuals, fill: Color32, stroke: Stroke, text: Color32| {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = stroke;
        w.fg_stroke = Stroke::new(1.0, text);
        w.corner_radius = radius;
        w.expansion = 0.0;
    };
    widget(&mut v.widgets.noninteractive, Color32::TRANSPARENT, Stroke::NONE, t.text);
    widget(&mut v.widgets.inactive, t.inset, Stroke::new(1.0, t.border), t.text);
    widget(&mut v.widgets.hovered, t.hover, Stroke::new(1.0, t.border_strong), t.text);
    widget(&mut v.widgets.active, t.hover, Stroke::new(1.0, t.accent), t.text);
    widget(&mut v.widgets.open, t.inset, Stroke::new(1.0, t.border_strong), t.text);

    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.slider_width = 200.0;
        s.spacing.combo_width = 160.0;
        s.spacing.indent = 18.0;
        s.spacing.scroll.bar_width = 10.0;
        s.spacing.scroll.floating = true;
        s.spacing.window_margin = Margin::same(10);
        s.spacing.menu_margin = Margin::same(6);
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(17.0)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
    });
}

// ── page furniture ───────────────────────────────────────────────────────

/// The title block at the top of a page.
pub fn page(ui: &mut Ui, title: &str, subtitle: &str, actions: impl FnOnce(&mut Ui)) {
    let t = theme(ui);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(21.0).strong().color(t.text));
            if !subtitle.is_empty() {
                ui.add_space(2.0);
                ui.label(RichText::new(subtitle).size(12.5).color(t.muted));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
    });
    ui.add_space(14.0);
}

/// A card: an optional heading, then content.
pub fn card(ui: &mut Ui, title: &str, subtitle: &str, body: impl FnOnce(&mut Ui)) {
    let t = theme(ui);
    egui::Frame::new()
        .fill(t.surface)
        .stroke(t.hairline())
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).size(14.5).strong().color(t.text));
                if !subtitle.is_empty() {
                    ui.add_space(3.0);
                    ui.label(RichText::new(subtitle).size(12.0).color(t.muted));
                }
                ui.add_space(12.0);
            }
            body(ui);
        });
    ui.add_space(12.0);
}

/// A card whose content is a list of rows, drawn edge to edge with separators.
pub fn card_rows(ui: &mut Ui, title: &str, subtitle: &str, rows: impl FnOnce(&mut Ui)) {
    let t = theme(ui);
    egui::Frame::new()
        .fill(t.surface)
        .stroke(t.hairline())
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::symmetric(4, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(title).size(14.5).strong().color(t.text));
                        if !subtitle.is_empty() {
                            ui.label(RichText::new(subtitle).size(12.0).color(t.muted));
                        }
                    });
                });
                ui.add_space(10.0);
                divider(ui);
            }
            rows(ui);
        });
    ui.add_space(12.0);
}

/// Width of the label column in [`row`]: fixed, so every control in a card
/// starts at the same x however long the wording is.
const LABEL_COLUMN: f32 = 250.0;

/// One row inside [`card_rows`]: a label on the left, a control on the right.
pub fn row(ui: &mut Ui, label: &str, hint: &str, control: impl FnOnce(&mut Ui)) {
    let t = theme(ui);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.set_width(LABEL_COLUMN);
            ui.set_max_width(LABEL_COLUMN);
            ui.label(RichText::new(label).size(13.5).color(t.text));
            if !hint.is_empty() {
                ui.add(egui::Label::new(RichText::new(hint).size(11.5).color(t.faint)).wrap());
            }
        });
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                control(ui);
            });
        });
    });
    ui.add_space(10.0);
}

/// Two rows stacked: a label, then the control underneath at full width.
pub fn field(ui: &mut Ui, label: &str, hint: &str, control: impl FnOnce(&mut Ui)) {
    let t = theme(ui);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(label).size(13.0).color(t.text));
            if !hint.is_empty() {
                ui.label(RichText::new(hint).size(11.5).color(t.faint));
            }
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        control(ui);
    });
    ui.add_space(10.0);
}

pub fn hint(ui: &mut Ui, text: &str) {
    let t = theme(ui);
    ui.label(RichText::new(text).size(12.0).color(t.muted));
}

pub fn divider(ui: &mut Ui) {
    let t = theme(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, t.border);
    ui.add_space(4.0);
}

/// A pill: small, coloured, non-interactive.
pub fn pill(ui: &mut Ui, text: &str, colour: Color32) -> Response {
    let t = theme(ui);
    let font = FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, colour);
    let size = Vec2::new(galley.size().x + 16.0, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(10), t.inset);
        ui.painter().rect_stroke(rect, CornerRadius::same(10), Stroke::new(1.0, t.border), StrokeKind::Inside);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, colour);
    }
    response
}

/// A dot, for status.
pub fn dot(ui: &mut Ui, colour: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size + 4.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, colour);
}

/// An icon, sized and coloured.
pub fn glyph(ui: &mut Ui, glyph: &str, size: f32, colour: Color32) {
    if !has_icons() {
        return;
    }
    ui.label(
        RichText::new(glyph)
            .font(FontId::new(size, FontFamily::Name("icons".into())))
            .color(colour),
    );
}

/// An icon in a rounded square, for cards and empty states.
#[allow(dead_code)]
pub fn glyph_tile(ui: &mut Ui, glyph: &str, colour: Color32, size: f32) {
    let t = theme(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same((size / 4.0) as u8), t.inset);
        if has_icons() {
            let galley = ui
                .painter()
                .layout_no_wrap(glyph.to_string(), FontId::new(size * 0.5, FontFamily::Name("icons".into())), colour);
            ui.painter().galley(rect.center() - galley.size() / 2.0, galley, colour);
        }
    }
}

/// An empty state: icon, headline, one line of explanation.
///
/// Nothing draws one today — every page has content to show — but it is the
/// shape the next "you have not set this up yet" page wants, and the test below
/// keeps it working.
#[allow(dead_code)]
pub fn empty(ui: &mut Ui, glyph_text: &str, title: &str, body: &str) {
    let t = theme(ui);
    ui.vertical_centered(|ui| {
        ui.add_space(6.0);
        if has_icons() {
            glyph_tile(ui, glyph_text, t.faint, 44.0);
            ui.add_space(8.0);
        }
        ui.label(RichText::new(title).size(14.0).color(t.text));
        ui.add_space(2.0);
        ui.label(RichText::new(body).size(12.0).color(t.muted));
        ui.add_space(6.0);
    });
}

// ── controls ─────────────────────────────────────────────────────────────

/// A toggle switch.
pub fn switch(ui: &mut Ui, on: &mut bool, label: impl Into<String>) -> Response {
    let label = label.into();
    let t = theme(ui);
    ui.horizontal(|ui| {
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(38.0, 21.0), Sense::click());
        let knob = ui.ctx().animate_bool(response.id, *on);
        if response.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        if ui.is_rect_visible(rect) {
            let track = if *on { t.accent } else { t.border_strong };
            ui.painter().rect_filled(rect, CornerRadius::same(11), track);
            let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), knob);
            ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, t.surface);
        }
        if !label.is_empty() {
            let colour = if *on { t.text } else { t.muted };
            ui.label(RichText::new(label.clone()).size(13.0).color(colour));
        }
        response
    })
    .inner
}

/// A pill segmented control. Returns true when the value changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let t = theme(ui);
    let mut changed = false;
    egui::Frame::new()
        .fill(t.inset)
        .stroke(t.hairline())
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                for (option, label) in options {
                    let selected = *value == *option;
                    let text = RichText::new(*label).size(12.5).color(if selected { t.on_accent } else { t.muted });
                    let button = egui::Button::new(text)
                        .fill(if selected { t.accent } else { Color32::TRANSPARENT })
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(7))
                        .min_size(Vec2::new(0.0, 22.0));
                    if ui.add(button).clicked() && !selected {
                        *value = *option;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// The primary action.
pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let t = theme(ui);
    let button = egui::Button::new(
        RichText::new(text).size(13.0).strong().color(if enabled { t.on_accent } else { t.faint }),
    )
    .fill(t.primary_fill(enabled))
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(9))
    .min_size(Vec2::new(0.0, 30.0));
    ui.add_enabled(enabled, button)
}

/// A normal button: visible at rest, so it does not have to be found by
/// hovering over it.
#[allow(dead_code)]
pub fn button(ui: &mut Ui, text: &str) -> Response {
    let t = theme(ui);
    let button = egui::Button::new(RichText::new(text).size(13.0).color(t.text))
        .fill(t.inset)
        .stroke(Stroke::new(1.0, t.border_strong))
        .corner_radius(CornerRadius::same(9))
        .min_size(Vec2::new(0.0, 30.0));
    ui.add(button)
}

/// A quiet button: no fill until hovered.
pub fn ghost(ui: &mut Ui, text: &str) -> Response {
    let t = theme(ui);
    let button = egui::Button::new(RichText::new(text).size(13.0).color(t.muted))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 28.0));
    ui.add(button)
}

/// A small icon button for rows (delete, say it now).
pub fn icon_button(ui: &mut Ui, glyph_text: &str, tooltip: &str) -> Response {
    let t = theme(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    let colour = if response.hovered() { t.text } else { t.muted };
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(7), t.hover);
        }
        if has_icons() {
            let galley = ui.painter().layout_no_wrap(
                glyph_text.to_string(),
                FontId::new(13.0, FontFamily::Name("icons".into())),
                colour,
            );
            ui.painter().galley(rect.center() - galley.size() / 2.0, galley, colour);
        }
    }
    response.on_hover_text(tooltip)
}

/// A single-line text field sized to the column it is in.
pub fn text_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(width).margin(Margin::symmetric(9, 6)))
}

/// A navigation entry: icon, label, hover and selected states.
pub fn nav(ui: &mut Ui, glyph_text: &str, label: &str, selected: bool) -> Response {
    let t = theme(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if selected {
            ui.painter().rect_filled(rect, CornerRadius::same(8), t.accent_soft(if t.dark { 34 } else { 46 }));
        } else if response.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(8), t.hover);
        }
        let colour = if selected { t.text } else { t.muted };
        let mut x = rect.left() + 10.0;
        if has_icons() {
            let galley = ui.painter().layout_no_wrap(
                glyph_text.to_string(),
                FontId::new(14.0, FontFamily::Name("icons".into())),
                if selected { t.accent } else { colour },
            );
            let y = rect.center().y - galley.size().y / 2.0;
            ui.painter().galley(egui::pos2(x, y), galley, colour);
            x += 24.0;
        }
        let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(13.5), colour);
        ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, colour);
    }
    response
}

/// A tiny uppercase group heading for the navigation column.
pub fn nav_group(ui: &mut Ui, label: &str) {
    let t = theme(ui);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(RichText::new(label.to_uppercase()).size(10.0).color(t.faint).strong());
    });
    ui.add_space(3.0);
}

/// A colour swatch, used for the accent picker.
pub fn swatch(ui: &mut Ui, colour: Color32, selected: bool) -> Response {
    let t = theme(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let centre = rect.center();
        ui.painter().circle_filled(centre, 12.0, colour);
        if selected {
            ui.painter().circle_stroke(centre, 12.5, Stroke::new(2.0, t.text));
            if has_icons() {
                let galley = ui.painter().layout_no_wrap(
                    ICON_CHECK.to_string(),
                    FontId::new(11.0, FontFamily::Name("icons".into())),
                    if luminance(colour) > 0.55 { Color32::BLACK } else { Color32::WHITE },
                );
                let c = if luminance(colour) > 0.55 { Color32::BLACK } else { Color32::WHITE };
                ui.painter().galley(centre - galley.size() / 2.0, galley, c);
            }
        } else if response.hovered() {
            ui.painter().circle_stroke(centre, 13.5, Stroke::new(1.0, t.border_strong));
        }
    }
    response
}

/// The app mark: a small neon orb.
pub fn mark(ui: &mut Ui, colour: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let centre = rect.center();
    let r = size / 2.0;
    let p = ui.painter();
    p.circle_filled(centre, r, colour.linear_multiply(0.16));
    p.circle_filled(centre, r * 0.72, Color32::from_rgb(10, 12, 16));
    p.circle_filled(centre + Vec2::new(-r * 0.18, -r * 0.14), r * 0.34, colour.linear_multiply(0.85));
    p.circle_stroke(centre, r * 0.72, Stroke::new(1.4, colour));
}

/// A window caption button (minimise, maximise, close).
pub fn caption_button(ui: &mut Ui, glyph_text: &str, hover: Color32, t: &Theme) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(7), hover.linear_multiply(if t.dark { 0.30 } else { 0.22 }));
        }
        if has_icons() {
            let colour = if response.hovered() { t.text } else { t.muted };
            let galley = ui.painter().layout_no_wrap(
                glyph_text.to_string(),
                FontId::new(11.0, FontFamily::Name("icons".into())),
                colour,
            );
            ui.painter().galley(rect.center() - galley.size() / 2.0, galley, colour);
        }
    }
    response
}

/// The eight invisible handles around the window edge. Without the system
/// frame there is nothing to grab, so they are drawn here.
pub fn resize_edges(ui: &mut Ui) {
    use egui::ViewportCommand;
    let ctx = ui.ctx().clone();
    const BAND: f32 = 5.0;
    let screen = ctx.viewport_rect();
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    if ctx.input(|i| i.pointer.any_down()) && !ctx.input(|i| i.pointer.primary_down()) {
        return;
    }
    let left = pos.x <= screen.left() + BAND;
    let right = pos.x >= screen.right() - BAND;
    let top = pos.y <= screen.top() + BAND;
    let bottom = pos.y >= screen.bottom() - BAND;
    let direction = match (left, right, top, bottom) {
        (true, _, true, _) => egui::ResizeDirection::NorthWest,
        (_, true, true, _) => egui::ResizeDirection::NorthEast,
        (true, _, _, true) => egui::ResizeDirection::SouthWest,
        (_, true, _, true) => egui::ResizeDirection::SouthEast,
        (true, _, _, _) => egui::ResizeDirection::West,
        (_, true, _, _) => egui::ResizeDirection::East,
        (_, _, true, _) => egui::ResizeDirection::North,
        (_, _, _, true) => egui::ResizeDirection::South,
        _ => return,
    };
    ctx.set_cursor_icon(match direction {
        egui::ResizeDirection::North | egui::ResizeDirection::South => egui::CursorIcon::ResizeVertical,
        egui::ResizeDirection::East | egui::ResizeDirection::West => egui::CursorIcon::ResizeHorizontal,
        egui::ResizeDirection::NorthWest | egui::ResizeDirection::SouthEast => egui::CursorIcon::ResizeNwSe,
        egui::ResizeDirection::NorthEast | egui::ResizeDirection::SouthWest => egui::CursorIcon::ResizeNeSw,
    });
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

// ── icon glyphs (Segoe Fluent Icons / MDL2 Assets) ───────────────────────
// Only the glyphs something draws are named here. The navigation column carries
// its own, next to the label each one belongs to; add another by name when a
// page needs one, rather than keeping a list nothing reads.
pub const ICON_CHECK: &str = "\u{E73E}";
pub const ICON_DELETE: &str = "\u{E74D}";
pub const ICON_CHEVRON_RIGHT: &str = "\u{E76C}";
pub const ICON_SEARCH: &str = "\u{E721}";
pub const ICON_MINIMIZE: &str = "\u{E921}";
pub const ICON_MAXIMIZE: &str = "\u{E922}";
pub const ICON_RESTORE: &str = "\u{E923}";
pub const ICON_CLOSE: &str = "\u{E8BB}";

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame of a UI, so the drawing helpers can be exercised without a
    /// window. Nothing here inspects pixels; it is a smoke test that the shared
    /// widgets lay out and paint without panicking.
    fn frame(ui_fn: impl FnOnce(&mut Ui)) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        let mut f = Some(ui_fn);
        let mut out = ctx.run_ui(input, |ui| {
            set(ui.ctx(), Theme::default());
            if let Some(f) = f.take() {
                f(ui);
            }
        });
        out.textures_delta.clear();
    }

    #[test]
    fn the_shared_widgets_draw() {
        frame(|ui| {
            empty(ui, ICON_SEARCH, "Nothing here", "Add one to get started.");
            let _ = button(ui, "Add");
            let _ = pill(ui, "installed", Color32::GREEN);
            dot(ui, Color32::RED, 7.0);
            glyph(ui, ICON_SEARCH, 12.0, Color32::WHITE);
            glyph_tile(ui, ICON_SEARCH, Color32::WHITE, 44.0);
            divider(ui);
            let mut on = false;
            let _ = switch(ui, &mut on, "on");
            let mut value = 1u8;
            assert!(!segmented(ui, &mut value, &[(1u8, "one"), (2u8, "two")]));
        });
    }

    /// Light and dark are separate palettes, and the accent decides what is
    /// readable on top of it.
    #[test]
    fn the_palette_is_consistent() {
        for dark in [true, false] {
            let t = Theme::new(dark, Color32::from_rgb(0xB6, 0xFF, 0x2E));
            assert_eq!(t.dark, dark);
            // A bright accent takes dark text on top of it, and vice versa.
            assert!(luminance(t.on_accent) < 0.5 || luminance(t.accent) < 0.5);
            assert_ne!(t.text, t.bg, "text must not match the background");
            assert_ne!(t.surface, t.inset, "surfaces must be distinguishable");
        }
    }
}
