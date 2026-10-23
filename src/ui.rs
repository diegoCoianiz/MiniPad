//! Ribbon, status bar, dialogs and the eframe entry point.

use crate::app::{After, App, Tab};
use crate::doc::{Align, Leader, ListKind, NO_COLOR, Pos, Rgba, Style, TabKind, TabStop, Wrap};
use crate::editor::c32;
use crate::fonts;
use crate::theme::{self, CustomPalette, Theme, ThemeName};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, Response, RichText,
    Sense, Stroke, Ui, pos2, vec2,
};

const GROUP_H: f32 = 66.0;
/// Shown in the status bar; hover it to read what the version changed.
const VERSION: &str = "1.2.1";
const VERSION_NOTES: &str = "v 1.2.1 *Colores de navegación y sugerencias del diccionario con contraste automático";

/// Colour used only for measuring a galley's width; never drawn.
const MEASURE: Color32 = Color32::PLACEHOLDER;

// ================================================================ widgets

/// Shows a tooltip on the dedicated tooltip surface (not the dialog surface).
/// Disabled widgets get the same treatment instead of egui's default suppression.
pub(crate) fn show_tip(ui: &Ui, resp: Response, text: impl Into<egui::WidgetText>) -> Response {
    let t = theme::tokens_for(ui);
    let frame = Frame::new()
        .fill(t.tooltip_bg)
        .stroke(Stroke::new(1.0, t.chrome_border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::same(7));
    let mut tooltip = if resp.enabled() {
        egui::Tooltip::for_enabled(&resp)
    } else {
        egui::Tooltip::for_disabled(&resp)
    };
    tooltip.popup = tooltip.popup.frame(frame);
    tooltip.show(|ui| {
        ui.style_mut().visuals.override_text_color = Some(t.tooltip_fg);
        ui.label(text);
    });
    resp
}

/// Four drag fields for the wrap clearance (top, right, bottom, left), in points.
/// Returns true when any value changed so the caller can apply it.
fn wrap_margins_menu(ui: &mut Ui, d: &mut [f32; 4]) -> bool {
    let mut changed = false;
    for (label, i) in [("Top", 0usize), ("Right", 1), ("Bottom", 2), ("Left", 3)] {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).size(12.0));
            changed |= ui
                .add(egui::DragValue::new(&mut d[i]).speed(0.5).range(0.0..=180.0).suffix(" pt"))
                .changed();
        });
    }
    changed
}

/// One row of the font menu: a heart button on the left that toggles the family
/// as a favorite, then the name (previewed in its own face when it is loaded).
/// Returns `(picked, toggled)` so the caller can act without borrowing itself.
fn font_row(ui: &mut Ui, name: &str, selected: bool, favorite: bool) -> (bool, bool) {
    let mut picked = false;
    let mut toggled = false;
    ui.horizontal(|ui| {
        let heart = if favorite { "♥" } else { "♡" };
        let resp = ui.add(egui::Button::new(RichText::new(heart).size(14.0)).frame(false));
        if show_tip(ui, resp, if favorite { "Remove from favorites" } else { "Add to favorites" }).clicked() {
            toggled = true;
        }
        let text = if fonts::is_loaded(name) {
            let fam = fonts::resolve(fonts::font_id(name), false, false).family;
            RichText::new(name).font(FontId::new(14.0, fam))
        } else {
            RichText::new(name).size(14.0)
        };
        if ui.selectable_label(selected, text).clicked() {
            picked = true;
        }
    });
    (picked, toggled)
}

#[derive(Clone, Copy)]
enum Icon {
    AlignLeft,
    AlignCenter,
    AlignRight,
    Justify,
    Bullets,
    Numbers,
    IndentMore,
    IndentLess,
    LineSpacing,
}

fn paint_icon(p: &egui::Painter, r: Rect, icon: Icon, col: Color32) {
    let s = Stroke::new(1.3, col);
    let c = r.center();
    let w = 14.0;
    let left = c.x - w / 2.0;
    let rows = [-4.5f32, -1.5, 1.5, 4.5];
    let line = |x0: f32, x1: f32, y: f32| p.line_segment([pos2(x0, c.y + y), pos2(x1, c.y + y)], s);
    match icon {
        Icon::AlignLeft => {
            for (i, y) in rows.iter().enumerate() {
                line(left, left + if i % 2 == 0 { w } else { w * 0.6 }, *y);
            }
        }
        Icon::AlignCenter => {
            for (i, y) in rows.iter().enumerate() {
                let ww = if i % 2 == 0 { w } else { w * 0.6 };
                line(c.x - ww / 2.0, c.x + ww / 2.0, *y);
            }
        }
        Icon::AlignRight => {
            for (i, y) in rows.iter().enumerate() {
                line(left + w - if i % 2 == 0 { w } else { w * 0.6 }, left + w, *y);
            }
        }
        Icon::Justify => {
            for y in rows {
                line(left, left + w, y);
            }
        }
        Icon::Bullets | Icon::Numbers => {
            for (i, y) in [-4.0f32, 0.0, 4.0].iter().enumerate() {
                if matches!(icon, Icon::Bullets) {
                    p.circle_filled(pos2(left + 1.5, c.y + y), 1.4, col);
                } else {
                    p.text(pos2(left + 1.5, c.y + y), Align2::CENTER_CENTER, format!("{}", i + 1), FontId::proportional(6.5), col);
                }
                line(left + 5.0, left + w, *y);
            }
        }
        Icon::IndentMore | Icon::IndentLess => {
            for y in rows {
                line(left + 6.0, left + w, y);
            }
            let (a, b) = if matches!(icon, Icon::IndentMore) { (left, left + 4.0) } else { (left + 4.0, left) };
            p.add(egui::Shape::convex_polygon(
                vec![pos2(a, c.y - 3.0), pos2(a, c.y + 3.0), pos2(b, c.y)],
                col,
                Stroke::NONE,
            ));
        }
        Icon::LineSpacing => {
            for y in rows {
                line(left + 6.0, left + w, y);
            }
            p.line_segment([pos2(left + 2.0, c.y - 5.0), pos2(left + 2.0, c.y + 5.0)], s);
            p.line_segment([pos2(left, c.y - 3.0), pos2(left + 2.0, c.y - 5.5)], s);
            p.line_segment([pos2(left + 4.0, c.y - 3.0), pos2(left + 2.0, c.y - 5.5)], s);
            p.line_segment([pos2(left, c.y + 3.0), pos2(left + 2.0, c.y + 5.5)], s);
            p.line_segment([pos2(left + 4.0, c.y + 3.0), pos2(left + 2.0, c.y + 5.5)], s);
        }
    }
}

fn button_bg(ui: &Ui, r: Rect, resp: &Response, selected: bool) {
    let t = theme::tokens_for(ui);
    let fill = if resp.is_pointer_button_down_on() || selected {
        Some(t.chrome_active_bg)
    } else if resp.hovered() {
        Some(t.chrome_hover_bg)
    } else {
        None
    };
    if let Some(f) = fill {
        ui.painter().rect_filled(r, CornerRadius::same(3), f);
        if selected {
            ui.painter().rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, t.chrome_accent), egui::StrokeKind::Inside);
        }
    }
}

fn icon_btn(ui: &mut Ui, icon: Icon, selected: bool, tip: &str) -> Response {
    let t = theme::tokens_for(ui);
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    button_bg(ui, r, &resp, selected);
    let col = if selected { t.chrome_icon_active } else { t.chrome_icon };
    paint_icon(ui.painter(), r, icon, col);
    show_tip(ui, resp, tip)
}

/// Small text-glyph button (B, I, U ...).
fn tool(ui: &mut Ui, text: RichText, selected: bool, tip: &str) -> Response {
    let t = theme::tokens_for(ui);
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    button_bg(ui, r, &resp, selected);
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    let pos = r.center() - galley.size() / 2.0;
    let col = if selected { t.chrome_icon_active } else { t.chrome_fg };
    ui.painter().galley(pos, galley, col);
    show_tip(ui, resp, tip)
}

fn big_btn(ui: &mut Ui, glyph: &str, label: &str, tip: &str) -> Response {
    big_btn_sel(ui, glyph, label, false, tip)
}

fn big_btn_sel(ui: &mut Ui, glyph: &str, label: &str, selected: bool, tip: &str) -> Response {
    let t = theme::tokens_for(ui);
    let font = FontId::proportional(12.0);
    let lw = ui.painter().layout_no_wrap(label.to_string(), font.clone(), MEASURE).size().x;
    let (r, resp) = ui.allocate_exact_size(vec2((lw + 12.0).max(46.0), GROUP_H - 4.0), Sense::click());
    button_bg(ui, r, &resp, selected);
    let (icon_col, label_col) = if selected { (t.chrome_icon_active, t.chrome_icon_active) } else { (t.chrome_icon, t.chrome_fg) };
    ui.painter().text(pos2(r.center().x, r.top() + 20.0), Align2::CENTER_CENTER, glyph, FontId::proportional(24.0), icon_col);
    ui.painter().text(pos2(r.center().x, r.bottom() - 13.0), Align2::CENTER_CENTER, label, font, label_col);
    show_tip(ui, resp, tip)
}

/// Small button with a glyph and label side by side.
fn small_btn(ui: &mut Ui, glyph: &str, label: &str, tip: &str) -> Response {
    let t = theme::tokens_for(ui);
    let font = FontId::proportional(12.5);
    let lw = ui.painter().layout_no_wrap(label.to_string(), font.clone(), MEASURE).size().x;
    let (r, resp) = ui.allocate_exact_size(vec2(lw + 30.0, 20.0), Sense::click());
    button_bg(ui, r, &resp, false);
    ui.painter().text(pos2(r.left() + 11.0, r.center().y), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), t.chrome_icon);
    ui.painter().text(pos2(r.left() + 24.0, r.center().y), Align2::LEFT_CENTER, label, font, t.chrome_fg);
    show_tip(ui, resp, tip)
}

/// A ribbon group: content plus a caption, followed by a separator.
fn group(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    let t = theme::tokens_for(ui);
    let r = ui.vertical(|ui| {
        let inner = ui.horizontal(|ui| {
            ui.set_min_height(GROUP_H);
            add(ui);
        });
        let w = inner.response.rect.width();
        ui.allocate_ui_with_layout(vec2(w, 14.0), egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.set_min_width(w);
            ui.label(RichText::new(title).size(11.0).color(t.chrome_fg_muted));
        });
    });
    let x = r.response.rect.right() + 4.0;
    let y0 = r.response.rect.top() + 4.0;
    let y1 = r.response.rect.bottom() - 4.0;
    ui.add_space(8.0);
    ui.painter().line_segment([pos2(x, y0), pos2(x, y1)], Stroke::new(1.0, t.chrome_border));
    ui.add_space(4.0);
}

const THEME_COLORS: [[u8; 3]; 10] = [
    [0xFF, 0xFF, 0xFF], [0x00, 0x00, 0x00], [0xE7, 0xE6, 0xE6], [0x44, 0x54, 0x6A], [0x44, 0x72, 0xC4],
    [0xED, 0x7D, 0x31], [0xA5, 0xA5, 0xA5], [0xFF, 0xC0, 0x00], [0x5B, 0x9B, 0xD5], [0x70, 0xAD, 0x47],
];
const STANDARD_COLORS: [[u8; 3]; 10] = [
    [0xC0, 0x00, 0x00], [0xFF, 0x00, 0x00], [0xFF, 0xC0, 0x00], [0xFF, 0xFF, 0x00], [0x92, 0xD0, 0x50],
    [0x00, 0xB0, 0x50], [0x00, 0xB0, 0xF0], [0x00, 0x70, 0xC0], [0x00, 0x20, 0x60], [0x70, 0x30, 0xA0],
];
const HIGHLIGHT_COLORS: [[u8; 3]; 15] = [
    [255, 255, 0], [0, 255, 0], [0, 255, 255], [255, 0, 255], [0, 0, 255], [255, 0, 0], [0, 0, 128], [0, 128, 128],
    [0, 128, 0], [128, 0, 128], [128, 0, 0], [128, 128, 0], [128, 128, 128], [192, 192, 192], [0, 0, 0],
];

fn rgba(c: [u8; 3]) -> Rgba {
    [c[0], c[1], c[2], 255]
}

fn swatch(ui: &mut Ui, c: Rgba, tip: &str) -> bool {
    let t = theme::tokens_for(ui);
    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    ui.painter().rect_filled(r, 0.0, c32(c));
    let stroke = if resp.hovered() { Stroke::new(2.0, t.chrome_accent) } else { Stroke::new(1.0, t.chrome_border) };
    ui.painter().rect_stroke(r, 0.0, stroke, egui::StrokeKind::Inside);
    show_tip(ui, resp, tip).clicked()
}

/// Shading variants of a theme color, like Word's palette columns.
fn tint(c: [u8; 3], t: f32) -> Rgba {
    let f = |v: u8| -> u8 {
        if t >= 0.0 {
            (v as f32 + (255.0 - v as f32) * t) as u8
        } else {
            (v as f32 * (1.0 + t)) as u8
        }
    };
    [f(c[0]), f(c[1]), f(c[2]), 255]
}

/// Font color palette; returns the picked color.
fn color_palette(ui: &mut Ui, custom: &mut Color32) -> Option<Rgba> {
    let mut picked = None;
    ui.set_min_width(200.0);
    if ui.button("Automatic").clicked() {
        picked = Some(crate::doc::BLACK);
    }
    ui.label(RichText::new("Theme Colors").color(theme::tokens_for(ui).dialog_fg));
    ui.horizontal(|ui| {
        for c in THEME_COLORS {
            if swatch(ui, rgba(c), "") {
                picked = Some(rgba(c));
            }
        }
    });
    for t in [0.8, 0.6, 0.4, -0.25, -0.5] {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for c in THEME_COLORS {
                let col = tint(c, t);
                if swatch(ui, col, "") {
                    picked = Some(col);
                }
            }
        });
    }
    ui.label(RichText::new("Standard Colors").color(theme::tokens_for(ui).dialog_fg));
    ui.horizontal(|ui| {
        for c in STANDARD_COLORS {
            if swatch(ui, rgba(c), "") {
                picked = Some(rgba(c));
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("More Colors:");
        egui::color_picker::color_edit_button_srgba(ui, custom, egui::color_picker::Alpha::Opaque);
        if ui.button("Apply").clicked() {
            picked = Some([custom.r(), custom.g(), custom.b(), 255]);
        }
    });
    if picked.is_some() {
        ui.close();
    }
    picked
}

// ================================================================ eframe

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Start maximized even when the initial builder request is ignored while
        // the window is still hidden (Windows) or a saved window state overrides
        // it. Repeat for a few frames without trusting the reported state, since
        // the builder request can already make `viewport().maximized` report true.
        // After that the user can restore the window as usual.
        if self.maximize_pending > 0 {
            self.maximize_pending -= 1;
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            ctx.request_repaint();
        }

        // Ask before closing with unsaved changes.
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(After::Exit);
        }

        let title = format!("{}{} - MiniPad", self.title(), if self.dirty { " •" } else { "" });
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }

        // Ctrl+wheel zoom.
        let zd = ctx.input(|i| i.zoom_delta());
        if zd != 1.0 {
            self.zoom = (self.zoom * zd).clamp(0.1, 5.0);
        }

        // Open dropped files.
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()).filter(|p| p.is_file())) {
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp") {
                if let Ok(b) = std::fs::read(&path) {
                    self.insert_image_bytes(b);
                }
            } else if !self.dirty {
                self.open_path(&path);
            } else {
                self.error("Save or discard your changes before opening another document.");
            }
        }

        // Dialogs that work on the whole document leave the header/footer first.
        if self.story.is_some() && (self.dlg.page_setup || self.dlg.header_footer || self.find.open || self.dlg.word_count) {
            self.close_story();
        }

        self.text_focus_frame = false;
        // Fonts requested last frame are now registered with egui: re-layout with them.
        if crate::fonts::commit() {
            self.rev += 1;
        }
        self.handle_events(&ctx);
        self.autosave_tick(&ctx);

        let t = self.theme.tokens;
        egui::Panel::top("ribbon").frame(Frame::NONE.fill(t.chrome_bg)).show(ui, |ui| self.ribbon(ui));
        egui::Panel::bottom("status").frame(Frame::NONE.fill(t.status_bg).inner_margin(Margin::symmetric(8, 3))).show(ui, |ui| self.status_bar(ui));

        if self.show_nav && self.story.is_none() {
            egui::Panel::left("navigation")
                .resizable(true)
                .default_size(240.0)
                .size_range(160.0..=420.0)
                .frame(Frame::NONE.fill(t.chrome_bg_alt).inner_margin(Margin::symmetric(8, 6)))
                .show(ui, |ui| self.nav_panel(ui));
        }

        // The desk behind the pages (a token so Custom themes can tune it).
        let bg = t.desk_bg;
        egui::CentralPanel::default().frame(Frame::NONE.fill(bg)).show(ui, |ui| {
            if self.show_ruler {
                self.ensure_layout();
                self.ruler(ui);
            }
            let avail = ui.available_size();
            self.viewport_h = avail.y;
            self.apply_fit(avail);
            let scroll = egui::ScrollArea::both().id_salt("doc_scroll").auto_shrink([false, false]).show(ui, |ui| {
                self.canvas(ui);
            });
            self.canvas_scroll_x = scroll.state.offset.x;
        });

        self.dialogs(&ctx);
        self.text_input_focused = self.text_focus_frame;
    }
}

impl App {
    fn track(&mut self, r: &Response) {
        if r.has_focus() {
            self.text_focus_frame = true;
        }
    }

    /// Switches theme at runtime: repaints egui and persists the choice.
    fn choose_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        self.theme = theme;
        theme::apply_theme(ctx, &theme);
        self.settings.set_theme(theme.name);
        self.settings.save();
    }

    /// Activates the Custom theme. The first time, the palette is seeded from
    /// the current theme so selecting Custom does not jump to another mood.
    fn choose_custom_theme(&mut self, ctx: &egui::Context) {
        if self.settings.custom_theme.is_none() {
            self.settings.set_custom_theme(CustomPalette::from_tokens(&self.theme.tokens));
        }
        let palette = self.settings.custom_theme.unwrap_or_default();
        self.choose_theme(ctx, Theme::custom(&palette));
    }

    /// Opens the colour editor on a copy of the palette; `Apply` persists it.
    fn open_custom_theme_dialog(&mut self) {
        let palette = self
            .settings
            .custom_theme
            .unwrap_or_else(|| CustomPalette::from_tokens(&self.theme.tokens));
        self.dlg.custom_palette = palette;
        self.dlg.custom_before = Some(self.theme);
        self.dlg.custom_theme = true;
    }

    // ================================================================ ribbon

    fn ribbon(&mut self, ui: &mut Ui) {
        let t = self.theme.tokens;
        // Tab strip.
        Frame::NONE.fill(t.chrome_bg_alt).inner_margin(Margin::symmetric(6, 0)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_height(30.0);
                ui.visuals_mut().override_text_color = Some(t.chrome_fg);
                ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                ui.visuals_mut().widgets.hovered.weak_bg_fill = t.chrome_hover_bg;
                ui.visuals_mut().widgets.open.weak_bg_fill = t.chrome_active_bg;
                ui.menu_button(RichText::new("  File  ").size(14.0), |ui| {
                    ui.visuals_mut().override_text_color = None;
                    self.file_menu(ui);
                });
                for (tab, name) in [
                    (Tab::Home, "Home"),
                    (Tab::Insert, "Insert"),
                    (Tab::LayoutView, "Layout & View"),
                    (Tab::Review, "Review"),
                ] {
                    let sel = self.tab == tab;
                    let font = FontId::proportional(14.0);
                    let w = ui.painter().layout_no_wrap(name.into(), font.clone(), MEASURE).size().x + 22.0;
                    let (r, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
                    let p = ui.painter();
                    if sel {
                        let tab_rect = Rect::from_min_max(pos2(r.left(), r.top() + 4.0), r.max);
                        p.rect_filled(tab_rect, CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, t.chrome_active_bg);
                    } else if resp.hovered() {
                        p.rect_filled(r, 0.0, t.chrome_hover_bg);
                    }
                    let col = if sel { t.chrome_accent_fg } else { t.chrome_fg_muted };
                    p.text(r.center() + vec2(0.0, 2.0), Align2::CENTER_CENTER, name, font, col);
                    if resp.clicked() {
                        self.tab = tab;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    let redo = ui.add_enabled(self.can_redo(), egui::Button::new("↷").frame(false));
                    if show_tip(ui, redo, "Redo (Ctrl+Y)").clicked() {
                        self.redo();
                    }
                    let undo = ui.add_enabled(self.can_undo(), egui::Button::new("↶").frame(false));
                    if show_tip(ui, undo, "Undo (Ctrl+Z)").clicked() {
                        self.undo();
                    }
                    let save = ui.add(egui::Button::new("💾").frame(false));
                    if show_tip(ui, save, "Save (Ctrl+S)").clicked() {
                        self.save();
                    }
                });
            });
        });

        // Ribbon body (the group cut-out).
        Frame::NONE.fill(t.chrome_bg_alt).inner_margin(Margin { left: 8, right: 8, top: 4, bottom: 2 }).show(ui, |ui| {
            egui::ScrollArea::horizontal().id_salt("ribbon_scroll").show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    if self.story.is_some() {
                        self.story_group(ui);
                    }
                    match self.tab {
                        Tab::Home => self.home_tab(ui),
                        Tab::Insert => self.insert_tab(ui),
                        Tab::LayoutView => self.layout_view_tab(ui),
                        Tab::Review => self.review_tab(ui),
                    }
                });
            });
        });
        let r = ui.max_rect();
        ui.painter().line_segment(
            [pos2(r.left(), ui.min_rect().bottom()), pos2(r.right(), ui.min_rect().bottom())],
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );
    }

    /// Contextual group shown while a header or footer is being edited.
    fn story_group(&mut self, ui: &mut Ui) {
        let Some(st) = self.story.as_deref() else { return };
        let (header, page, name, is_hf) = (st.zone() == crate::story::Zone::Header, st.page, st.name(), st.is_hf());
        let title = match st.target {
            crate::story::Target::Hf { .. } => "Header & Footer",
            crate::story::Target::Note(_) => "Notes",
            crate::story::Target::Nested(_) => "Table",
        };
        group(ui, title, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(name).strong().color(theme::tokens_for(ui).chrome_accent));
                if !is_hf {
                    return;
                }
                ui.horizontal(|ui| {
                    if small_btn(ui, "#", "Page", "Insert the page number").clicked() {
                        self.insert_page_field(false);
                    }
                    if small_btn(ui, "Σ", "Pages", "Insert the number of pages").clicked() {
                        self.insert_page_field(true);
                    }
                });
                let (glyph, label) = if header { ("▁", "Go to Footer") } else { ("▔", "Go to Header") };
                if small_btn(ui, glyph, label, "Switch between the header and the footer").clicked() {
                    self.story_req = Some(crate::story::StoryReq::Open { page, header: !header, at: None });
                }
            });
            let tip = if is_hf { "Close Header and Footer (Esc)" } else { "Return to the document (Esc)" };
            if big_btn(ui, "✖", "Close", tip).clicked() {
                self.story_req = Some(crate::story::StoryReq::Close { at: None });
            }
        });
    }

    fn file_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(230.0);
        let item = |ui: &mut Ui, label: &str, sc: &str| ui.add(egui::Button::new(label).shortcut_text(sc));
        if item(ui, "📄  New", "Ctrl+N").clicked() {
            ui.close();
            self.guarded(After::New);
        }
        if item(ui, "📂  Open…", "Ctrl+O").clicked() {
            ui.close();
            self.guarded(After::Open);
        }
        ui.separator();
        if item(ui, "💾  Save", "Ctrl+S").clicked() {
            ui.close();
            self.save();
        }
        if item(ui, "    Save As…", "Ctrl+Shift+S").clicked() {
            ui.close();
            self.save_as();
        }
        ui.separator();
        if item(ui, "📕  Export as PDF…", "").clicked() {
            ui.close();
            self.export_pdf();
        }
        if item(ui, "🌐  Export as Web Page…", "").clicked() {
            ui.close();
            self.export_html();
        }
        if item(ui, "🗒  Export as Plain Text…", "").clicked() {
            ui.close();
            self.export_text();
        }
        ui.separator();
        if item(ui, "🖶  Print…", "Ctrl+P").clicked() {
            ui.close();
            self.print();
        }
        if item(ui, "    Page Setup…", "").clicked() {
            ui.close();
            self.dlg.page_setup = true;
        }
        ui.separator();
        if item(ui, "    Keyboard Shortcuts", "").clicked() {
            ui.close();
            self.dlg.shortcuts = true;
        }
        if item(ui, "    About MiniPad", "").clicked() {
            ui.close();
            self.dlg.about = true;
        }
        ui.separator();
        if item(ui, "    Exit", "Alt+F4").clicked() {
            ui.close();
            self.guarded(After::Exit);
        }
    }

    fn home_tab(&mut self, ui: &mut Ui) {
        let f = self.current_fmt();
        let para = self.current_para().clone();

        group(ui, "Clipboard", |ui| {
            if big_btn(ui, "📋", "Paste", "Paste (Ctrl+V)").clicked() {
                self.paste_from_system();
            }
            ui.vertical(|ui| {
                if small_btn(ui, "✂", "Cut", "Cut (Ctrl+X)").clicked() {
                    self.cut();
                }
                if small_btn(ui, "🗐", "Copy", "Copy (Ctrl+C)").clicked() {
                    self.copy();
                }
                let fp = self.format_painter.is_some();
                let r = small_btn(ui, "🖌", if fp { "Painting…" } else { "Format Painter" }, "Copy formatting from one place and apply it to another");
                if r.clicked() {
                    self.start_format_painter();
                }
            });
        });

        group(ui, "Font", |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let name = fonts::font_name(f.font);
                    egui::ComboBox::from_id_salt("font_family").selected_text(&name).width(230.0).height(400.0).show_ui(ui, |ui| {
                        ui.set_min_width(210.0);
                        let favs = self.settings.favorites.clone();
                        let all = fonts::available();
                        let mut chosen: Option<String> = None;
                        let mut toggled: Option<String> = None;
                        if !favs.is_empty() {
                            ui.label(RichText::new("Favorites").size(12.0).weak());
                            for n in &favs {
                                let (pick, star) = font_row(ui, n, n.eq_ignore_ascii_case(&name), true);
                                if pick {
                                    chosen = Some(n.clone());
                                }
                                if star {
                                    toggled = Some(n.clone());
                                }
                            }
                            ui.separator();
                        }
                        ui.label(RichText::new("All fonts").size(12.0).weak());
                        for n in &all {
                            if self.settings.is_favorite(n) {
                                continue;
                            }
                            let (pick, star) = font_row(ui, n, n.eq_ignore_ascii_case(&name), false);
                            if pick {
                                chosen = Some(n.clone());
                            }
                            if star {
                                toggled = Some(n.clone());
                            }
                        }
                        if let Some(n) = chosen {
                            fonts::request(ui.ctx(), [n.clone()]);
                            let id = fonts::font_id(&n);
                            self.apply_fmt(|c| c.font = id);
                        }
                        if let Some(n) = toggled {
                            self.settings.toggle_favorite(&n);
                            self.settings.save();
                            // Keep the chosen default in step with the favorites list.
                            if let Some(first) = self.settings.favorites.first() {
                                crate::fonts::set_preferred_default(first);
                            }
                        }
                    });
                    let size_text = if f.size.fract() == 0.0 { format!("{}", f.size) } else { format!("{:.1}", f.size) };
                    egui::ComboBox::from_id_salt("font_size").selected_text(size_text).width(48.0).height(400.0).show_ui(ui, |ui| {
                        for s in App::SIZES {
                            if ui.selectable_label((f.size - s).abs() < 0.01, format!("{s}")).clicked() {
                                self.apply_fmt(|c| c.size = s);
                            }
                        }
                    });
                    if tool(ui, RichText::new("A▴").size(13.0), false, "Increase Font Size (Ctrl+])").clicked() {
                        self.grow_font(true);
                    }
                    if tool(ui, RichText::new("A▾").size(11.0), false, "Decrease Font Size (Ctrl+[)").clicked() {
                        self.grow_font(false);
                    }
                    let case_resp = ui.menu_button(RichText::new("Aa▾"), |ui| {
                        for (i, label) in ["Sentence case.", "lowercase", "UPPERCASE", "Capitalize Each Word", "tOGGLE cASE"].iter().enumerate() {
                            if ui.button(*label).clicked() {
                                self.change_case(i as u8);
                                ui.close();
                            }
                        }
                    })
                    .response;
                    show_tip(ui, case_resp, "Change Case");
                    if tool(ui, RichText::new("A⌫").size(12.0), false, "Clear Formatting (Ctrl+Space)").clicked() {
                        self.clear_formatting();
                    }
                });
                ui.horizontal(|ui| {
                    if tool(ui, RichText::new("B").size(15.0), f.bold, "Bold (Ctrl+B)").clicked() {
                        self.toggle_bold();
                    }
                    if tool(ui, RichText::new("I").italics().size(15.0), f.italic, "Italic (Ctrl+I)").clicked() {
                        self.toggle_italic();
                    }
                    if tool(ui, RichText::new("U").underline().size(15.0), f.underline, "Underline (Ctrl+U)").clicked() {
                        self.toggle_underline();
                    }
                    if tool(ui, RichText::new("ab").strikethrough().size(14.0), f.strike, "Strikethrough (Ctrl+Shift+D)").clicked() {
                        self.toggle_strike();
                    }
                    if tool(ui, RichText::new("x₂").size(14.0), f.script == -1, "Subscript (Ctrl+=)").clicked() {
                        self.toggle_script(-1);
                    }
                    if tool(ui, RichText::new("x²").size(14.0), f.script == 1, "Superscript (Ctrl+Shift+=)").clicked() {
                        self.toggle_script(1);
                    }
                    ui.add_space(4.0);
                    // Highlight color.
                    let hl = self.last_highlight;
                    let r = tool(ui, RichText::new("ab").size(14.0), false, "Text Highlight Color");
                    ui.painter().rect_filled(Rect::from_min_size(r.rect.left_bottom() + vec2(5.0, -5.0), vec2(16.0, 3.0)), 0.0, c32(hl));
                    if r.clicked() {
                        self.apply_fmt(|c| c.highlight = hl);
                    }
                    ui.menu_button("▾", |ui| {
                        ui.set_min_width(140.0);
                        egui::Grid::new("hl").spacing(vec2(4.0, 4.0)).show(ui, |ui| {
                            for (i, c) in HIGHLIGHT_COLORS.iter().enumerate() {
                                if swatch(ui, rgba(*c), "") {
                                    self.last_highlight = rgba(*c);
                                    self.apply_fmt(|f| f.highlight = rgba(*c));
                                    ui.close();
                                }
                                if i % 5 == 4 {
                                    ui.end_row();
                                }
                            }
                        });
                        if ui.button("No Color").clicked() {
                            self.apply_fmt(|f| f.highlight = NO_COLOR);
                            ui.close();
                        }
                    });
                    // Font color.
                    let fc = self.last_color;
                    let r = tool(ui, RichText::new("A").size(15.0), false, "Font Color");
                    ui.painter().rect_filled(Rect::from_min_size(r.rect.left_bottom() + vec2(5.0, -5.0), vec2(16.0, 3.0)), 0.0, c32(fc));
                    if r.clicked() {
                        self.apply_fmt(|c| c.color = fc);
                    }
                    ui.menu_button("▾", |ui| {
                        let mut custom = c32(self.last_color);
                        if let Some(c) = color_palette(ui, &mut custom) {
                            self.last_color = c;
                            self.apply_fmt(|f| f.color = c);
                        }
                    });
                });
            });
        });

        group(ui, "Paragraph", |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if icon_btn(ui, Icon::Bullets, para.list == ListKind::Bullet, "Bullets (Ctrl+Shift+L)").clicked() {
                        self.toggle_list(ListKind::Bullet);
                    }
                    if icon_btn(ui, Icon::Numbers, para.list == ListKind::Number, "Numbering").clicked() {
                        self.toggle_list(ListKind::Number);
                    }
                    if icon_btn(ui, Icon::IndentLess, false, "Decrease Indent (Ctrl+Shift+M)").clicked() {
                        self.indent(-1);
                    }
                    if icon_btn(ui, Icon::IndentMore, false, "Increase Indent (Ctrl+M)").clicked() {
                        self.indent(1);
                    }
                    if tool(ui, RichText::new("¶").size(15.0), self.show_marks, "Show/Hide formatting marks").clicked() {
                        self.show_marks = !self.show_marks;
                    }
                });
                ui.horizontal(|ui| {
                    for (a, icon, tip) in [
                        (Align::Left, Icon::AlignLeft, "Align Left (Ctrl+L)"),
                        (Align::Center, Icon::AlignCenter, "Center (Ctrl+E)"),
                        (Align::Right, Icon::AlignRight, "Align Right (Ctrl+R)"),
                        (Align::Justify, Icon::Justify, "Justify (Ctrl+J)"),
                    ] {
                        if icon_btn(ui, icon, para.align == a, tip).clicked() {
                            self.set_align(a);
                        }
                    }
                    let r = icon_btn(ui, Icon::LineSpacing, false, "Line and Paragraph Spacing");
                    egui::Popup::menu(&r).show(|ui| {
                        for v in [1.0, 1.15, 1.5, 2.0, 2.5, 3.0] {
                            let label = format!("{}{v}", if (para.line_spacing - v).abs() < 0.01 { "✔ " } else { "    " });
                            if ui.button(label).clicked() {
                                self.set_line_spacing(v);
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Paragraph Options…").clicked() {
                            self.dlg.paragraph = true;
                            ui.close();
                        }
                    });
                });
            });
        });

        group(ui, "Styles", |ui| {
            let t = self.theme.tokens;
            let dark = theme::is_dark_sheet(&self.theme);
            for st in Style::ALL {
                let sel = para.style == st;
                let (r, resp) = ui.allocate_exact_size(vec2(74.0, GROUP_H - 4.0), Sense::click());
                let p = ui.painter();
                p.rect_filled(r, CornerRadius::same(2), t.chrome_bg);
                let stroke = if sel {
                    Stroke::new(2.0, t.chrome_accent)
                } else if resp.hovered() {
                    Stroke::new(1.0, t.chrome_accent_hover)
                } else {
                    Stroke::new(1.0, t.chrome_border)
                };
                p.rect_stroke(r, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                let sf = st.char_fmt();
                let res = fonts::resolve(sf.font, sf.bold, sf.italic);
                let size = (sf.size * 1.25).min(20.0);
                let mut preview = c32(sf.color);
                if dark {
                    preview = theme::remap_for_dark_sheet(preview, &t);
                }
                let job = egui::text::LayoutJob::single_section(
                    "AaBbCc".into(),
                    egui::TextFormat {
                        font_id: FontId::new(size, res.family),
                        color: preview,
                        italics: sf.italic || res.fake_italic,
                        ..Default::default()
                    },
                );
                let g = ui.painter().layout_job(job);
                let clip = ui.painter().with_clip_rect(r.shrink(3.0));
                clip.galley(pos2(r.center().x - g.size().x / 2.0, r.top() + 20.0 - g.size().y / 2.0), g, t.chrome_fg);
                p.text(pos2(r.center().x, r.bottom() - 11.0), Align2::CENTER_CENTER, st.name(), FontId::proportional(11.0), t.chrome_fg_muted);
                if show_tip(ui, resp, format!("Apply the {} style", st.name())).clicked() {
                    self.set_style(st);
                }
            }
        });

        group(ui, "Editing", |ui| {
            ui.vertical(|ui| {
                if small_btn(ui, "🔍", "Find", "Find (Ctrl+F)").clicked() {
                    self.find.open = true;
                    self.find.replace = false;
                    self.find.focus_query = true;
                }
                if small_btn(ui, "⇄", "Replace", "Replace (Ctrl+H)").clicked() {
                    self.find.open = true;
                    self.find.replace = true;
                    self.find.focus_query = true;
                }
                if small_btn(ui, "⬚", "Select All", "Select All (Ctrl+A)").clicked() {
                    let e = self.doc.end();
                    self.select(crate::doc::Pos::default(), e);
                }
            });
        });
    }

    fn insert_tab(&mut self, ui: &mut Ui) {
        group(ui, "Pages", |ui| {
            if big_btn(ui, "⤓", "Page Break", "Start the next page at the cursor (Ctrl+Enter)").clicked() {
                self.insert_page_break();
            }
            if big_btn(ui, "↵", "Line Break", "Start a new line in the same paragraph (Shift+Enter)").clicked() {
                self.insert_line_break();
            }
            if big_btn(ui, "▥", "Section", "Start a new section on the next page").clicked() {
                self.insert_section_break();
            }
        });
        group(ui, "Tables", |ui| {
            let r = big_btn(ui, "▦", "Table", "Insert a table");
            egui::Popup::menu(&r).show(|ui| {
                ui.label("Insert Table");
                let hover = egui::Id::new("table_picker_hover");
                let (hr, hc): (u32, u32) = ui.data(|d| d.get_temp(hover)).unwrap_or((0, 0));
                let mut new_hover = (0, 0);
                let mut picked = None;
                egui::Grid::new("table_picker").spacing(vec2(2.0, 2.0)).show(ui, |ui| {
                    for r in 1..=8u32 {
                        for c in 1..=10u32 {
                            let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                            let on = r <= hr && c <= hc;
                            let t = theme::tokens_for(ui);
                            ui.painter().rect_filled(rect, 0.0, if on { t.chrome_accent_hover } else { t.dialog_bg });
                            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, t.chrome_border), egui::StrokeKind::Inside);
                            if resp.hovered() {
                                new_hover = (r, c);
                            }
                            if resp.clicked() {
                                picked = Some((r, c));
                            }
                        }
                        ui.end_row();
                    }
                });
                ui.data_mut(|d| d.insert_temp(hover, new_hover));
                ui.label(if new_hover.0 > 0 { format!("{} × {} Table", new_hover.1, new_hover.0) } else { "Pick a size".into() });
                if let Some((r, c)) = picked {
                    self.insert_table(r, c);
                    ui.close();
                }
            });
        });
        group(ui, "Illustrations", |ui| {
            if big_btn(ui, "🖼", "Pictures", "Insert a picture from a file").clicked() {
                self.insert_picture();
            }
        });
        group(ui, "Links", |ui| {
            if big_btn(ui, "🔗", "Link", "Insert a hyperlink (Ctrl+K)").clicked() {
                self.open_link_dialog();
            }
        });
        if let Some(id) = self.selected_image() {
            group(ui, "Picture Size", |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        for pct in [25.0, 50.0, 75.0, 100.0] {
                            if ui.button(format!("{pct}%")).clicked() {
                                self.scale_image(id, pct / 100.0);
                            }
                        }
                    });
                    if ui.button("Fit to page width").clicked() {
                        self.scale_image(id, 100.0);
                    }
                    ui.label(RichText::new("Tip: drag the corner handle to resize").size(11.0).weak());
                });
            });
            group(ui, "Text Wrap", |ui| {
                let anchor = self.doc.images.get(&id).and_then(|i| i.anchor);
                let opts: [(&str, Option<crate::doc::Wrap>); 3] = [
                    ("Inline", None),
                    ("Square", Some(crate::doc::Wrap::Side)),
                    ("Top & Bottom", Some(crate::doc::Wrap::Clear)),
                ];
                ui.horizontal(|ui| {
                    for (label, mode) in opts {
                        let selected = match mode {
                            None => anchor.is_none(),
                            Some(w) => anchor.is_some_and(|a| a.wrap == w && !a.behind),
                        };
                        if ui.selectable_label(selected, label).clicked() {
                            self.set_image_wrap(id, mode);
                        }
                    }
                });
                ui.label(RichText::new("Square flows the text at the picture's sides").size(11.0).weak());
                let mut d = anchor.map(|a| a.dist).unwrap_or([0.0; 4]);
                let r = ui.button("Margins…");
                egui::Popup::menu(&r).show(|ui| {
                    ui.set_min_width(150.0);
                    if wrap_margins_menu(ui, &mut d) {
                        self.set_image_margins(id, d[0], d[1], d[2], d[3]);
                    }
                });
            });
        }
        group(ui, "Footnotes", |ui| {
            if big_btn(ui, "¹", "Footnote", "Insert a footnote at the cursor (Ctrl+Alt+F)").clicked() {
                self.insert_note(false);
            }
            if big_btn(ui, "ⁱ", "Endnote", "Insert an endnote at the cursor (Ctrl+Alt+D)").clicked() {
                self.insert_note(true);
            }
        });
        group(ui, "Header & Footer", |ui| {
            let page = self.caret_page();
            if big_btn(ui, "▔", "Header", "Edit the header (or double-click the top of a page)").clicked() {
                self.open_hf(page, true, None);
            }
            if big_btn(ui, "▁", "Footer", "Edit the footer (or double-click the bottom of a page)").clicked() {
                self.open_hf(page, false, None);
            }
            if self.story.is_some() {
                if big_btn(ui, "#", "Page Number", "Insert the page number at the cursor").clicked() {
                    self.insert_page_field(false);
                }
            } else {
                if big_btn_sel(ui, "#", "Page Number", self.doc.page_numbers, "Show page numbers in the footer").clicked() {
                    self.checkpoint(crate::app::EditKind::Other);
                    self.doc.page_numbers = !self.doc.page_numbers;
                    self.doc.clear_rich_hf(false);
                }
                if small_btn(ui, "▤", "Quick Text…", "Set plain header and footer text").clicked() {
                    self.dlg.header_footer = true;
                }
            }
        });
        group(ui, "Text", |ui| {
            let r = big_btn(ui, "📅", "Date & Time", "Insert the current date or time");
            egui::Popup::menu(&r).show(|ui| {
                let now = chrono::Local::now();
                for f in ["%-m/%-d/%Y", "%A, %B %-d, %Y", "%B %-d, %Y", "%Y-%m-%d", "%d %B %Y", "%-I:%M %p", "%H:%M", "%-m/%-d/%Y %-I:%M %p"] {
                    if ui.button(now.format(f).to_string()).clicked() {
                        self.insert_date(f);
                        ui.close();
                    }
                }
            });
        });
        group(ui, "Symbols", |ui| {
            let r = big_btn(ui, "Ω", "Symbol", "Insert a symbol");
            egui::Popup::menu(&r).show(|ui| {
                egui::Grid::new("quick_symbols").show(ui, |ui| {
                    for (i, s) in ["©", "®", "™", "€", "£", "¥", "§", "°", "±", "≠", "≤", "≥", "÷", "×", "∞", "µ", "α", "β", "π", "Ω", "—", "–", "…", "•"].iter().enumerate() {
                        if ui.button(RichText::new(*s).size(16.0)).clicked() {
                            self.insert_typed(s);
                            ui.close();
                        }
                        if i % 8 == 7 {
                            ui.end_row();
                        }
                    }
                });
                if ui.button("More Symbols…").clicked() {
                    self.dlg.symbols = true;
                    ui.close();
                }
            });
        });
    }

    /// The merged Layout & View panel: document layout first, then the
    /// view/theme/zoom controls.
    fn layout_view_tab(&mut self, ui: &mut Ui) {
        self.layout_tab(ui);
        self.view_tab(ui);
    }

    fn layout_tab(&mut self, ui: &mut Ui) {
        group(ui, "Page Setup", |ui| {
            let r = big_btn(ui, "▣", "Margins", "Select the margin sizes");
            egui::Popup::menu(&r).show(|ui| {
                for (name, m) in [
                    ("Normal  (1\" all sides)", [72.0, 72.0, 72.0, 72.0]),
                    ("Narrow  (0.5\" all sides)", [36.0; 4]),
                    ("Moderate  (1\" × 0.75\")", [72.0, 54.0, 72.0, 54.0]),
                    ("Wide  (1\" × 2\")", [72.0, 144.0, 72.0, 144.0]),
                ] {
                    if ui.button(name).clicked() {
                        self.checkpoint(crate::app::EditKind::Other);
                        self.doc.page.margins = m;
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Custom Margins…").clicked() {
                    self.dlg.page_setup = true;
                    ui.close();
                }
            });
            let r = big_btn(ui, "⟲", "Orientation", "Portrait or landscape");
            egui::Popup::menu(&r).show(|ui| {
                let pg = self.doc.page.clone();
                let landscape = pg.width > pg.height;
                if ui.selectable_label(!landscape, "Portrait").clicked() && landscape {
                    self.checkpoint(crate::app::EditKind::Other);
                    self.doc.page.width = pg.height;
                    self.doc.page.height = pg.width;
                    ui.close();
                }
                if ui.selectable_label(landscape, "Landscape").clicked() && !landscape {
                    self.checkpoint(crate::app::EditKind::Other);
                    self.doc.page.width = pg.height;
                    self.doc.page.height = pg.width;
                    ui.close();
                }
            });
            let r = big_btn(ui, "▯", "Size", "Choose a paper size");
            egui::Popup::menu(&r).show(|ui| {
                for (name, w, h) in [
                    ("Letter  8.5\" × 11\"", 612.0, 792.0),
                    ("Legal  8.5\" × 14\"", 612.0, 1008.0),
                    ("Executive  7.25\" × 10.5\"", 522.0, 756.0),
                    ("A4  21 cm × 29.7 cm", 595.3, 841.9),
                    ("A5  14.8 cm × 21 cm", 419.5, 595.3),
                ] {
                    if ui.button(name).clicked() {
                        self.checkpoint(crate::app::EditKind::Other);
                        let landscape = self.doc.page.width > self.doc.page.height;
                        let (w, h) = if landscape { (h, w) } else { (w, h) };
                        self.doc.page.width = w;
                        self.doc.page.height = h;
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Custom Size…").clicked() {
                    self.dlg.page_setup = true;
                    ui.close();
                }
            });
        });
        let para = self.current_para().clone();
        group(ui, "Indent", |ui| {
            ui.vertical(|ui| {
                let inch = |ui: &mut Ui, label: &str, v: f32| -> Option<f32> {
                    ui.horizontal(|ui| {
                        ui.add_sized(vec2(42.0, 18.0), egui::Label::new(label));
                        let mut x = v / 72.0;
                        ui.add(egui::DragValue::new(&mut x).range(-5.0..=10.0).speed(0.01).fixed_decimals(2).suffix("\""))
                            .changed()
                            .then_some(x * 72.0)
                    })
                    .inner
                };
                if let Some(v) = inch(ui, "Left:", para.ind_left) {
                    self.apply_para(|p| p.ind_left = v);
                }
                if let Some(v) = inch(ui, "Right:", para.ind_right) {
                    self.apply_para(|p| p.ind_right = v);
                }
                if let Some(v) = inch(ui, "First:", para.ind_first) {
                    self.apply_para(|p| p.ind_first = v);
                }
            });
        });
        group(ui, "Spacing", |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label("Before:");
                    let mut b = para.space_before;
                    if ui.add(egui::DragValue::new(&mut b).range(0.0..=200.0).speed(1.0).suffix(" pt")).changed() {
                        self.apply_para(|p| p.space_before = b);
                    }
                    ui.label("After:");
                    let mut a = para.space_after;
                    if ui.add(egui::DragValue::new(&mut a).range(0.0..=200.0).speed(1.0).suffix(" pt")).changed() {
                        self.apply_para(|p| p.space_after = a);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Line spacing:");
                    let mut ls = para.line_spacing;
                    if para.line_rule == crate::doc::LineRule::Auto {
                        if ui.add(egui::DragValue::new(&mut ls).range(0.5..=5.0).speed(0.05).fixed_decimals(2)).changed() {
                            self.set_line_spacing(ls);
                        }
                    } else {
                        let label = if para.line_rule == crate::doc::LineRule::Exact { "Exactly" } else { "At least" };
                        ui.label(label);
                        if ui.add(egui::DragValue::new(&mut ls).range(1.0..=200.0).speed(0.5).suffix(" pt")).changed() {
                            self.apply_para(|p| p.line_spacing = ls);
                        }
                    }
                });
            });
        });
        if self.in_table().is_some() {
            self.table_group(ui);
        }
    }

    fn table_group(&mut self, ui: &mut Ui) {
        group(ui, "Table", |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if small_btn(ui, "⬆", "Row Above", "Insert a row above").clicked() {
                        self.table_insert_row(false);
                    }
                    if small_btn(ui, "⬇", "Row Below", "Insert a row below").clicked() {
                        self.table_insert_row(true);
                    }
                });
                ui.horizontal(|ui| {
                    if small_btn(ui, "⬅", "Column Left", "Insert a column to the left").clicked() {
                        self.table_insert_col(false);
                    }
                    if small_btn(ui, "➡", "Column Right", "Insert a column to the right").clicked() {
                        self.table_insert_col(true);
                    }
                });
                ui.horizontal(|ui| {
                    let r = small_btn(ui, "✖", "Delete", "Delete rows, columns or the table");
                    egui::Popup::menu(&r).show(|ui| {
                        if ui.button("Delete Row").clicked() {
                            self.table_delete_row();
                            ui.close();
                        }
                        if ui.button("Delete Column").clicked() {
                            self.table_delete_col();
                            ui.close();
                        }
                        if ui.button("Delete Table").clicked() {
                            self.delete_table();
                            ui.close();
                        }
                    });
                    let borders = self.in_table().and_then(|c| self.doc.tables.get(&c.table)).is_some_and(|t| t.borders);
                    if small_btn(ui, "▦", if borders { "No Borders" } else { "All Borders" }, "Toggle table borders").clicked() {
                        self.toggle_table_borders();
                    }
                    let cur_wrap = self.in_table().and_then(|c| self.doc.tables.get(&c.table)).and_then(|t| t.anchor).map(|a| a.wrap);
                    let wrap_label = match cur_wrap {
                        Some(Wrap::Clear) => "Top & Bottom",
                        Some(_) => "Around",
                        None => "In Line",
                    };
                    let r = small_btn(ui, "⤢", wrap_label, "Text wrapping around the table (move it freely)");
                    egui::Popup::menu(&r).show(|ui| {
                        let tid = self.in_table().map(|c| c.table);
                        if let Some(tid) = tid {
                            if ui.selectable_label(cur_wrap.is_none(), "In Line").clicked() {
                                self.set_table_wrap(tid, None);
                                ui.close();
                            }
                            if ui.selectable_label(cur_wrap == Some(Wrap::Side), "Around").clicked() {
                                self.set_table_wrap(tid, Some(Wrap::Side));
                                ui.close();
                            }
                            if ui.selectable_label(cur_wrap == Some(Wrap::Clear), "Top & Bottom").clicked() {
                                self.set_table_wrap(tid, Some(Wrap::Clear));
                                ui.close();
                            }
                        }
                    });
                    if let Some(mut d) = self.in_table().and_then(|c| self.doc.tables.get(&c.table)).and_then(|t| t.anchor).map(|a| a.dist) {
                        let r = small_btn(ui, "↔", "Margins", "Distance between the table and the text");
                        egui::Popup::menu(&r).show(|ui| {
                            ui.set_min_width(150.0);
                            if wrap_margins_menu(ui, &mut d) {
                                if let Some(tid) = self.in_table().map(|c| c.table) {
                                    self.set_table_margins(tid, d[0], d[1], d[2], d[3]);
                                }
                            }
                        });
                    }
                    let r = small_btn(ui, "▩", "Shading", "Cell shading");
                    egui::Popup::menu(&r).show(|ui| {
                        ui.horizontal(|ui| {
                            for c in THEME_COLORS.iter().chain(STANDARD_COLORS.iter()) {
                                if swatch(ui, tint(*c, 0.6), "") {
                                    self.shade_cell(Some(tint(*c, 0.6)));
                                    ui.close();
                                }
                            }
                        });
                        if ui.button("No Color").clicked() {
                            self.shade_cell(None);
                            ui.close();
                        }
                    });
                });
            });
        });
    }

    pub fn open_link_dialog(&mut self) {
        let existing = self.link_at_cursor().map(crate::pool::link);
        self.dlg.link_url = match existing {
            Some((url, anchor)) if url.is_empty() => format!("#{anchor}"),
            Some((url, _)) => url,
            None => String::new(),
        };
        self.dlg.link_text = if self.has_sel() {
            let (a, b) = self.sel();
            self.doc.plain_text(a, b)
        } else {
            String::new()
        };
        self.dlg.link = true;
    }

    fn review_tab(&mut self, ui: &mut Ui) {
        group(ui, "Proofing", |ui| {
            if big_btn(ui, "ABC✓", "Spelling", "Check the spelling of the document (F7)").clicked() {
                self.start_spelling();
            }
            if big_btn(ui, "123", "Word Count", "Find the number of words, characters and paragraphs").clicked() {
                self.dlg.word_count = true;
            }
        });
        group(ui, "Tracking", |ui| {
            let markup = crate::layout::show_markup();
            if big_btn_sel(ui, "✎", "Show Markup", markup, "Show insertions and deletions (otherwise show the final text with change bars)").clicked() {
                self.set_show_markup(!markup);
            }
            let has = self.has_revisions();
            if ui.add_enabled_ui(has, |ui| big_btn(ui, "✔", "Accept All", "Accept all tracked changes")).inner.clicked() {
                self.resolve_all_changes(true);
            }
            if ui.add_enabled_ui(has, |ui| big_btn(ui, "✖", "Reject All", "Reject all tracked changes")).inner.clicked() {
                self.resolve_all_changes(false);
            }
        });
        group(ui, "Comments", |ui| {
            let n = self.doc.comments.len();
            let label = if n == 0 { "No Comments".to_string() } else { format!("Comments ({n})") };
            if big_btn_sel(ui, "💬", &label, self.show_comments && n > 0, "Show comments beside the page").clicked() {
                self.show_comments = !self.show_comments;
            }
        });
        group(ui, "Language", |ui| {
            ui.vertical(|ui| {
                let current = self.spell.active.clone();
                let label = self.spell.active_label();
                egui::ComboBox::from_id_salt("spell_lang").selected_text(label).width(160.0).show_ui(ui, |ui| {
                    for l in self.spell.langs.clone() {
                        if ui.selectable_label(l.code == current, l.label()).clicked() {
                            self.spell.set_language(&l.code);
                            self.settings.set_spell_lang(&l.code);
                            self.settings.save();
                        }
                    }
                });
                if ui.checkbox(&mut self.spell.enabled, "Check spelling as you type").changed() {
                    self.settings.set_spell_enabled(self.spell.enabled);
                    self.settings.save();
                }
            });
        });
        group(ui, "Find", |ui| {
            if big_btn(ui, "🔍", "Find", "Find text (Ctrl+F)").clicked() {
                self.find.open = true;
                self.find.replace = false;
                self.find.focus_query = true;
            }
            if big_btn(ui, "⇄", "Replace", "Replace text (Ctrl+H)").clicked() {
                self.find.open = true;
                self.find.replace = true;
                self.find.focus_query = true;
            }
        });
        group(ui, "Export", |ui| {
            if big_btn(ui, "📕", "PDF", "Export the document as PDF").clicked() {
                self.export_pdf();
            }
        });
    }

    fn view_tab(&mut self, ui: &mut Ui) {
        group(ui, "Show", |ui| {
            ui.vertical(|ui| {
                ui.checkbox(&mut self.show_ruler, "Ruler");
                ui.checkbox(&mut self.show_marks, "Formatting marks");
                if ui.checkbox(&mut self.show_nav, "Navigation pane").changed() {
                    self.settings.show_nav = self.show_nav;
                    self.settings.save();
                }
            });
        });
        group(ui, "Backup", |ui| {
            ui.vertical(|ui| {
                if ui.checkbox(&mut self.settings.autosave_enabled, "Autosave recovery copy").changed() {
                    self.settings.save();
                }
                ui.add_enabled_ui(self.settings.autosave_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Every");
                        if ui
                            .add(egui::DragValue::new(&mut self.settings.autosave_secs).range(10..=600).suffix(" s"))
                            .changed()
                        {
                            self.settings.save();
                        }
                    });
                });
            });
        });
        group(ui, "Theme", |ui| {
            ui.vertical(|ui| {
                let current = self.theme.name;
                if ui.selectable_label(current == ThemeName::Light, "Light").clicked() && current != ThemeName::Light {
                    let ctx = ui.ctx().clone();
                    self.choose_theme(&ctx, Theme::light());
                }
                if ui.selectable_label(current == ThemeName::Dark, "Dark").clicked() && current != ThemeName::Dark {
                    let ctx = ui.ctx().clone();
                    self.choose_theme(&ctx, Theme::dark());
                }
                if ui.selectable_label(current == ThemeName::Custom, "Custom").clicked() && current != ThemeName::Custom {
                    let ctx = ui.ctx().clone();
                    self.choose_custom_theme(&ctx);
                }
                if ui.button("Edit Colours…").clicked() {
                    self.open_custom_theme_dialog();
                }
            });
        });
        group(ui, "Zoom", |ui| {
            if big_btn(ui, "🔍", "Zoom", "Zoom in").clicked() {
                self.zoom = (self.zoom * 1.1).min(5.0);
            }
            if big_btn(ui, "1:1", "100%", "Zoom to 100%").clicked() {
                self.zoom = 1.0;
            }
            if big_btn(ui, "▯", "One Page", "Fit a whole page in the window").clicked() {
                self.fit_request = Some(false);
            }
            if big_btn(ui, "↔", "Page Width", "Fit the page width to the window").clicked() {
                self.fit_request = Some(true);
            }
        });
    }

    // ================================================================ navigation

    fn nav_panel(&mut self, ui: &mut Ui) {
        let t = self.theme.tokens;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Navigation").color(t.chrome_fg));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let close = ui.small_button("x");
                if show_tip(ui, close, "Close the navigation pane").clicked() {
                    self.show_nav = false;
                    self.settings.show_nav = false;
                    self.settings.save();
                }
            });
        });
        ui.separator();
        if self.nav_cache.as_ref().is_none_or(|(rev, _)| *rev != self.rev) {
            self.nav_cache = Some((self.rev, self.doc.headings()));
        }
        let mut jump = None;
        let current = self.cursor.p;
        {
            let headings = &self.nav_cache.as_ref().unwrap().1;
            if headings.is_empty() {
                ui.add_space(4.0);
                ui.weak("No headings. Apply Heading 1–3 to build an outline.");
            } else {
                let active = headings.iter().rposition(|h| h.para <= current);
                egui::ScrollArea::vertical().id_salt("nav_headings").auto_shrink([false, false]).show(ui, |ui| {
                    for (i, h) in headings.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add_space(f32::from(h.level.saturating_sub(1)) * 12.0);
                            let selected = Some(i) == active;
                            let color = if selected { t.chrome_selection_fg } else { t.chrome_fg };
                            let text = RichText::new(&h.text).color(color);
                            if ui.selectable_label(selected, text).clicked() {
                                jump = Some(Pos::new(h.para, 0));
                            }
                        });
                    }
                });
            }
        }
        if let Some(pos) = jump {
            self.set_cursor(pos, false);
        }
    }

    // ================================================================ status bar

    fn status_bar(&mut self, ui: &mut Ui) {
        let fg = self.theme.tokens.status_fg;
        ui.horizontal(|ui| {
            ui.visuals_mut().override_text_color = Some(fg);
            let (pages, page) = match &self.layout {
                Some(l) => (l.pages, l.caret(self.doc.clamp(self.cursor)).0 + 1),
                None => (1, 1),
            };
            ui.label(format!("Page {page} of {pages}"));
            ui.add_space(12.0);
            let st = self.main_doc().stats();
            if self.has_sel() {
                let (a, b) = self.sel();
                let words = self.doc.plain_text(a, b).split_whitespace().count();
                ui.label(format!("{words} of {} words", st.words));
            } else {
                ui.label(format!("{} words", st.words));
            }
            if let Some((msg, t)) = &self.status {
                if t.elapsed().as_secs() < 6 {
                    ui.add_space(12.0);
                    ui.label(msg);
                }
            }
            ui.add_space(12.0);
            let lang = self.spell.active_label();
            let spell_resp = ui.add(egui::Label::new(RichText::new(format!("📖 {lang}")).color(fg)).sense(Sense::click()));
            if show_tip(ui, spell_resp, "Check spelling (F7)").clicked() {
                self.start_spelling();
            }
            ui.add_space(12.0);
            let version_resp = ui.add(egui::Label::new(RichText::new(format!("version {VERSION}")).color(fg)).sense(Sense::hover()));
            show_tip(ui, version_resp, VERSION_NOTES);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("{:.0}%", self.zoom * 100.0));
                if ui.add(egui::Button::new("+").frame(false)).clicked() {
                    self.zoom = ((self.zoom * 10.0).round() / 10.0 + 0.1).min(5.0);
                }
                let mut z = self.zoom;
                ui.spacing_mut().slider_width = 110.0;
                if ui.add(egui::Slider::new(&mut z, 0.1..=5.0).logarithmic(true).show_value(false)).changed() {
                    self.zoom = z;
                }
                if ui.add(egui::Button::new("−").frame(false)).clicked() {
                    self.zoom = ((self.zoom * 10.0).round() / 10.0 - 0.1).max(0.1);
                }
            });
        });
    }

    // ================================================================ dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        // Crash-recovery prompt.
        if self.show_recovery {
            if self.recovery_items.is_empty() {
                self.show_recovery = false;
            } else {
                let entries = self.recovery_items.clone();
                egui::Modal::new(egui::Id::new("recovery")).show(ctx, |ui| {
                    ui.set_width(540.0);
                    ui.heading("Recover Unsaved Documents");
                    ui.add_space(4.0);
                    ui.label("MiniPad found recovery copies from a session that did not close normally.");
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                        for (i, e) in entries.iter().enumerate() {
                            Frame::group(ui.style()).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let original = e
                                        .meta
                                        .original
                                        .as_ref()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|| "Untitled document".into());
                                    ui.vertical(|ui| {
                                        ui.strong(&e.meta.title);
                                        ui.small(original);
                                        ui.small(format!("Saved {}", e.saved_at_text()));
                                    });
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.button("Discard").clicked() {
                                            self.discard_recovery(i);
                                        }
                                        if ui.button("Restore").clicked() {
                                            self.restore_recovery(i);
                                        }
                                    });
                                });
                            });
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Discard All").clicked() {
                            self.discard_all_recovery();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Later").clicked() {
                                self.show_recovery = false;
                            }
                        });
                    });
                });
            }
        }

        // Unsaved-changes prompt.
        if let Some(action) = self.confirm {
            let mut choice = None;
            egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading("MiniPad");
                ui.add_space(6.0);
                ui.label(format!("Do you want to save changes to {}?", self.title()));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("  Save  ").clicked() {
                        choice = Some(0);
                    }
                    if ui.button("Don't Save").clicked() {
                        choice = Some(1);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(2);
                    }
                });
            });
            match choice {
                Some(0) => {
                    self.confirm = None;
                    if self.save() {
                        self.run_after(action);
                    }
                }
                Some(1) => {
                    self.confirm = None;
                    self.dirty = false;
                    self.drop_recovery();
                    self.run_after(action);
                }
                Some(2) => self.confirm = None,
                _ => {}
            }
        }

        if let Some(msg) = self.dlg.error.clone() {
            let r = egui::Modal::new(egui::Id::new("error")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading("MiniPad");
                ui.add_space(6.0);
                ui.label(&msg);
                ui.add_space(10.0);
                ui.button("OK").clicked()
            });
            if r.inner || r.should_close() {
                self.dlg.error = None;
            }
        }

        self.find_dialog(ctx);

        if self.dlg.word_count {
            let st = self.doc.stats();
            let pages = self.layout.as_ref().map(|l| l.pages).unwrap_or(1);
            let mut open = true;
            egui::Window::new("Word Count").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                egui::Grid::new("wc").num_columns(2).spacing(vec2(30.0, 6.0)).show(ui, |ui| {
                    for (k, v) in [
                        ("Pages", pages),
                        ("Words", st.words),
                        ("Characters (no spaces)", st.chars_no_spaces),
                        ("Characters (with spaces)", st.chars),
                        ("Paragraphs", st.paragraphs),
                    ] {
                        ui.label(k);
                        ui.label(RichText::new(v.to_string()).color(theme::tokens_for(ui).dialog_fg));
                        ui.end_row();
                    }
                });
            });
            self.dlg.word_count = open;
        }

        if self.dlg.symbols {
            let mut open = true;
            let mut insert = None;
            egui::Window::new("Symbol").open(&mut open).collapsible(false).default_width(420.0).show(ctx, |ui| {
                let ranges: [(&str, std::ops::RangeInclusive<u32>); 6] = [
                    ("Latin-1", 0xA1..=0xFF),
                    ("Greek", 0x391..=0x3C9),
                    ("Punctuation", 0x2010..=0x2044),
                    ("Currency", 0x20A0..=0x20BF),
                    ("Arrows", 0x2190..=0x21FF),
                    ("Math", 0x2200..=0x22FF),
                ];
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for (name, range) in ranges {
                        ui.label(RichText::new(name).color(theme::tokens_for(ui).dialog_fg));
                        ui.horizontal_wrapped(|ui| {
                            for cp in range {
                                if let Some(c) = char::from_u32(cp) {
                                    if c.is_control() || c.is_whitespace() {
                                        continue;
                                    }
                                    let b = egui::Button::new(RichText::new(c.to_string()).size(16.0)).min_size(vec2(28.0, 28.0));
                                    let resp = ui.add(b);
                                    if show_tip(ui, resp, format!("U+{cp:04X}")).clicked() {
                                        insert = Some(c);
                                    }
                                }
                            }
                        });
                    }
                });
            });
            if let Some(c) = insert {
                self.insert_typed(&c.to_string());
            }
            self.dlg.symbols = open;
        }

        if self.dlg.header_footer {
            let mut open = true;
            let mut header = self.doc.header.clone();
            let mut footer = self.doc.footer.clone();
            let mut nums = self.doc.page_numbers;
            egui::Window::new("Header & Footer").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label("Header text:");
                let r = ui.add(egui::TextEdit::singleline(&mut header).desired_width(320.0));
                self.track(&r);
                ui.label("Footer text:");
                let r = ui.add(egui::TextEdit::singleline(&mut footer).desired_width(320.0));
                self.track(&r);
                ui.checkbox(&mut nums, "Show page numbers (Page X of Y)");
            });
            if header != self.doc.header || footer != self.doc.footer || nums != self.doc.page_numbers {
                self.checkpoint(crate::app::EditKind::Typing);
                // Plain text replaces rich headers/footers.
                if header != self.doc.header {
                    self.doc.clear_rich_hf(true);
                }
                if footer != self.doc.footer || nums != self.doc.page_numbers {
                    self.doc.clear_rich_hf(false);
                }
                self.doc.header = header;
                self.doc.footer = footer;
                self.doc.page_numbers = nums;
            }
            self.dlg.header_footer = open;
        }

        if self.dlg.page_setup {
            let mut open = true;
            let sec = self.doc.section_at_para(self.cursor.p);
            let setups = self.doc.section_setups();
            let total = setups.len().max(1);
            let mut pg = setups.get(sec).cloned().unwrap_or_else(|| self.doc.page.clone());
            let before = pg.clone();
            let mut apply_all = self.dlg.page_setup_all;
            egui::Window::new("Page Setup").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                let inch = |ui: &mut Ui, label: &str, v: &mut f32, max: f32| {
                    ui.label(label);
                    let mut i = *v / 72.0;
                    if ui.add(egui::DragValue::new(&mut i).range(0.0..=max).speed(0.01).fixed_decimals(2).suffix("\"")).changed() {
                        *v = i * 72.0;
                    }
                };
                ui.label(format!("Section {} of {total}", sec + 1));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut apply_all, false, "This section");
                    ui.radio_value(&mut apply_all, true, "All sections");
                });
                ui.separator();
                ui.label(RichText::new("Margins").color(theme::tokens_for(ui).dialog_fg));
                egui::Grid::new("margins").num_columns(4).show(ui, |ui| {
                    inch(ui, "Top:", &mut pg.margins[0], 5.0);
                    inch(ui, "Bottom:", &mut pg.margins[2], 5.0);
                    ui.end_row();
                    inch(ui, "Left:", &mut pg.margins[3], 5.0);
                    inch(ui, "Right:", &mut pg.margins[1], 5.0);
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Paper size").color(theme::tokens_for(ui).dialog_fg));
                egui::Grid::new("paper").num_columns(4).show(ui, |ui| {
                    inch(ui, "Width:", &mut pg.width, 22.0);
                    inch(ui, "Height:", &mut pg.height, 22.0);
                    ui.end_row();
                });
                let mut landscape = pg.width > pg.height;
                if ui.checkbox(&mut landscape, "Landscape").changed() && landscape != (pg.width > pg.height) {
                    std::mem::swap(&mut pg.width, &mut pg.height);
                }
                ui.add_space(6.0);
                ui.label(RichText::new("From edge").color(theme::tokens_for(ui).dialog_fg));
                egui::Grid::new("hfdist").num_columns(4).show(ui, |ui| {
                    inch(ui, "Header:", &mut pg.header_dist, 5.0);
                    inch(ui, "Footer:", &mut pg.footer_dist, 5.0);
                    ui.end_row();
                });
            });
            pg.width = pg.width.max(144.0);
            pg.height = pg.height.max(144.0);
            pg.margins[1] = pg.margins[1].min(pg.width / 2.0 - 36.0).max(0.0);
            pg.margins[3] = pg.margins[3].min(pg.width / 2.0 - 36.0).max(0.0);
            pg.margins[0] = pg.margins[0].min(pg.height / 2.0 - 36.0).max(0.0);
            pg.margins[2] = pg.margins[2].min(pg.height / 2.0 - 36.0).max(0.0);
            self.dlg.page_setup_all = apply_all;
            if pg != before {
                self.checkpoint(crate::app::EditKind::Typing);
                if apply_all {
                    for s in 0..total {
                        crate::docx::set_section_setup(&mut self.doc, s, &pg);
                    }
                } else {
                    crate::docx::set_section_setup(&mut self.doc, sec, &pg);
                }
            }
            self.dlg.page_setup = open;
        }

        if self.dlg.paragraph {
            let mut open = true;
            let mut p = self.current_para().clone();
            let before = p.clone();
            egui::Window::new("Paragraph").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                egui::Grid::new("para").num_columns(2).spacing(vec2(16.0, 6.0)).show(ui, |ui| {
                    ui.label("Alignment:");
                    egui::ComboBox::from_id_salt("pa").selected_text(format!("{:?}", p.align)).show_ui(ui, |ui| {
                        for a in [Align::Left, Align::Center, Align::Right, Align::Justify] {
                            ui.selectable_value(&mut p.align, a, format!("{a:?}"));
                        }
                    });
                    ui.end_row();
                    let inch = |ui: &mut Ui, v: &mut f32| {
                        let mut x = *v / 72.0;
                        if ui.add(egui::DragValue::new(&mut x).range(-5.0..=10.0).speed(0.01).fixed_decimals(2).suffix("\"")).changed() {
                            *v = x * 72.0;
                        }
                    };
                    ui.label("Indent left:");
                    inch(ui, &mut p.ind_left);
                    ui.end_row();
                    ui.label("Indent right:");
                    inch(ui, &mut p.ind_right);
                    ui.end_row();
                    ui.label("First line (− = hanging):");
                    inch(ui, &mut p.ind_first);
                    ui.end_row();
                    ui.label("Spacing before:");
                    ui.add(egui::DragValue::new(&mut p.space_before).range(0.0..=200.0).suffix(" pt"));
                    ui.end_row();
                    ui.label("Spacing after:");
                    ui.add(egui::DragValue::new(&mut p.space_after).range(0.0..=200.0).suffix(" pt"));
                    ui.end_row();
                    ui.label("Line spacing:");
                    ui.horizontal(|ui| {
                        use crate::doc::LineRule;
                        let prev_rule = p.line_rule;
                        egui::ComboBox::from_id_salt("lr").selected_text(match p.line_rule {
                            LineRule::Auto => "Multiple",
                            LineRule::AtLeast => "At least",
                            LineRule::Exact => "Exactly",
                        }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut p.line_rule, LineRule::Auto, "Multiple");
                            ui.selectable_value(&mut p.line_rule, LineRule::AtLeast, "At least");
                            ui.selectable_value(&mut p.line_rule, LineRule::Exact, "Exactly");
                        });
                        if p.line_rule != prev_rule {
                            p.line_spacing = if p.line_rule == LineRule::Auto { 1.0 } else { 12.0 };
                        }
                        if p.line_rule == LineRule::Auto {
                            ui.add(egui::DragValue::new(&mut p.line_spacing).range(0.5..=5.0).speed(0.05).fixed_decimals(2));
                        } else {
                            ui.add(egui::DragValue::new(&mut p.line_spacing).range(1.0..=200.0).suffix(" pt"));
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut p.contextual, "Don't add space between paragraphs of the same style");
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut p.page_break_before, "Page break before");
                    ui.end_row();
                });
                ui.separator();
                ui.label(RichText::new("Tab stops (from the left margin)").color(theme::tokens_for(ui).dialog_fg));
                let kind_label = |k: TabKind| match k {
                    TabKind::Left => "Left",
                    TabKind::Center => "Center",
                    TabKind::Right => "Right",
                    TabKind::Decimal => "Decimal",
                    TabKind::Bar => "Bar",
                    TabKind::Clear => "Clear",
                };
                let leader_label = |l: Leader| match l {
                    Leader::None => "None",
                    Leader::Dot => "Dots",
                    Leader::Hyphen => "Hyphens",
                    Leader::Underscore => "Underline",
                };
                let mut remove = None;
                for i in 0..p.tabs.len() {
                    ui.horizontal(|ui| {
                        let t = &mut p.tabs[i];
                        let mut pos = t.pos / 72.0;
                        if ui
                            .add(egui::DragValue::new(&mut pos).range(0.0..=10.0).speed(0.01).fixed_decimals(2).suffix("\""))
                            .changed()
                        {
                            t.pos = pos * 72.0;
                        }
                        egui::ComboBox::from_id_salt(("tabk", i))
                            .selected_text(kind_label(t.kind))
                            .width(78.0)
                            .show_ui(ui, |ui| {
                                for k in [TabKind::Left, TabKind::Center, TabKind::Right, TabKind::Decimal, TabKind::Bar] {
                                    ui.selectable_value(&mut t.kind, k, kind_label(k));
                                }
                            });
                        egui::ComboBox::from_id_salt(("tabl", i))
                            .selected_text(leader_label(t.leader))
                            .width(88.0)
                            .show_ui(ui, |ui| {
                                for l in [Leader::None, Leader::Dot, Leader::Hyphen, Leader::Underscore] {
                                    ui.selectable_value(&mut t.leader, l, leader_label(l));
                                }
                            });
                        let rm = ui.button("✕");
                        if show_tip(ui, rm, "Remove tab stop").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    p.tabs.remove(i);
                }
                if ui.button("＋ Add tab stop").clicked() {
                    let pos = p.tabs.last().map(|t| t.pos + 36.0).unwrap_or(p.ind_left + 36.0);
                    p.tabs.push(TabStop { pos, kind: TabKind::Left, leader: Leader::None });
                }
                p.tabs.sort_by(|a, b| a.pos.total_cmp(&b.pos));
            });
            let changed = p.align != before.align
                || p.ind_left != before.ind_left
                || p.ind_right != before.ind_right
                || p.ind_first != before.ind_first
                || p.space_before != before.space_before
                || p.space_after != before.space_after
                || p.line_spacing != before.line_spacing
                || p.line_rule != before.line_rule
                || p.contextual != before.contextual
                || p.page_break_before != before.page_break_before
                || p.tabs != before.tabs;
            if changed {
                self.apply_para(|q| {
                    q.align = p.align;
                    q.ind_left = p.ind_left;
                    q.ind_right = p.ind_right;
                    q.ind_first = p.ind_first;
                    q.space_before = p.space_before;
                    q.space_after = p.space_after;
                    q.line_spacing = p.line_spacing;
                    q.line_rule = p.line_rule;
                    q.contextual = p.contextual;
                    q.page_break_before = p.page_break_before;
                    q.tabs = p.tabs.clone();
                });
            }
            self.dlg.paragraph = open;
        }

        if self.dlg.spelling {
            self.spelling_dialog(ctx);
        }

        if self.dlg.link {
            let mut open = true;
            let mut apply = false;
            let mut remove = false;
            let editing_existing = self.link_at_cursor().is_some();
            egui::Window::new("Insert Hyperlink").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                egui::Grid::new("link").num_columns(2).show(ui, |ui| {
                    ui.label("Text to display:");
                    let r = ui.add_enabled(!self.has_sel(), egui::TextEdit::singleline(&mut self.dlg.link_text).desired_width(300.0));
                    self.track(&r);
                    ui.end_row();
                    ui.label("Address:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.dlg.link_url).desired_width(300.0).hint_text("https://… or #bookmark"));
                    self.track(&r);
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        apply = true;
                    }
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        apply = true;
                    }
                    if editing_existing && ui.button("Remove Link").clicked() {
                        remove = true;
                    }
                });
            });
            if apply {
                let (url, text) = (self.dlg.link_url.clone(), self.dlg.link_text.clone());
                self.insert_link(&url, &text);
                open = false;
            }
            if remove {
                self.remove_link();
                open = false;
            }
            self.dlg.link = open;
        }

        if self.dlg.about {
            let mut open = true;
            egui::Window::new("About MiniPad").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.heading("MiniPad");
                ui.label("A word processor written in Rust with egui.");
                ui.label("Reads and writes .docx, exports PDF, HTML and plain text.");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            });
            self.dlg.about = open;
        }

        if self.dlg.shortcuts {
            let mut open = true;
            egui::Window::new("Keyboard Shortcuts").open(&mut open).collapsible(false).show(ctx, |ui| {
                egui::Grid::new("keys").striped(true).show(ui, |ui| {
                    for (k, v) in [
                        ("Ctrl+N / O / S", "New / Open / Save"),
                        ("Ctrl+Shift+S", "Save As"),
                        ("Ctrl+P", "Print (via PDF)"),
                        ("Ctrl+Z / Ctrl+Y", "Undo / Redo"),
                        ("Ctrl+X / C / V", "Cut / Copy / Paste"),
                        ("Ctrl+Shift+V", "Paste as plain text"),
                        ("Ctrl+A", "Select all"),
                        ("Ctrl+B / I / U", "Bold / Italic / Underline"),
                        ("Ctrl+Shift+D", "Strikethrough"),
                        ("Ctrl+= / Ctrl+Shift+=", "Subscript / Superscript"),
                        ("Ctrl+[ / Ctrl+]", "Shrink / Grow font"),
                        ("Ctrl+Space", "Clear character formatting"),
                        ("Ctrl+L / E / R / J", "Align left / center / right / justify"),
                        ("Ctrl+1 / 2 / 5", "Line spacing 1 / 2 / 1.5"),
                        ("Ctrl+Alt+1 / 2 / 3", "Heading 1 / 2 / 3"),
                        ("Ctrl+Shift+N", "Normal style"),
                        ("Ctrl+Shift+L", "Bulleted list"),
                        ("Ctrl+M / Ctrl+Shift+M", "Indent / Outdent"),
                        ("Tab / Shift+Tab", "Indent list item"),
                        ("Ctrl+Enter", "Page break"),
                        ("Shift+Enter", "Line break"),
                        ("Ctrl+F / Ctrl+H", "Find / Replace"),
                        ("Ctrl+Mouse wheel", "Zoom"),
                    ] {
                        ui.label(RichText::new(k).color(theme::tokens_for(ui).dialog_fg));
                        ui.label(v);
                        ui.end_row();
                    }
                });
            });
            self.dlg.shortcuts = open;
        }

        self.custom_theme_dialog(ctx);
    }

    /// Colour editor for the Custom theme. Changes preview live; `Apply` stores
    /// the palette and activates Custom, `Cancel` restores the previous theme.
    fn custom_theme_dialog(&mut self, ctx: &egui::Context) {
        if !self.dlg.custom_theme {
            return;
        }
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut changed = false;

        egui::Window::new("Custom Theme")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.label(
                    "Choose the main colours. The remaining shades are derived automatically so text stays readable.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Start from Light").clicked() {
                        self.dlg.custom_palette = CustomPalette::from_tokens(&theme::light());
                        changed = true;
                    }
                    if ui.button("Start from Dark").clicked() {
                        self.dlg.custom_palette = CustomPalette::from_tokens(&theme::dark());
                        changed = true;
                    }
                });
                ui.separator();

                // Small live preview of the three surfaces.
                let preview = self.dlg.custom_palette.tokens();
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 92.0), Sense::hover());
                let p = ui.painter();
                let ribbon = Rect::from_min_size(r.min, vec2(r.width(), 26.0));
                p.rect_filled(ribbon, 2.0, preview.chrome_bg);
                p.rect_filled(
                    Rect::from_min_size(ribbon.min, vec2(ribbon.width(), 8.0)),
                    2.0,
                    preview.chrome_bg_alt,
                );
                p.rect_filled(
                    Rect::from_min_size(pos2(ribbon.left() + 8.0, ribbon.top() + 12.0), vec2(34.0, 6.0)),
                    1.0,
                    preview.chrome_accent,
                );
                p.text(
                    pos2(ribbon.left() + 50.0, ribbon.center().y + 3.0),
                    Align2::LEFT_CENTER,
                    "Ribbon",
                    FontId::proportional(11.0),
                    preview.chrome_fg,
                );
                let desk = Rect::from_min_max(
                    pos2(r.left(), ribbon.bottom()),
                    pos2(r.right(), r.bottom() - 18.0),
                );
                p.rect_filled(desk, 0.0, preview.desk_bg);
                let page = Rect::from_center_size(desk.center(), vec2(150.0, desk.height() - 8.0));
                p.rect_filled(page, 0.0, preview.sheet_bg);
                p.rect_stroke(
                    page,
                    0.0,
                    Stroke::new(1.0, preview.sheet_border),
                    egui::StrokeKind::Inside,
                );
                p.text(
                    page.center(),
                    Align2::CENTER_CENTER,
                    "Page",
                    FontId::proportional(11.0),
                    preview.sheet_fg,
                );
                let status = Rect::from_min_max(pos2(r.left(), r.bottom() - 18.0), r.max);
                p.rect_filled(status, 0.0, preview.status_bg);
                p.text(
                    pos2(status.left() + 8.0, status.center().y),
                    Align2::LEFT_CENTER,
                    "Status bar",
                    FontId::proportional(11.0),
                    preview.status_fg,
                );
                ui.separator();

                egui::ScrollArea::vertical().max_height(390.0).show(ui, |ui| {
                    let mut group = "";
                    for f in theme::CUSTOM_FIELDS {
                        if f.group != group {
                            group = f.group;
                            ui.add_space(6.0);
                            ui.label(RichText::new(group).color(theme::tokens_for(ui).dialog_fg));
                        }
                        let mut c = self.dlg.custom_palette.get(f.key).unwrap_or(Color32::BLACK);
                        ui.horizontal(|ui| {
                            ui.label(f.label);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if egui::color_picker::color_edit_button_srgba(
                                        ui,
                                        &mut c,
                                        egui::color_picker::Alpha::Opaque,
                                    )
                                    .changed()
                                    {
                                        self.dlg.custom_palette.set(f.key, c);
                                        changed = true;
                                    }
                                    ui.label(
                                        RichText::new(format!(
                                            "#{:02X}{:02X}{:02X}",
                                            c.r(),
                                            c.g(),
                                            c.b()
                                        ))
                                        .monospace()
                                        .size(11.0),
                                    );
                                },
                            );
                        });
                    }
                });

                let t = self.dlg.custom_palette.tokens();
                for (name, fg, bg) in [
                    ("Ribbon text", t.chrome_fg, t.chrome_bg),
                    ("Accent text", t.chrome_accent_fg, t.chrome_accent),
                    ("List selection text", t.chrome_selection_fg, t.chrome_selection_bg),
                    ("Page text", t.sheet_fg, t.sheet_bg),
                    ("Dialog text", t.dialog_fg, t.dialog_bg),
                ] {
                    let ratio = theme::contrast_ratio(fg, bg);
                    if ratio < 4.5 {
                        ui.colored_label(
                            t.error,
                            format!("{name}: contrast {ratio:.1}:1 (WCAG AA needs 4.5:1)"),
                        );
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if changed && !apply && !cancel {
            let theme = Theme::custom(&self.dlg.custom_palette);
            self.theme = theme;
            theme::apply_theme(ctx, &theme);
        }
        if apply || (!open && !cancel) {
            let palette = self.dlg.custom_palette;
            self.settings.set_custom_theme(palette);
            self.settings.set_theme(ThemeName::Custom);
            self.settings.save();
            let theme = Theme::custom(&palette);
            self.theme = theme;
            theme::apply_theme(ctx, &theme);
            self.dlg.custom_theme = false;
            self.dlg.custom_before = None;
        } else if cancel {
            if let Some(before) = self.dlg.custom_before {
                self.theme = before;
                theme::apply_theme(ctx, &before);
            }
            self.dlg.custom_theme = false;
            self.dlg.custom_before = None;
        }
    }

    fn spelling_dialog(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut action: Option<&str> = None;
        let item = self.dlg.spell_item.clone();
        let t = self.theme.tokens;
        egui::Window::new(format!("Spelling: {}", self.spell.active_label()))
            .id(egui::Id::new("spelling_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .default_pos(Pos2::new(ctx.content_rect().right() - 400.0, 160.0))
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(t.dialog_fg);
                if !self.spell.is_ready() {
                    ui.label("Loading dictionary…");
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                let Some((a, b, sugg)) = &item else {
                    ui.label(if self.dlg.spell_done { "The spelling check is complete." } else { "No spelling errors found." });
                    if ui.button("Close").clicked() {
                        action = Some("close");
                    }
                    return;
                };
                let word = self.doc.plain_text(*a, *b);
                // Show the word in its sentence.
                let para = &self.doc.paras[a.p];
                let s = a.o.saturating_sub(40);
                let e = (b.o + 40).min(para.len());
                let before: String = para.chars[s..a.o].iter().filter(|c| **c != crate::doc::OBJ).collect();
                let after: String = para.chars[b.o..e].iter().filter(|c| **c != crate::doc::OBJ).collect();
                ui.label("Not in Dictionary:");
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(330.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(format!("{}{before}", if s > 0 { "…" } else { "" }));
                        ui.label(RichText::new(&word).color(self.theme.tokens.error));
                        ui.label(format!("{after}{}", if e < para.len() { "…" } else { "" }));
                    });
                });
                ui.add_space(4.0);
                ui.label("Suggestions:");
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(330.0);
                    ui.set_min_height(110.0);
                    if sugg.is_empty() {
                        ui.label(RichText::new("(no suggestions)").italics().color(t.dialog_fg));
                    }
                    for (i, s) in sugg.iter().enumerate() {
                        let selected = self.dlg.spell_choice == i;
                        let color = if selected { t.chrome_selection_fg } else { t.dialog_fg };
                        let r = ui.selectable_label(selected, RichText::new(s).color(color));
                        if r.clicked() {
                            self.dlg.spell_choice = i;
                        }
                        if r.double_clicked() {
                            self.dlg.spell_choice = i;
                            action = Some("change");
                        }
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Ignore Once").clicked() {
                        action = Some("ignore");
                    }
                    if ui.button("Ignore All").clicked() {
                        action = Some("ignore_all");
                    }
                    if ui.button("Add to Dictionary").clicked() {
                        action = Some("add");
                    }
                });
                ui.horizontal(|ui| {
                    let can = !sugg.is_empty();
                    if ui.add_enabled(can, egui::Button::new("Change")).clicked() {
                        action = Some("change");
                    }
                    if ui.add_enabled(can, egui::Button::new("Change All")).clicked() {
                        action = Some("change_all");
                    }
                });
            });
        if let (Some(act), Some((a, b, sugg))) = (action, item.clone()) {
            let word = self.doc.plain_text(a, b);
            let choice = sugg.get(self.dlg.spell_choice).cloned();
            match act {
                "ignore" => self.spell_advance(b),
                "ignore_all" => {
                    self.spell.ignore_all(&word);
                    self.spell_advance(a);
                }
                "add" => {
                    self.spell.add_to_dictionary(&word);
                    self.spell_advance(a);
                }
                "change" => {
                    if let Some(c) = choice {
                        self.replace_word(a, b, &c);
                        let next = Pos::new(a.p, a.o + c.chars().count());
                        self.spell_advance(next);
                    }
                }
                "change_all" => {
                    if let Some(c) = choice {
                        self.replace_all_words(&word, &c);
                        self.spell_advance(a);
                    }
                }
                _ => {}
            }
        }
        if action == Some("close") {
            open = false;
        }
        if self.dlg.spell_item.is_none() && !self.dlg.spell_done && self.spell.is_ready() && open {
            // The dictionary finished loading after the dialog opened.
            let from = self.sel().0;
            self.spell_advance(from);
        }
        self.dlg.spelling = open;
    }

    fn find_dialog(&mut self, ctx: &egui::Context) {
        if !self.find.open {
            return;
        }
        let mut open = true;
        let title = if self.find.replace { "Find and Replace" } else { "Find" };
        let screen = ctx.content_rect();
        egui::Window::new(title)
            .id(egui::Id::new("find_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(Pos2::new(screen.right() - 380.0, 150.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.find.replace, false, "Find");
                    ui.selectable_value(&mut self.find.replace, true, "Replace");
                });
                ui.separator();
                let mut enter = false;
                egui::Grid::new("find_grid").num_columns(2).show(ui, |ui| {
                    ui.label("Find what:");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.find.query).desired_width(220.0));
                    if self.find.focus_query {
                        r.request_focus();
                        self.find.focus_query = false;
                        self.text_focus_frame = true;
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        enter = true;
                    }
                    self.track(&r);
                    ui.end_row();
                    if self.find.replace {
                        ui.label("Replace with:");
                        let r = ui.add(egui::TextEdit::singleline(&mut self.find.replacement).desired_width(220.0));
                        self.track(&r);
                        ui.end_row();
                    }
                });
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut self.find.match_case, "Match case").changed() {
                        self.invalidate_find();
                    }
                    if ui.checkbox(&mut self.find.whole_word, "Whole words").changed() {
                        self.invalidate_find();
                    }
                });
                self.refresh_find();
                let n = self.find.matches.len();
                ui.label(if self.find.query.is_empty() {
                    String::new()
                } else if let Some(c) = self.find.current {
                    format!("Result {} of {n}", c + 1)
                } else {
                    format!("{n} result(s)")
                });
                ui.horizontal(|ui| {
                    if ui.button("◀ Previous").clicked() {
                        self.find_next(false);
                    }
                    if ui.button("Next ▶").clicked() || enter {
                        self.find_next(true);
                        if enter {
                            self.find.focus_query = true;
                        }
                    }
                    if self.find.replace {
                        if ui.button("Replace").clicked() {
                            self.replace_one();
                        }
                        if ui.button("Replace All").clicked() {
                            self.replace_all();
                        }
                    }
                });
            });
        if !open {
            self.find.open = false;
            self.find.current = None;
        }
    }
}
