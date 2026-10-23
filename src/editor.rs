//! The page canvas: keyboard and mouse handling, and painting.

use crate::app::{App, After, RulerMark, Tab};
use crate::ui::show_tip;
use crate::story::{StoryReq, Target, Zone};
use crate::doc::{
    Align, Anchor, HRel, LINE_BREAK, Leader, ListKind, OBJ, PAGE_BREAK, Pos, PosSpec, Rgba, Style,
    TabKind, VRel, Wrap,
};
use crate::layout::{self, Layout};
use crate::render;
use crate::theme;
use std::collections::HashMap;
use eframe::egui::{
    self, Color32, CursorIcon, Event, EventFilter, Key, Modifiers, Pos2, Rect, Sense, Stroke,
    StrokeKind, TextFormat, Ui, Vec2, pos2, text::LayoutJob, vec2,
};

pub const GAP: f32 = 24.0;
/// Width of comment cards beside the page.
const COMMENT_W: f32 = 230.0;

pub fn c32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

/// Replaces a token colour's alpha channel (keeping its RGB).
fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

impl App {
    // ================================================================ keyboard

    pub fn handle_events(&mut self, ctx: &egui::Context) {
        if self.text_input_focused {
            return;
        }
        let (events, mods) = ctx.input(|i| (i.events.clone(), i.modifiers));
        // Ctrl+V is handled after the batch so we can read the clipboard once
        // (egui also emits Event::Paste for the same key, which we suppress).
        let ctrl_v = events.iter().find_map(|e| match e {
            Event::Key { key: Key::V, pressed: true, modifiers, .. }
                if modifiers.command && !modifiers.alt => Some(modifiers.shift),
            _ => None,
        });
        for ev in events {
            match ev {
                // Ctrl+key chords are shortcuts, not text (AltGr = Ctrl+Alt still types).
                Event::Text(t) if !(mods.ctrl && !mods.alt) => self.insert_typed(&t),
                Event::Ime(egui::ImeEvent::Commit(t)) => self.insert_typed(&t),
                Event::Copy => self.copy(),
                Event::Cut => self.cut(),
                Event::Paste(_) if ctrl_v.is_some() => {}
                Event::Paste(_) => self.paste_from_system(),
                Event::Key { key: Key::V, pressed: true, modifiers, .. } if modifiers.command => {}
                Event::Key { key, pressed: true, modifiers, .. } => self.key(key, modifiers),
                _ => {}
            }
        }
        if let Some(plain) = ctrl_v {
            if plain {
                self.paste_plain_from_system();
            } else {
                self.paste_from_system();
            }
        }
    }

    fn key(&mut self, key: Key, m: Modifiers) {
        let ctrl = m.command;
        let shift = m.shift;
        let alt = m.alt;
        // Arrows nudge a selected floating picture or table instead of the caret.
        if !alt {
            let step = if shift { 10.0 } else { 1.0 };
            let (dx, dy) = match key {
                Key::ArrowLeft => (-step, 0.0),
                Key::ArrowRight => (step, 0.0),
                Key::ArrowUp => (0.0, -step),
                Key::ArrowDown => (0.0, step),
                _ => (0.0, 0.0),
            };
            if dx != 0.0 || dy != 0.0 {
                if let Some(table) = self.selected_table() {
                    if self.doc.tables.get(&table).is_some_and(|t| t.anchor.is_some()) {
                        self.nudge_table(table, dx, dy);
                        return;
                    }
                }
                if let Some(id) = self.selected_image() {
                    if self.doc.images.get(&id).is_some_and(|i| i.anchor.is_some()) {
                        self.nudge_image(id, dx, dy);
                        return;
                    }
                }
            }
        }
        let horizontal = |s: &mut App, pos: Pos| {
            s.goal_x = None;
            s.set_cursor(pos, shift);
        };
        match key {
            Key::ArrowLeft => {
                if !shift && self.has_sel() {
                    let a = self.sel().0;
                    horizontal(self, a);
                } else {
                    let p = if ctrl { self.doc.word_left(self.cursor) } else { self.doc.prev(self.cursor) };
                    horizontal(self, p);
                }
            }
            Key::ArrowRight => {
                if !shift && self.has_sel() {
                    let b = self.sel().1;
                    horizontal(self, b);
                } else {
                    let p = if ctrl { self.doc.word_right(self.cursor) } else { self.doc.next(self.cursor) };
                    horizontal(self, p);
                }
            }
            Key::ArrowUp if ctrl => {
                let p = if self.cursor.o == 0 && self.cursor.p > 0 { self.cursor.p - 1 } else { self.cursor.p };
                horizontal(self, Pos::new(p, 0));
            }
            Key::ArrowDown if ctrl => {
                let p = (self.cursor.p + 1).min(self.doc.paras.len() - 1);
                let pos = if p == self.cursor.p { Pos::new(p, self.doc.paras[p].len()) } else { Pos::new(p, 0) };
                horizontal(self, pos);
            }
            Key::ArrowUp => self.move_vertical(-1, shift, false),
            Key::ArrowDown => self.move_vertical(1, shift, false),
            Key::PageUp => self.move_vertical(-1, shift, true),
            Key::PageDown => self.move_vertical(1, shift, true),
            Key::Home => {
                if ctrl {
                    horizontal(self, Pos::default());
                } else {
                    self.ensure_layout();
                    let lay = self.layout.as_ref().unwrap();
                    let l = &lay.lines[lay.line_of(self.cursor)];
                    let p = Pos::new(l.para, l.start);
                    horizontal(self, p);
                }
            }
            Key::End => {
                if ctrl {
                    let e = self.doc.end();
                    horizontal(self, e);
                } else {
                    self.ensure_layout();
                    let lay = self.layout.as_ref().unwrap();
                    let l = &lay.lines[lay.line_of(self.cursor)];
                    let o = if l.last_in_para && !l.forced { l.end } else { l.end.saturating_sub(1).max(l.start) };
                    let p = Pos::new(l.para, o);
                    horizontal(self, p);
                }
            }
            Key::Backspace => self.backspace(ctrl),
            Key::Delete => self.delete_forward(ctrl),
            Key::Enter => {
                if ctrl {
                    self.insert_page_break();
                } else if shift {
                    self.insert_line_break();
                } else {
                    self.enter();
                }
            }
            Key::Tab if self.in_table().is_some() && !ctrl => self.next_cell(!shift),
            Key::Tab => {
                let multi = self.sel().0.p != self.sel().1.p;
                let p = self.current_para();
                if multi || (p.list != ListKind::None && self.cursor.o == 0) {
                    self.indent(if shift { -1 } else { 1 });
                } else if !shift {
                    self.insert_typed("\t");
                }
            }
            Key::F7 => self.start_spelling(),
            Key::Escape => {
                if self.format_painter.is_none() && !self.find.open {
                    self.close_story();
                }
                self.format_painter = None;
                self.find.open = false;
            }
            _ if ctrl => self.shortcut(key, shift, alt),
            _ => {}
        }
    }

    fn shortcut(&mut self, key: Key, shift: bool, alt: bool) {
        match key {
            Key::A => {
                let e = self.doc.end();
                self.select(Pos::default(), e);
            }
            Key::F if alt => self.insert_note(false),
            Key::D if alt => self.insert_note(true),
            Key::B => self.toggle_bold(),
            Key::I => self.toggle_italic(),
            Key::U => self.toggle_underline(),
            Key::Z => self.undo(),
            Key::Y => self.redo(),
            Key::S if shift => {
                self.save_as();
            }
            Key::S => {
                self.save();
            }
            Key::O => self.guarded(After::Open),
            Key::N if shift => self.set_style(Style::Normal),
            Key::N => self.guarded(After::New),
            Key::P => self.print(),
            Key::F => {
                self.find.open = true;
                self.find.replace = false;
                self.find.focus_query = true;
                self.prefill_find();
            }
            Key::H => {
                self.find.open = true;
                self.find.replace = true;
                self.find.focus_query = true;
                self.prefill_find();
            }
            Key::E => self.set_align(Align::Center),
            Key::L if shift => self.toggle_list(ListKind::Bullet),
            Key::L => self.set_align(Align::Left),
            Key::R => self.set_align(Align::Right),
            Key::J => self.set_align(Align::Justify),
            Key::M => self.indent(if shift { -1 } else { 1 }),
            Key::OpenBracket => self.grow_font(false),
            Key::CloseBracket => self.grow_font(true),
            Key::Comma if shift => self.grow_font(false),
            Key::Period if shift => self.grow_font(true),
            Key::Plus => self.toggle_script(1),
            Key::Equals if shift => self.toggle_script(1),
            Key::Equals => self.toggle_script(-1),
            Key::Space => self.clear_formatting(),
            Key::Num1 if alt => self.set_style(Style::Heading1),
            Key::Num2 if alt => self.set_style(Style::Heading2),
            Key::Num3 if alt => self.set_style(Style::Heading3),
            Key::Num1 => self.set_line_spacing(1.0),
            Key::Num2 => self.set_line_spacing(2.0),
            Key::Num5 => self.set_line_spacing(1.5),
            Key::D if shift => self.toggle_strike(),
            Key::K => self.open_link_dialog(),
            _ => {}
        }
    }

    fn prefill_find(&mut self) {
        if self.has_sel() {
            let (a, b) = self.sel();
            if a.p == b.p && b.o - a.o < 100 {
                self.find.query = self.doc.plain_text(a, b);
            }
        }
        self.invalidate_find();
    }

    fn move_vertical(&mut self, dir: i32, extend: bool, by_page: bool) {
        self.ensure_layout();
        let lay = self.layout.as_ref().unwrap();
        let li = lay.line_of(self.cursor);
        let (_, x, _, _) = lay.caret(self.cursor);
        let gx = self.goal_x.unwrap_or(x);
        // Visual movement: the nearest line above/below that overlaps the goal x.
        let mut target = Some(li);
        if by_page {
            let mut acc = 0.0;
            let limit = self.viewport_h * 0.9;
            let mut cur = li;
            while acc < limit {
                match lay.vertical(cur, gx, dir) {
                    Some(n) => {
                        acc += lay.lines[n].height;
                        cur = n;
                    }
                    None => break,
                }
            }
            target = Some(cur);
        } else {
            target = target.and_then(|t| lay.vertical(t, gx, dir));
        }
        let pos = match target {
            Some(t) if t != li || by_page => lay.pos_in_line(t, gx),
            _ if dir < 0 => Pos::default(),
            _ => self.doc.end(),
        };
        self.set_cursor(pos, extend);
        self.goal_x = Some(gx);
    }

    // ================================================================ canvas

    /// Width reserved to the right of the pages for comment cards.
    pub fn comments_width(&self) -> f32 {
        if self.show_comments && !self.main_doc().comments.is_empty() { COMMENT_W + 2.0 * GAP } else { 0.0 }
    }

    fn page_origin(&self, rect: Rect, lay: &Layout, page: usize) -> Pos2 {
        let cw = self.comments_width();
        let g = lay.geom(page);
        let x = rect.left() + ((rect.width() - cw - g.w) / 2.0).max(GAP);
        pos2(x, rect.top() + GAP + lay.page_top(page))
    }

    fn hit_point(&self, rect: Rect, lay: &Layout, p: Pos2) -> (usize, Pos) {
        let rel = p.y - rect.top() - GAP;
        let page = lay.page_at_y(rel);
        let o = self.page_origin(rect, lay, page);
        (page, lay.hit(page, p.x - o.x, p.y - o.y))
    }

    /// Screen rectangle of a picture character (page-relative).
    fn image_box(&self, lay: &Layout, pos: Pos) -> Option<(usize, Rect)> {
        // Floating pictures live at their anchored position.
        if let Some(fb) = lay.floats.iter().find(|f| f.para == pos.p && f.off == pos.o && f.image != 0) {
            return Some((fb.page, Rect::from_min_size(pos2(fb.x, fb.y), vec2(fb.w, fb.h))));
        }
        let para = self.doc.paras.get(pos.p)?;
        let f = para.fmts.get(pos.o)?;
        let img = self.doc.images.get(&f.image)?;
        if img.anchor.is_some() {
            return None;
        }
        let l = &lay.lines[lay.line_of(pos)];
        if pos.o < l.start || pos.o >= l.end {
            return None;
        }
        let x = l.xs[pos.o - l.start];
        let base = l.y + l.baseline;
        let sc = lay.scale;
        Some((l.page, Rect::from_min_size(pos2(x, base - img.h * sc), vec2(img.w * sc, img.h * sc))))
    }

    /// Floating picture under a screen point: (image id, anchor pos, screen rect, behind text).
    fn float_under(&self, rect: Rect, lay: &Layout, p: Pos2) -> Option<(u32, Pos, Rect, bool)> {
        lay.floats.iter().rev().find_map(|f| {
            if f.image == 0 {
                return None;
            }
            let o = self.page_origin(rect, lay, f.page);
            let r = Rect::from_min_size(o + vec2(f.x, f.y), vec2(f.w, f.h));
            r.contains(p).then_some((f.image, Pos::new(f.para, f.off), r, f.behind))
        })
    }

    /// Moves a floating picture to a page offset in points, clamped to the page so
    /// it always stays fully on the sheet. Does nothing to inline pictures.
    pub fn move_image_to_page(&mut self, id: u32, x_pt: f32, y_pt: f32) {
        let Some(img) = self.doc.images.get(&id).cloned() else { return };
        if img.anchor.is_none() {
            return;
        }
        let (pw, ph) = (self.doc.page.width, self.doc.page.height);
        let nx = x_pt.clamp(0.0, (pw - img.w).max(0.0));
        let ny = y_pt.clamp(0.0, (ph - img.h).max(0.0));
        if let Some(a) = self.doc.images.get_mut(&id).and_then(|i| i.anchor.as_mut()) {
            a.h_rel = HRel::Page;
            a.h_pos = PosSpec::Offset(nx);
            a.v_rel = VRel::Page;
            a.v_pos = PosSpec::Offset(ny);
        }
        self.rev += 1;
    }

    /// Nudges a floating picture by `(dx, dy)` points from wherever it is now.
    /// One coalesced undo step per burst of key presses.
    pub fn nudge_image(&mut self, id: u32, dx: f32, dy: f32) {
        if !self.doc.images.get(&id).is_some_and(|i| i.anchor.is_some()) {
            return;
        }
        self.ensure_layout();
        let pos = self.doc.paras.iter().enumerate().find_map(|(p, para)| {
            para.fmts.iter().position(|f| f.image == id).map(|o| Pos::new(p, o))
        });
        let xy = pos.and_then(|pos| {
            let lay = self.layout.as_ref()?;
            let sc = lay.scale;
            self.image_box(lay, pos).map(|(_, r)| (r.left() / sc, r.top() / sc))
        });
        let Some((x, y)) = xy else { return };
        self.checkpoint(crate::app::EditKind::ImageMove);
        self.move_image_to_page(id, x + dx, y + dy);
    }

    /// Page-relative bounds (page, x, y, w, h) of a laid-out table, if it has cells.
    fn table_bounds(&self, table: u32) -> Option<(usize, f32, f32, f32, f32)> {
        table_bounds_in(self.layout.as_ref()?, table)
    }

    /// Screen rect of a laid-out table (page-relative bounds moved to the screen).
    fn table_screen(&self, rect: Rect, lay: &Layout, table: u32) -> Option<Rect> {
        let cells: Vec<&layout::CellBox> = lay.cells.iter().filter(|c| c.table == table).collect();
        if cells.is_empty() {
            return None;
        }
        let page = cells[0].page;
        let x0 = cells.iter().map(|c| c.x).fold(f32::MAX, f32::min);
        let x1 = cells.iter().map(|c| c.x + c.w).fold(f32::MIN, f32::max);
        let y0 = cells.iter().map(|c| c.y).fold(f32::MAX, f32::min);
        let y1 = cells.iter().map(|c| c.y + c.h).fold(f32::MIN, f32::max);
        let o = self.page_origin(rect, lay, page);
        Some(Rect::from_min_max(o + vec2(x0, y0), o + vec2(x1, y1)))
    }

    /// The "move" handle of any table under the pointer: (table id, screen rect).
    fn table_handle_at(&self, rect: Rect, lay: &Layout, p: Pos2) -> Option<(u32, Rect)> {
        let mut ids: Vec<u32> = lay.cells.iter().map(|c| c.table).filter(|t| *t != 0).collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            if let Some(r) = self.table_screen(rect, lay, id) {
                let handle = Rect::from_center_size(r.min, vec2(14.0, 14.0));
                if handle.contains(p) {
                    return Some((id, r));
                }
            }
        }
        None
    }

    /// The "resize" handle (bottom-right) of any table under the pointer.
    fn table_resize_at(&self, rect: Rect, lay: &Layout, p: Pos2) -> Option<(u32, Rect)> {
        let mut ids: Vec<u32> = lay.cells.iter().map(|c| c.table).filter(|t| *t != 0).collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            if let Some(r) = self.table_screen(rect, lay, id) {
                let handle = Rect::from_center_size(r.max, vec2(14.0, 14.0));
                if handle.contains(p) {
                    return Some((id, r));
                }
            }
        }
        None
    }

    /// Selects every cell paragraph of a table (the whole-table selection).
    pub fn select_table(&mut self, table: u32) {
        let mut first = usize::MAX;
        let mut last = 0usize;
        for (i, p) in self.doc.paras.iter().enumerate() {
            if p.cell.is_some_and(|c| c.table == table) {
                first = first.min(i);
                last = i;
            }
        }
        if first == usize::MAX {
            return;
        }
        self.select_quiet(Pos::new(first, 0), Pos::new(last, self.doc.paras[last].len()));
    }

    /// The table id when the whole table is selected, else None.
    pub fn selected_table(&self) -> Option<u32> {
        if !self.has_sel() {
            return None;
        }
        let (a, b) = self.sel();
        let first = self.doc.paras[a.p].cell?;
        if a.o != 0 || b.o != self.doc.paras[b.p].len() {
            return None;
        }
        if (a.p..=b.p).all(|p| self.doc.paras[p].cell.is_some_and(|c| c.table == first.table)) {
            Some(first.table)
        } else {
            None
        }
    }

    /// Moves a floating table to a page offset in points, clamped to the sheet.
    pub fn move_table_to_page(&mut self, table: u32, x_pt: f32, y_pt: f32) {
        if !self.doc.tables.get(&table).is_some_and(|t| t.anchor.is_some()) {
            return;
        }
        let (pw, ph) = (self.doc.page.width, self.doc.page.height);
        let (tw, th) = self.table_bounds(table).map_or_else(
            || {
                // No layout at hand (mid-drag): clamp using the grid width in points.
                let tw = self.doc.tables.get(&table).map_or(0.0, |t| t.grid.iter().sum::<f32>());
                (tw, 0.0)
            },
            |(_, _, _, w, h)| {
                let sc = self.layout.as_ref().map(|l| l.scale).unwrap_or(1.0);
                (w / sc, h / sc)
            },
        );
        let nx = x_pt.clamp(0.0, (pw - tw).max(0.0));
        let ny = y_pt.clamp(0.0, (ph - th).max(0.0));
        if let Some(a) = self.doc.tables.get_mut(&table).and_then(|t| t.anchor.as_mut()) {
            a.h_rel = HRel::Page;
            a.h_pos = PosSpec::Offset(nx);
            a.v_rel = VRel::Page;
            a.v_pos = PosSpec::Offset(ny);
        }
        self.rev += 1;
    }

    /// Sets the gap kept between a floating picture and the text (points, t/r/b/l).
    pub fn set_image_margins(&mut self, id: u32, t: f32, r: f32, b: f32, l: f32) {
        if let Some(a) = self.doc.images.get_mut(&id).and_then(|i| i.anchor.as_mut()) {
            a.dist = [t.max(0.0), r.max(0.0), b.max(0.0), l.max(0.0)];
            self.checkpoint(crate::app::EditKind::ImageMove);
            self.rev += 1;
        }
    }

    /// Sets the gap kept between a floating table and the text (points, t/r/b/l).
    pub fn set_table_margins(&mut self, table: u32, t: f32, r: f32, b: f32, l: f32) {
        if let Some(a) = self.doc.tables.get_mut(&table).and_then(|t2| t2.anchor.as_mut()) {
            a.dist = [t.max(0.0), r.max(0.0), b.max(0.0), l.max(0.0)];
            self.checkpoint(crate::app::EditKind::ImageMove);
            self.rev += 1;
        }
    }

    /// Nudges a selected floating table by `(dx, dy)` points.
    pub fn nudge_table(&mut self, table: u32, dx: f32, dy: f32) {
        if !self.doc.tables.get(&table).is_some_and(|t| t.anchor.is_some()) {
            return;
        }
        self.ensure_layout();
        let Some((_, x, y, _, _)) = self.table_bounds(table) else { return };
        let sc = self.layout.as_ref().map(|l| l.scale).unwrap_or(1.0);
        self.checkpoint(crate::app::EditKind::ImageMove);
        self.move_table_to_page(table, x / sc + dx, y / sc + dy);
    }

    /// Scales a table's columns so their total width becomes `new_total` points,
    /// proportionally to `base` (the columns captured when the drag started).
    pub fn resize_table_width(&mut self, table: u32, base: &[f32], new_total: f32) {
        let total0: f32 = base.iter().sum();
        let min_total = base.len().max(1) as f32 * 18.0;
        let max_total = self.content_width_pt().max(min_total);
        let new_total = new_total.clamp(min_total, max_total);
        let factor = if total0 > 0.0 { new_total / total0 } else { 1.0 };
        if let Some(t) = self.doc.tables.get_mut(&table) {
            for (c, &g0) in t.grid.iter_mut().zip(base.iter()) {
                *c = g0 * factor;
            }
        }
        self.rev += 1;
    }

    /// Sets how text wraps around a table: `None` keeps it in the flow ("In Line"),
    /// `Some(Wrap::Side)` floats it "Around", `Some(Wrap::Clear)` keeps text only
    /// above and below ("Top & Bottom"). A table that is not floating yet is
    /// floated at the position it currently occupies.
    pub fn set_table_wrap(&mut self, table: u32, wrap: Option<Wrap>) {
        let Some(cur) = self.doc.tables.get(&table).map(|t| t.anchor) else { return };
        match wrap {
            None => {
                if cur.is_none() {
                    return;
                }
                self.checkpoint(crate::app::EditKind::Other);
                if let Some(t) = self.doc.tables.get_mut(&table) {
                    t.anchor = None;
                }
            }
            Some(w) => {
                if cur.is_some_and(|a| a.wrap == w) {
                    return;
                }
                match cur {
                    // Already floating: keep the position, just change the mode.
                    Some(a) => {
                        self.checkpoint(crate::app::EditKind::Other);
                        if let Some(t) = self.doc.tables.get_mut(&table) {
                            t.anchor = Some(Anchor { wrap: w, ..a });
                        }
                    }
                    None => {
                        self.ensure_layout();
                        let (x, y) = self.table_bounds(table).map(|(_, x, y, _, _)| {
                            let sc = self.layout.as_ref().map(|l| l.scale).unwrap_or(1.0);
                            (x / sc, y / sc)
                        }).unwrap_or((0.0, 0.0));
                        self.checkpoint(crate::app::EditKind::Other);
                        if let Some(t) = self.doc.tables.get_mut(&table) {
                            t.anchor = Some(Anchor {
                                h_rel: HRel::Page,
                                h_pos: PosSpec::Offset(x),
                                v_rel: VRel::Page,
                                v_pos: PosSpec::Offset(y),
                                wrap: w,
                                behind: false,
                                dist: [0.0; 4],
                            });
                        }
                    }
                }
            }
        }
        self.rev += 1;
    }

    /// Changes how text wraps around a picture. `None` makes it inline again;
    /// `Some(wrap)` floats it at its current position with that wrap mode.
    pub fn set_image_wrap(&mut self, id: u32, wrap: Option<crate::doc::Wrap>) {
        let Some(img) = self.doc.images.get(&id).cloned() else { return };
        match wrap {
            None => {
                if img.anchor.is_some() {
                    self.checkpoint(crate::app::EditKind::Other);
                    self.doc.images.get_mut(&id).unwrap().anchor = None;
                }
            }
            Some(w) => {
                // Paragraph-relative anchors drift when Enter splits the paragraph
                // above the picture, so those are always re-derived below.
                if img.anchor.is_some_and(|a| a.wrap == w && !a.behind && a.v_rel != VRel::Paragraph) {
                    return;
                }
                // Glue the float to the line that holds its anchor character: the
                // position is stored relative to that line, so typing or splitting
                // the paragraph above it cannot slide the picture down.
                let pos = (0..self.doc.paras.len())
                    .find_map(|p| self.doc.paras[p].fmts.iter().position(|f| f.image == id).map(|o| Pos::new(p, o)));
                self.ensure_layout();
                let derived = match (pos, self.layout.as_ref()) {
                    (Some(pos), Some(lay)) => {
                        let sc = lay.scale;
                        let line_y = lay.lines[lay.line_of(pos)].y;
                        self.image_box(lay, pos).map(|(_, r)| ((r.left() / sc) - self.doc.page.margins[3], (r.top() - line_y) / sc))
                    }
                    _ => None,
                };
                let anchor = match img.anchor {
                    Some(mut a) => {
                        a.wrap = w;
                        a.behind = false;
                        if let Some((h_off, v_off)) = derived.filter(|_| a.v_rel == VRel::Paragraph) {
                            a.h_rel = HRel::Margin;
                            a.h_pos = PosSpec::Offset(h_off);
                            a.v_rel = VRel::Line;
                            a.v_pos = PosSpec::Offset(v_off);
                        }
                        a
                    }
                    None => {
                        let (h_off, v_off) = derived.unwrap_or((0.0, 0.0));
                        Anchor {
                            h_rel: HRel::Margin,
                            h_pos: PosSpec::Offset(h_off),
                            v_rel: VRel::Line,
                            v_pos: PosSpec::Offset(v_off),
                            wrap: w,
                            behind: false,
                            dist: [0.0; 4],
                        }
                    }
                };
                self.checkpoint(crate::app::EditKind::Other);
                self.doc.images.get_mut(&id).unwrap().anchor = Some(anchor);
            }
        }
    }

    pub fn canvas(&mut self, ui: &mut Ui) {
        self.ensure_layout();
        let lay = self.layout.take().unwrap();
        let avail = ui.available_size();
        let max_w = (0..lay.pages).map(|p| lay.geom(p).w).fold(lay.page_w, f32::max);
        let total_w = (max_w + 2.0 * GAP + self.comments_width()).max(avail.x);
        let last = lay.geom(lay.pages.saturating_sub(1));
        let total_h = lay.page_top(lay.pages.saturating_sub(1)) + last.h + GAP;
        let (rect, resp) = ui.allocate_exact_size(vec2(total_w, total_h), Sense::click_and_drag());
        let id = resp.id;

        if !self.text_input_focused {
            ui.memory_mut(|m| {
                m.request_focus(id);
                m.set_focus_lock_filter(
                    id,
                    EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                );
            });
        }

        let rev0 = self.rev;
        self.mouse(ui, &resp, rect, &lay);
        // The context menu (and drags) can mutate the document after the layout
        // was taken; relayout before painting so `paint` never indexes stale,
        // now-shorter paragraphs (which used to crash the app).
        let lay = if self.rev != rev0 {
            self.layout = Some(lay);
            self.ensure_layout();
            self.layout.take().unwrap()
        } else {
            lay
        };
        self.paint(ui, rect, &lay);

        if self.scroll_to_caret {
            self.scroll_to_caret = false;
            let (pg, x, y, h) = lay.caret(self.cursor);
            let o = self.page_origin(rect, &lay, pg);
            let r = Rect::from_min_size(o + vec2(x, y), vec2(2.0, h)).expand(30.0);
            ui.scroll_to_rect(r, None);
        }
        self.layout = Some(lay);
        if let Some(req) = self.story_req.take() {
            self.apply_story_req(req);
        }
    }

    /// Page under a screen point, the zone of that page it is in (judged from the
    /// body layout), and the page-relative point.
    fn zone_at(&self, rect: Rect, lay: &Layout, p: Pos2) -> (usize, Zone, Pos2) {
        let bl = self.main_layout().unwrap_or(lay);
        let rel = p.y - rect.top() - GAP;
        let page = bl.page_at_y(rel);
        let o = self.page_origin(rect, bl, page);
        let local = pos2(p.x - o.x, p.y - o.y);
        let g = bl.geom(page);
        let note = bl.notes.iter().find(|n| {
            let (t, b) = Layout::note_extent(n);
            n.page == page && local.y >= t - 2.0 && local.y <= b + 2.0
        });
        let zone = if let Some(n) = note {
            Zone::Note(n.key.clone())
        } else if let Some(para) = bl.nested_at(page, local.x, local.y) {
            Zone::Nested(para)
        } else if local.y < g.margins[0] {
            Zone::Header
        } else if local.y > g.h - g.margins[2] {
            Zone::Footer
        } else {
            Zone::Body
        };
        (page, zone, local)
    }

    fn mouse(&mut self, ui: &Ui, resp: &egui::Response, rect: Rect, lay: &Layout) {
        let (pointer, pressed, secondary, down, released, shift, time) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_pressed(),
                i.pointer.secondary_pressed(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.modifiers.shift,
                i.time,
            )
        });
        let sc = lay.scale;
        let handle_of = |s: &App, id: u32| -> Option<Rect> {
            let (a, _) = s.sel();
            let (pg, r) = s.image_box(lay, a)?;
            let _ = id;
            let o = s.page_origin(rect, lay, pg);
            Some(Rect::from_center_size(o + r.max.to_vec2(), vec2(10.0, 10.0)))
        };

        if resp.hovered() {
            if let Some(p) = pointer {
                let over_handle = self.selected_image().and_then(|id| handle_of(self, id)).is_some_and(|h| h.contains(p));
                // A floating picture's body is draggable to any point on the page.
                let over_float = self.float_under(rect, lay, p).is_some_and(|(fid, _, _, behind)| {
                    self.doc.images.get(&fid).is_some_and(|i| i.anchor.is_some())
                        && (!behind || self.selected_image() == Some(fid))
                });
                let ctrl = ui.input(|i| i.modifiers.command);
                let over_link = ctrl && {
                    let (_, pos) = self.hit_point(rect, lay, p);
                    self.doc.paras[pos.p].fmts.get(pos.o).is_some_and(|f| f.link != 0)
                };
                let over_table = self.table_handle_at(rect, lay, p).is_some();
                let over_table_resize = self.table_resize_at(rect, lay, p).is_some();
                let icon = if over_handle || over_table_resize {
                    CursorIcon::ResizeNwSe
                } else if over_float || over_table {
                    CursorIcon::Grab
                } else if over_link {
                    CursorIcon::PointingHand
                } else if self.format_painter.is_some() {
                    CursorIcon::Crosshair
                } else {
                    CursorIcon::Text
                };
                ui.ctx().set_cursor_icon(icon);
            }
        }

        if pressed && resp.hovered() {
            self.text_input_focused = false;
            if let Some(p) = pointer {
                if let Some(id) = self.selected_image() {
                    if handle_of(self, id).is_some_and(|h| h.contains(p)) {
                        let w = self.doc.images[&id].w;
                        self.checkpoint(crate::app::EditKind::Other);
                        self.drag.resizing = Some((id, w, p));
                        return;
                    }
                }
                // Table resize handle (bottom-right): scales the column widths.
                if let Some((tid, _)) = self.table_resize_at(rect, lay, p) {
                    if let Some(t) = self.doc.tables.get(&tid) {
                        let total: f32 = t.grid.iter().sum();
                        let grid = t.grid.clone();
                        self.checkpoint(crate::app::EditKind::Other);
                        self.drag.resizing_table = Some((tid, p, total, grid));
                        return;
                    }
                }
                // Table move handle (top-left, like Word): selects the whole table
                // and arms a move; the first drag makes it float ("Around" wrap).
                if let Some((tid, r)) = self.table_handle_at(rect, lay, p) {
                    self.select_table(tid);
                    self.drag.moved = false;
                    // Store the origin in PAGE-relative points (the screen rect is
                    // only for hit-testing), otherwise the table jumps on grab.
                    let (ox, oy, w_pt, h_pt) = table_bounds_in(lay, tid)
                        .map(|(_, x, y, w, h)| (x / sc, y / sc, w / sc, h / sc))
                        .unwrap_or((r.left() / sc, r.top() / sc, r.width() / sc, r.height() / sc));
                    self.drag.moving_table = Some((tid, p, ox, oy, w_pt, h_pt));
                    return;
                }
                // Pressing a floating picture (square/tight/top&bottom wrap) selects it
                // and arms a move, so a single press-drag slides it anywhere on the page.
                if let Some((fid, ip, r, behind)) = self.float_under(rect, lay, p) {
                    if self.doc.images.get(&fid).is_some_and(|i| i.anchor.is_some())
                        && (!behind || self.selected_image() == Some(fid))
                    {
                        let next = self.doc.next(ip);
                        self.select_quiet(ip, next);
                        self.drag.moved = false;
                        // Page-relative origin (avoids the grab jump).
                        let (ox, oy) = self
                            .image_box(lay, ip)
                            .map(|(_, rp)| (rp.left() / sc, rp.top() / sc))
                            .unwrap_or((r.left() / sc, r.top() / sc));
                        self.drag.moving = Some((fid, p, ox, oy));
                        return;
                    }
                }
                let same_spot = time - self.last_click.0 < 0.45 && self.last_click.1.distance(p) < 6.0;
                let count = if same_spot { self.last_click.2 + 1 } else { 1 };
                self.last_click = (time, p, count);
                // Headers and footers open and close with a double-click, like Word;
                // notes and nested tables with a single click.
                let (zpage, zone, local) = self.zone_at(rect, lay, p);
                let at = Some((local.x, local.y));
                let at_page = Some((zpage, local.x, local.y));
                let hf_req = |z: &Zone| StoryReq::Open { page: zpage, header: *z == Zone::Header, at };
                let own = self.story.as_deref().map(|s| (s.zone(), s.page));
                let req = match own {
                    None => match &zone {
                        Zone::Note(key) => Some(StoryReq::OpenNote { key: key.clone(), at: at_page }),
                        Zone::Nested(para) => Some(StoryReq::OpenNested { para: *para, at: at_page }),
                        Zone::Header | Zone::Footer if count == 2 => Some(hf_req(&zone)),
                        _ => None,
                    },
                    // Inside the open story: ordinary editing.
                    Some((z0, p0)) if zone == z0 && (zpage == p0 || matches!(z0, Zone::Note(_) | Zone::Nested(_))) => None,
                    Some((z0, _)) => {
                        let note_open = matches!(z0, Zone::Note(_) | Zone::Nested(_));
                        Some(match &zone {
                            Zone::Note(key) => StoryReq::OpenNote { key: key.clone(), at: at_page },
                            Zone::Nested(para) if note_open => StoryReq::OpenNested { para: *para, at: at_page },
                            Zone::Body if note_open => StoryReq::Close { at: at_page },
                            Zone::Body | Zone::Nested(_) if count == 2 => StoryReq::Close { at: None },
                            // Another header/footer (or the same one on another page).
                            Zone::Header | Zone::Footer if count == 2 || zone == z0 => hf_req(&zone),
                            _ => return,
                        })
                    }
                };
                if req.is_some() {
                    self.story_req = req;
                    return;
                }
                let (page, pos) = self.hit_point(rect, lay, p);
                self.goal_x = None;
                // Ctrl+Click follows hyperlinks, like Word.
                if ui.input(|i| i.modifiers.command) {
                    let para = &self.doc.paras[pos.p];
                    let link = [pos.o, pos.o.saturating_sub(1)].into_iter().filter_map(|o| para.fmts.get(o)).map(|f| f.link).find(|l| *l != 0);
                    if let Some(id) = link {
                        self.open_link(id);
                        return;
                    }
                }
                match count {
                    1 => {
                        // Clicking a picture selects it.
                        let o = self.page_origin(rect, lay, page);
                        let local = p - o.to_vec2();
                        // Floating objects first (topmost last in the list), then inline pictures.
                        let float_hit = lay
                            .floats
                            .iter()
                            .rev()
                            .find(|f| f.page == page && Rect::from_min_size(pos2(f.x, f.y), vec2(f.w, f.h)).contains(local))
                            .map(|f| Pos::new(f.para, f.off));
                        let img_hit = float_hit.or_else(|| {
                            [pos, self.doc.prev(pos)].into_iter().find(|cand| {
                                self.image_box(lay, *cand).is_some_and(|(pg, r)| pg == page && r.contains(local))
                            })
                        });
                        if let (Some(ip), false) = (img_hit, shift) {
                            let next = self.doc.next(ip);
                            self.select(ip, next);
                        } else {
                            self.set_cursor(pos, shift);
                            self.drag.selecting = true;
                        }
                    }
                    2 => {
                        let (a, b) = self.doc.word_at(pos);
                        self.select(a, b);
                        self.drag.word_mode = Some((a, b));
                        self.drag.selecting = true;
                    }
                    _ => {
                        let len = self.doc.paras[pos.p].len();
                        self.select(Pos::new(pos.p, 0), Pos::new(pos.p, len));
                        self.drag.selecting = false;
                    }
                }
            }
        }

        if secondary && resp.hovered() {
            if let Some(p) = pointer {
                let (_, pos) = self.hit_point(rect, lay, p);
                let (a, b) = self.sel();
                if !(self.has_sel() && pos >= a && pos <= b) {
                    self.set_cursor(pos, false);
                }
                // Remember a misspelled word under the pointer for the context menu.
                self.ctx_word = None;
                let ranges = self.spell.misspelled(&self.doc.paras[pos.p]);
                if let Some((s, e)) = ranges.into_iter().find(|(s, e)| pos.o >= *s && pos.o <= *e) {
                    let (ws, we) = (Pos::new(pos.p, s), Pos::new(pos.p, e));
                    let word = self.doc.plain_text(ws, we);
                    let sugg = self.spell.suggest(&word);
                    self.ctx_word = Some((ws, we, sugg));
                }
            }
        }

        if down {
            if let (Some((id, w0, start)), Some(p)) = (self.drag.resizing, pointer) {
                if let Some(img) = self.doc.images.get(&id).cloned() {
                    let maxw = self.content_width_pt();
                    let w = (w0 + (p.x - start.x) / sc).clamp(8.0, maxw);
                    let im = self.doc.images.get_mut(&id).unwrap();
                    im.w = w;
                    im.h = w * img.px_h as f32 / img.px_w.max(1) as f32;
                    self.rev += 1;
                }
            } else if let (Some((id, start, x0, y0)), Some(p)) = (self.drag.moving, pointer) {
                let (dx, dy) = (p.x - start.x, p.y - start.y);
                // Only take one undo step, and only once the pointer really moved.
                if !self.drag.moved && dx.hypot(dy) >= 2.0 {
                    self.checkpoint(crate::app::EditKind::Other);
                    self.drag.moved = true;
                }
                if self.drag.moved {
                    self.move_image_to_page(id, x0 + dx / sc, y0 + dy / sc);
                    // Scroll the view when the pointer nears the top/bottom edge so
                    // the picture can be placed on a part of the page off-screen.
                    let edge = 40.0;
                    let sy = if p.y < rect.top() + edge {
                        p.y - (rect.top() + edge)
                    } else if p.y > rect.bottom() - edge {
                        p.y - (rect.bottom() - edge)
                    } else {
                        0.0
                    };
                    if sy != 0.0 {
                        ui.scroll_with_delta(vec2(0.0, sy.clamp(-24.0, 24.0)));
                    }
                }
            } else if let (Some((tid, start, total0, grid0)), Some(p)) = (self.drag.resizing_table.clone(), pointer) {
                let new_total = total0 + (p.x - start.x) / sc;
                self.resize_table_width(tid, &grid0, new_total);
            } else if let (Some((tid, start, x0, y0, w_pt, h_pt)), Some(p)) = (self.drag.moving_table, pointer) {
                let (dx, dy) = (p.x - start.x, p.y - start.y);
                if !self.drag.moved && dx.hypot(dy) >= 2.0 {
                    // First real movement: float the table ("Around") if it wasn't.
                    if self.doc.tables.get(&tid).and_then(|t| t.anchor).is_none() {
                        self.set_table_wrap(tid, Some(Wrap::Side));
                    } else {
                        self.checkpoint(crate::app::EditKind::Other);
                    }
                    self.drag.moved = true;
                }
                if self.drag.moved {
                    let (pw, ph) = (self.doc.page.width, self.doc.page.height);
                    let nx = (x0 + dx / sc).clamp(0.0, (pw - w_pt).max(0.0));
                    let ny = (y0 + dy / sc).clamp(0.0, (ph - h_pt).max(0.0));
                    self.move_table_to_page(tid, nx, ny);
                    let edge = 40.0;
                    let sy = if p.y < rect.top() + edge {
                        p.y - (rect.top() + edge)
                    } else if p.y > rect.bottom() - edge {
                        p.y - (rect.bottom() - edge)
                    } else {
                        0.0
                    };
                    if sy != 0.0 {
                        ui.scroll_with_delta(vec2(0.0, sy.clamp(-24.0, 24.0)));
                    }
                }
            } else if self.drag.selecting {
                if let Some(p) = pointer {
                    let (_, pos) = self.hit_point(rect, lay, p);
                    if let Some((a, b)) = self.drag.word_mode {
                        let (wa, wb) = self.doc.word_at(pos);
                        if pos < a {
                            self.anchor = b;
                            self.cursor = wa;
                        } else {
                            self.anchor = a;
                            self.cursor = wb.max(b);
                        }
                    } else {
                        self.cursor = pos;
                    }
                    self.blink = std::time::Instant::now();
                    if !ui.clip_rect().contains(p) {
                        self.scroll_to_caret = true;
                    }
                }
            }
        }

        if released {
            if self.drag.selecting && self.format_painter.is_some() {
                self.apply_format_painter();
            }
            self.drag = Default::default();
        }

        resp.context_menu(|ui| {
            ui.set_min_width(180.0);
            let t = theme::tokens_for(ui);
            if let Some((ws, we, sugg)) = self.ctx_word.clone() {
                if sugg.is_empty() {
                    ui.label(egui::RichText::new("(no spelling suggestions)").italics().color(t.dialog_fg));
                }
                for s in &sugg {
                    if ui.button(egui::RichText::new(s).color(t.dialog_fg)).clicked() {
                        self.replace_word(ws, we, s);
                        self.ctx_word = None;
                        ui.close();
                    }
                }
                let word = self.doc.plain_text(ws, we);
                if ui.button("Ignore All").clicked() {
                    self.spell.ignore_all(&word);
                    self.ctx_word = None;
                    ui.close();
                }
                if ui.button("Add to Dictionary").clicked() {
                    self.spell.add_to_dictionary(&word);
                    self.ctx_word = None;
                    ui.close();
                }
                ui.separator();
            }
            if ui.add_enabled(self.has_sel(), egui::Button::new("✂  Cut").shortcut_text("Ctrl+X")).clicked() {
                self.cut();
                ui.close();
            }
            if ui.add_enabled(self.has_sel(), egui::Button::new("🗐  Copy").shortcut_text("Ctrl+C")).clicked() {
                self.copy();
                ui.close();
            }
            if ui.add(egui::Button::new("📋  Paste").shortcut_text("Ctrl+V")).clicked() {
                self.paste_from_system();
                ui.close();
            }
            ui.separator();
            let csv = self.csv_rows_selection().is_some();
            let convert = ui.add_enabled(csv, egui::Button::new("▦  Convert to Table"));
            if show_tip(ui, convert, "Turn the selected comma/tab separated text into a table").clicked() {
                self.insert_table_from_csv();
                ui.close();
            }
            ui.separator();
            if ui.add(egui::Button::new("Select All").shortcut_text("Ctrl+A")).clicked() {
                let e = self.doc.end();
                self.select(Pos::default(), e);
                ui.close();
            }
            ui.separator();
            if ui.button("Paragraph…").clicked() {
                self.dlg.paragraph = true;
                ui.close();
            }
            ui.menu_button("Bullets / Numbering", |ui| {
                if ui.button("• Bullets").clicked() {
                    self.toggle_list(ListKind::Bullet);
                    ui.close();
                }
                if ui.button("1. Numbering").clicked() {
                    self.toggle_list(ListKind::Number);
                    ui.close();
                }
            });
            if ui.button("Insert Picture…").clicked() {
                ui.close();
                self.tab = Tab::Insert;
                self.insert_picture();
            }
        });
    }

    // ================================================================ painting

    fn texture(&mut self, ctx: &egui::Context, id: u32) -> Option<egui::TextureId> {
        if !self.textures.contains_key(&id) {
            let img = [self.main_doc(), &self.doc]
                .into_iter()
                .find_map(|d| d.all_images().into_iter().find(|(k, _)| *k == id).map(|(_, v)| v.clone()))?;
            let rgba = image::load_from_memory(&img.bytes).ok()?.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let ci = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            let tex = ctx.load_texture(format!("img{id}"), ci, egui::TextureOptions::LINEAR);
            self.textures.insert(id, tex);
        }
        self.textures.get(&id).map(|t| t.id())
    }

    fn paint(&mut self, ui: &Ui, rect: Rect, lay: &Layout) {
        let painter = ui.painter().clone();
        let clip = ui.clip_rect();
        let (sa, sb) = self.sel();
        let has_sel = self.has_sel();
        let t = self.theme.tokens;
        let dark = theme::is_dark_sheet(&self.theme);
        let sel_color = with_alpha(t.selection_bg, if dark { 140 } else { 255 });
        let search_color = with_alpha(t.search_highlight, if dark { 115 } else { 230 });
        let search_current = with_alpha(t.warning, if dark { 150 } else { 220 });
        let mark_color = t.chrome_icon;
        let caret_on = (self.blink.elapsed().as_secs_f32() % 1.06) < 0.53;
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(265));

        if self.find.open && !self.find.query.is_empty() {
            self.refresh_find();
        }
        let matches = if self.find.open { self.find.matches.clone() } else { Vec::new() };
        let mut ids: Vec<u32> = self.main_doc().all_images().into_iter().map(|(k, _)| k).collect();
        ids.extend(self.doc.images.keys());
        for id in ids {
            self.texture(ui.ctx(), id);
        }
        let textures: HashMap<u32, egui::TextureId> = self.textures.iter().map(|(k, v)| (*k, v.id())).collect();
        let sc = lay.scale;
        // While a header/footer is edited, `lay` is its layout and the page is drawn from the body's.
        let story = self.story.as_deref().and_then(|s| Some((s, s.body_layout.as_ref()?)));
        let (pdoc, play) = match story {
            Some((s, bl)) => (&s.body, bl),
            None => (&self.doc, lay),
        };
        let comment_ranges = if self.show_comments { play.comment_ranges.clone() } else { Vec::new() };

        for page in 0..lay.pages {
            let o = self.page_origin(rect, lay, page);
            let lg = lay.geom(page);
            let pr = Rect::from_min_size(o, vec2(lg.w, lg.h));
            if !pr.intersects(clip) {
                continue;
            }
            painter.rect_filled(pr.translate(vec2(2.0, 3.0)), 0.0, t.sheet_shadow);
            painter.rect_filled(pr, 0.0, t.sheet_bg);
            painter.rect_stroke(pr, 0.0, Stroke::new(1.0, t.sheet_border), StrokeKind::Outside);

            // Margin guides (corner marks, like Word).
            let pg = play.geom(page);
            let [mt, mr, mb, ml] = pg.margins;
            let guide = Stroke::new(1.0, t.margin_guide);
            for (cx, cy, dx, dy) in [
                (ml, mt, -1.0, -1.0),
                (pg.w - mr, mt, 1.0, -1.0),
                (ml, pg.h - mb, -1.0, 1.0),
                (pg.w - mr, pg.h - mb, 1.0, 1.0),
            ] {
                let c = o + vec2(cx, cy);
                painter.line_segment([c, c + vec2(dx * 12.0, 0.0)], guide);
                painter.line_segment([c, c + vec2(0.0, dy * 12.0)], guide);
            }
            // Gridlines for borderless tables, like Word's "View Gridlines".
            for cell in play.cells.iter().filter(|c| c.page == page && !c.edges.iter().any(crate::doc::Border::visible)) {
                let r = Rect::from_min_size(o + vec2(cell.x, cell.y), vec2(cell.w, cell.h));
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, t.grid_line), StrokeKind::Inside);
            }
            // Move handle (top-left) for floating tables and the selected table.
            {
                let sel_table = self.selected_table();
                let mut tids: Vec<u32> = play.cells.iter().filter(|c| c.page == page).map(|c| c.table).filter(|t| *t != 0).collect();
                tids.sort_unstable();
                tids.dedup();
                for tid in tids {
                    let floating = self.doc.tables.get(&tid).and_then(|t| t.anchor).is_some();
                    if !floating && sel_table != Some(tid) {
                        continue;
                    }
                    let cells: Vec<&layout::CellBox> = play.cells.iter().filter(|c| c.table == tid).collect();
                    if cells.is_empty() {
                        continue;
                    }
                    let x0 = cells.iter().map(|c| c.x).fold(f32::MAX, f32::min);
                    let y0 = cells.iter().map(|c| c.y).fold(f32::MAX, f32::min);
                    if sel_table == Some(tid) {
                        let x1 = cells.iter().map(|c| c.x + c.w).fold(f32::MIN, f32::max);
                        let y1 = cells.iter().map(|c| c.y + c.h).fold(f32::MIN, f32::max);
                        let r = Rect::from_min_max(o + vec2(x0, y0), o + vec2(x1, y1));
                        painter.rect_stroke(r, 0.0, Stroke::new(1.0, t.chrome_accent), StrokeKind::Outside);
                    }
                    let blue = t.chrome_accent;
                    let h = Rect::from_center_size(o + vec2(x0, y0), vec2(14.0, 14.0));
                    painter.rect_filled(h, 2.0, t.sheet_bg);
                    painter.rect_stroke(h, 2.0, Stroke::new(1.0, blue), StrokeKind::Inside);
                    painter.text(h.center(), egui::Align2::CENTER_CENTER, "✥", egui::FontId::proportional(11.0), blue);
                    // Resize handle at the bottom-right corner.
                    let x1 = cells.iter().map(|c| c.x + c.w).fold(f32::MIN, f32::max);
                    let y1 = cells.iter().map(|c| c.y + c.h).fold(f32::MIN, f32::max);
                    let hr = Rect::from_center_size(o + vec2(x1, y1), vec2(12.0, 12.0));
                    painter.rect_filled(hr, 0.0, t.sheet_bg);
                    painter.rect_stroke(hr, 0.0, Stroke::new(1.0, blue), StrokeKind::Inside);
                    painter.text(hr.center(), egui::Align2::CENTER_CENTER, "◢", egui::FontId::proportional(10.0), blue);
                }
            }

            let mut canvas = EguiCanvas { painter: &painter, origin: o, textures: &textures, sheet: if dark { Some(t) } else { None } };
            if let Some((st, _)) = story {
                render::draw_page(&mut canvas, pdoc, play, page, |_, _| {});
                let veil = with_alpha(t.sheet_bg, 140);
                // The band of the page showing the story stays bright; the rest is veiled.
                let band = match &st.target {
                    Target::Hf { section, variant } if play.hf_slot(pdoc, *variant < 2, page) == (*section, *variant) => {
                        Some(if *variant < 2 { (0.0, pg.margins[0]) } else { (pg.h - pg.margins[2], pg.h) })
                    }
                    Target::Note(_) | Target::Nested(_) if page == st.page && !lay.lines.is_empty() => {
                        let t = lay.lines.iter().map(|l| l.y).fold(f32::MAX, f32::min);
                        let b = lay.lines.iter().map(|l| l.y + l.height).fold(0.0f32, f32::max);
                        Some((t - 4.0, b + 4.0))
                    }
                    _ => None,
                };
                let (band_t, band_b) = band.unwrap_or((0.0, 0.0));
                for (y0, y1) in [(0.0, band_t), (band_b, lg.h)] {
                    if y1 > y0 {
                        painter.rect_filled(Rect::from_min_max(o + vec2(0.0, y0), o + vec2(lg.w, y1)), 0.0, veil);
                    }
                }
                if band.is_some() {
                    let blue = t.chrome_accent;
                    for y in [band_t, band_b] {
                        if y <= 0.0 || y >= lg.h {
                            continue;
                        }
                        let mut x = 0.0;
                        while x < lg.w {
                            painter.line_segment([o + vec2(x, y), o + vec2((x + 4.0).min(lg.w), y)], Stroke::new(1.0, blue));
                            x += 7.0;
                        }
                    }
                    let font = egui::FontId::proportional(11.0);
                    let galley = painter.layout_no_wrap(st.name(), font, t.chrome_accent_fg);
                    let size = galley.size() + vec2(10.0, 4.0);
                    let tag_y = if band_t <= 0.0 { band_b } else { band_t - size.y };
                    let tag = Rect::from_min_size(o + vec2(pg.margins[3], tag_y), size);
                    painter.rect_filled(tag, 2.0, blue);
                    painter.galley(tag.min + vec2(5.0, 2.0), galley, t.chrome_accent_fg);
                }
                // Selection in the header.
                if has_sel {
                    for li in lay.page_lines[page].clone() {
                        let l = &lay.lines[li];
                        if l.para < sa.p || l.para > sb.p {
                            continue;
                        }
                        let s = if l.para == sa.p { sa.o } else { 0 }.max(l.start);
                        let e = if l.para == sb.p { sb.o } else { usize::MAX }.min(l.end);
                        if s > e {
                            continue;
                        }
                        let (x0, mut x1) = (l.xs[s - l.start], l.xs[e - l.start]);
                        if l.para < sb.p && l.last_in_para {
                            x1 += 6.0;
                        }
                        let r = Rect::from_min_max(o + vec2(x0, l.y), o + vec2(x1, l.y + l.height));
                        painter.rect_filled(r, 0.0, sel_color);
                    }
                }
            }
            let doc = &self.doc;
            let current = self.find.current;
            if story.is_none() {
            render::draw_page(&mut canvas, doc, lay, page, |cv, li| {
                let l = &lay.lines[li];
                let top = l.y;
                let x_of = |k: usize| l.xs[k - l.start];
                // Search matches.
                for (i, (ma, mb)) in matches.iter().enumerate() {
                    if ma.p != l.para || mb.o <= l.start || ma.o >= l.end {
                        continue;
                    }
                    let s = ma.o.max(l.start);
                    let e = mb.o.min(l.end);
                    let col = if current == Some(i) { search_current } else { search_color };
                    if e > s {
                        cv.painter.rect_filled(
                            Rect::from_min_max(cv.origin + vec2(x_of(s), top), cv.origin + vec2(x_of(e), top + l.height)),
                            0.0,
                            col,
                        );
                    }
                }
                // Commented text.
                for (_, cs, ce) in comment_ranges.iter() {
                    if l.para < cs.p || l.para > ce.p {
                        continue;
                    }
                    let s = if l.para == cs.p { cs.o } else { 0 }.max(l.start);
                    let e = if l.para == ce.p { ce.o } else { usize::MAX }.min(l.end);
                    if s < e {
                        cv.painter.rect_filled(
                            Rect::from_min_max(cv.origin + vec2(x_of(s), top), cv.origin + vec2(x_of(e), top + l.height)),
                            0.0,
                            search_color,
                        );
                    }
                }
                // Selection.
                if has_sel && l.para >= sa.p && l.para <= sb.p {
                    let s = if l.para == sa.p { sa.o } else { 0 }.max(l.start);
                    let e = if l.para == sb.p { sb.o } else { usize::MAX }.min(l.end);
                    let extends = l.para < sb.p && l.last_in_para;
                    if s <= e {
                        let x0 = x_of(s);
                        let mut x1 = x_of(e);
                        if extends {
                            x1 += 6.0;
                        }
                        if x1 > x0 {
                            cv.painter.rect_filled(
                                Rect::from_min_max(cv.origin + vec2(x0, top), cv.origin + vec2(x1, top + l.height)),
                                0.0,
                                sel_color,
                            );
                        }
                    }
                }
            });
            }

            for li in lay.page_lines[page].clone() {
                let l = &lay.lines[li];
                if l.page != page {
                    continue;
                }
                let para = match self.doc.paras.get(l.para) {
                    Some(p) => p,
                    None => continue,
                };
                let base = o.y + l.y + l.baseline;
                let x_of = |k: usize| o.x + l.xs[k - l.start];
                // Spelling: red wavy underline.
                for (s, e) in self.spell.misspelled(para) {
                    let (s, e) = (s.max(l.start), e.min(l.end));
                    if s >= e {
                        continue;
                    }
                    let (x0, x1) = (x_of(s), x_of(e));
                    let y = base + 2.0 * sc.max(0.8);
                    let amp = 1.4;
                    let step = 2.5;
                    let mut pts = Vec::new();
                    let mut x = x0;
                    let mut up = true;
                    while x < x1 {
                        pts.push(pos2(x, if up { y - amp } else { y + amp }));
                        x += step;
                        up = !up;
                    }
                    pts.push(pos2(x1, y));
                    painter.add(egui::Shape::line(pts, Stroke::new(1.0, t.error)));
                }
                // Selected picture: frame and resize handle.
                for k in l.start..l.end {
                    let Some(&f) = para.fmts.get(k) else { break };
                    if f.image == 0 || !(has_sel && sa == Pos::new(l.para, k) && sb == Pos::new(l.para, k + 1)) {
                        continue;
                    }
                    if let Some((pg, ir)) = self.image_box(lay, Pos::new(l.para, k)) {
                        let po = self.page_origin(rect, lay, pg);
                        let r = ir.translate(po.to_vec2());
                        let _ = base;
                        let blue = t.chrome_accent;
                        painter.rect_stroke(r, 0.0, Stroke::new(1.0, blue), StrokeKind::Outside);
                        let h = Rect::from_center_size(r.max, vec2(8.0, 8.0));
                        painter.rect_filled(h, 0.0, t.sheet_bg);
                        painter.rect_stroke(h, 0.0, Stroke::new(1.0, blue), StrokeKind::Inside);
                    }
                }
                // Formatting marks.
                if self.show_marks {
                    let mf = egui::FontId::proportional((11.0 * sc).max(6.0));
                    for k in l.start..l.end {
                        let (Some(&c), Some(&f)) = (para.chars.get(k), para.fmts.get(k)) else { break };
                        let (x0, x1) = (x_of(k), x_of(k + 1));
                        if f.hidden {
                            continue;
                        }
                        match c {
                            ' ' => {
                                painter.circle_filled(pos2((x0 + x1) / 2.0, base - 3.0 * sc), 1.2 * sc.max(1.0), mark_color);
                            }
                            '\t' => {
                                painter.text(pos2(x0, base), egui::Align2::LEFT_BOTTOM, "→", mf.clone(), mark_color);
                            }
                            LINE_BREAK => {
                                painter.text(pos2(x0, base), egui::Align2::LEFT_BOTTOM, "↵", mf.clone(), mark_color);
                            }
                            PAGE_BREAK => {
                                let r = o.x + lay.paras.get(l.para).map_or(l.xs.last().copied().unwrap_or(0.0), |p| p.right);
                                let left = pos2(x0 + 4.0, base - 4.0);
                                painter.line_segment([left, pos2(r, left.y)], Stroke::new(1.0, mark_color));
                                painter.text(
                                    pos2((left.x + r) / 2.0, left.y),
                                    egui::Align2::CENTER_CENTER,
                                    " Page Break ",
                                    mf.clone(),
                                    mark_color,
                                );
                            }
                            OBJ if f.obj != 0 => {
                                // Show where fields and bookmarks are.
                                if let Some(ob) = crate::pool::obj(f.obj) {
                                    let glyph = match ob.kind {
                                        crate::pool::ObjKind::FieldBegin => Some("{"),
                                        crate::pool::ObjKind::FieldEnd => Some("}"),
                                        crate::pool::ObjKind::Hidden if ob.xml.contains("bookmarkStart") => Some("["),
                                        _ => None,
                                    };
                                    if let Some(g) = glyph {
                                        painter.text(pos2(x0, base), egui::Align2::CENTER_BOTTOM, g, mf.clone(), t.sheet_fg_muted);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if l.last_in_para {
                        let x = x_of(l.end);
                        let mark = if para.cell.is_some() && self.doc.at_cell_end(Pos::new(l.para, para.len())) { "¤" } else { "¶" };
                        painter.text(pos2(x + 1.0, base), egui::Align2::LEFT_BOTTOM, mark, mf.clone(), mark_color);
                    }
                }
            }
        }

        // Comment cards beside the pages.
        if !comment_ranges.is_empty() {
            let mut next_free = f32::MIN;
            for (id, cs, _) in &comment_ranges {
                let Some(c) = pdoc.comments.get(id) else { continue };
                let (pg, x, y, _) = play.caret(*cs);
                let o = self.page_origin(rect, lay, pg);
                let anchor = o + vec2(x, y);
                let card_x = o.x + lay.geom(pg).w + GAP;
                let mut job = LayoutJob::default();
                job.wrap.max_width = COMMENT_W - 16.0;
                job.append(
                    &format!(
                        "{}{}\n",
                        if !c.author.is_empty() { &c.author } else if !c.initials.is_empty() { &c.initials } else { "Comment" },
                        c.date.get(..10).map(|d| format!("  ·  {d}")).unwrap_or_default()
                    ),
                    0.0,
                    TextFormat { font_id: egui::FontId::proportional(12.0), color: t.comment_card_fg, ..Default::default() },
                );
                job.append(&c.text, 0.0, TextFormat { font_id: egui::FontId::proportional(12.0), color: t.comment_card_fg, ..Default::default() });
                let galley = painter.layout_job(job);
                let h = galley.size().y + 14.0;
                let top = anchor.y.max(next_free);
                next_free = top + h + 6.0;
                let card = Rect::from_min_size(pos2(card_x, top), vec2(COMMENT_W, h));
                if !card.intersects(clip) {
                    continue;
                }
                painter.line_segment([anchor, pos2(card_x, top + 10.0)], Stroke::new(1.0, t.comment_card_border));
                painter.rect_filled(card, 4.0, t.comment_card_bg);
                painter.rect_stroke(card, 4.0, Stroke::new(1.0, t.comment_card_border), StrokeKind::Inside);
                painter.galley(card.min + vec2(8.0, 7.0), galley, t.comment_card_fg);
            }
        }

        // Caret.
        if caret_on && !self.text_input_focused && (!has_sel || self.drag.selecting) && self.selected_image().is_none() {
            let (pg, x, y, h) = lay.caret(self.cursor);
            let p = &self.doc.paras[self.cursor.p];
            let ls = if p.line_rule == crate::doc::LineRule::Auto { p.line_spacing.max(1.0) } else { 1.0 };
            let o = self.page_origin(rect, lay, pg);
            let ch = h / ls;
            let top = o + vec2(x, y);
            let cw = if dark { 2.0 } else { 1.5 };
            painter.line_segment([top, top + vec2(0.0, ch)], Stroke::new(cw, t.caret));
        }
    }

    pub fn ruler(&mut self, ui: &mut Ui) {
        let cw = self.comments_width();
        let Some(lay) = &self.layout else { return };
        let sc = lay.scale;
        let cur_page = lay.caret(self.cursor).0;
        let rg = lay.geom(cur_page);
        let page_w = rg.w;
        let [_, mr, _, ml] = rg.margins;
        // Current paragraph's laid-out box (this already accounts for columns and
        // tables); fall back to the margins when it is not available.
        let (pb_left, pb_right) = lay
            .paras
            .get(self.cursor.p)
            .map(|pb| (pb.left, pb.right))
            .unwrap_or((ml, page_w - mr));
        let (ind, first) = {
            let p = &self.doc.paras[self.cursor.p];
            ((p.ind_left, p.ind_right, p.ind_first), pb_left + p.ind_first * sc)
        };
        let left = pb_left;
        let right = pb_right.max(left);
        // Right column edge; fall back to the margin if the box was clamped.
        let col_right = if pb_right > left { right + ind.1 * sc } else { page_w - mr };

        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click_and_drag());
        let p = ui.painter_at(rect);
        let t = self.theme.tokens;
        p.rect_filled(rect, 0.0, t.ruler_bg);
        let total_w = (page_w + 2.0 * GAP + cw).max(rect.width());
        // Match the page origin of the scroll area so markers track the page when
        // the view is zoomed or horizontally scrolled.
        let x0 = rect.left() + ((total_w - cw - page_w) / 2.0).max(GAP) - self.canvas_scroll_x;
        let col_left_pt = left - ind.0 * sc;
        let tabs: Vec<(usize, f32, TabKind)> = self.doc.paras[self.cursor.p]
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (i, x0 + col_left_pt + t.pos * sc, t.kind))
            .collect();
        let page = Rect::from_min_max(pos2(x0, rect.top() + 3.0), pos2(x0 + page_w, rect.bottom() - 3.0));
        p.rect_filled(page, 0.0, t.ruler_page_band);
        let content = Rect::from_min_max(pos2(x0 + ml, page.top()), pos2(x0 + page_w - mr, page.bottom()));
        p.rect_filled(content, 0.0, t.ruler_content);
        let inch = 72.0 * sc;
        let tick = t.ruler_tick;
        let n = (page_w / (inch / 8.0)) as i32;
        for i in -((ml / (inch / 8.0)) as i32)..=n {
            let x = content.left() + i as f32 * inch / 8.0;
            if x < page.left() || x > page.right() {
                continue;
            }
            if i % 8 == 0 {
                if i != 0 {
                    p.text(
                        pos2(x, page.center().y),
                        egui::Align2::CENTER_CENTER,
                        format!("{}", (i / 8).abs()),
                        egui::FontId::proportional(10.0),
                        tick,
                    );
                }
            } else {
                let h = if i % 4 == 0 { 5.0 } else if i % 2 == 0 { 3.0 } else { 0.0 };
                if h > 0.0 {
                    p.line_segment([pos2(x, page.center().y - h / 2.0), pos2(x, page.center().y + h / 2.0)], Stroke::new(1.0, tick));
                }
            }
        }

        // Indent markers for the current paragraph: first line (down), left and
        // right (up).
        let blue = t.ruler_indent_marker;
        let active = self.drag.ruler.as_ref().map(|d| d.mark);
        let tri = |x: f32, down: bool| {
            let y = if down { page.top() } else { page.bottom() };
            let d = if down { 5.0 } else { -5.0 };
            vec![pos2(x0 + x - 4.0, y), pos2(x0 + x + 4.0, y), pos2(x0 + x, y + d)]
        };
        for (mark, x, down) in [(RulerMark::First, first, true), (RulerMark::Left, left, false), (RulerMark::Right, right, false)] {
            let fill = if active == Some(mark) { blue } else { t.ruler_content };
            p.add(egui::Shape::convex_polygon(tri(x, down), fill, Stroke::new(1.0, blue)));
        }
        // Custom tab stops sit just above the bottom edge so they do not hide the
        // right indent marker. The letter identifies the alignment type.
        for (i, tx, kind) in &tabs {
            if *tx < page.left() - 1.0 || *tx > page.right() + 1.0 {
                continue;
            }
            let (label, col) = match kind {
                TabKind::Center => ("C", blue),
                TabKind::Right => ("R", blue),
                TabKind::Decimal => ("D", blue),
                TabKind::Bar => ("|", blue),
                _ => ("L", blue),
            };
            let col = if active == Some(RulerMark::Tab(*i)) { t.chrome_accent } else { col };
            p.text(pos2(*tx, page.bottom() - 1.0), egui::Align2::CENTER_BOTTOM, label, egui::FontId::monospace(9.0), col);
        }

        // Hit-test and drag the markers.
        let (pointer, pressed, down, alt) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.alt));
        let hit = pointer.filter(|px| rect.contains(*px)).and_then(|px| {
            let mut cands = vec![
                (RulerMark::First, x0 + first, page.top() + 2.0),
                (RulerMark::Left, x0 + left, page.bottom() - 2.0),
                (RulerMark::Right, x0 + right, page.bottom() - 2.0),
            ];
            cands.extend(tabs.iter().map(|(i, tx, _)| (RulerMark::Tab(*i), *tx, page.bottom() - 6.0)));
            cands
                .into_iter()
                .map(|(m, cx, cy)| (m, (px.x - cx).powi(2) + (px.y - cy).powi(2)))
                .filter(|(_, d2)| *d2 <= 9.0 * 9.0)
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .map(|(m, _)| m)
        });
        if hit.is_some() || self.drag.ruler.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }

        if self.drag.ruler.is_none() && pressed {
            let tab_pos = match hit {
                Some(RulerMark::Tab(i)) => self.doc.paras[self.cursor.p].tabs.get(i).map(|t| t.pos).unwrap_or(0.0),
                _ => 0.0,
            };
            if let (Some(px), Some(mark)) = (pointer, hit) {
                self.drag.ruler = Some(crate::app::RulerDrag {
                    mark,
                    start_x: px.x,
                    base: ind,
                    col: (col_left_pt, col_right),
                    tab_pos,
                });
                self.checkpoint(crate::app::EditKind::Other);
                // Grabbing a marker must not scroll the page under the pointer.
                self.scroll_to_caret = false;
            } else if let Some(px) = pointer.filter(|px| content.contains(*px)) {
                // Clicking the empty ruler adds an editable tab stop of the
                // default (left) type, then lets the same drag move it.
                let pos = ((((px.x - (x0 + col_left_pt)) / sc) / 9.0).round() * 9.0).clamp(0.0, 720.0);
                self.add_tab_stop(pos, TabKind::Left, Leader::None);
                let i = self.doc.paras[self.cursor.p]
                    .tabs
                    .iter()
                    .position(|t| (t.pos - pos).abs() < 0.5)
                    .unwrap_or(0);
                self.drag.ruler = Some(crate::app::RulerDrag {
                    mark: RulerMark::Tab(i),
                    start_x: px.x,
                    base: ind,
                    col: (col_left_pt, col_right),
                    tab_pos: pos,
                });
                self.scroll_to_caret = false;
            }
        }

        if let Some(d) = self.drag.ruler {
            if !down {
                self.drag.ruler = None;
            } else if let Some(px) = pointer {
                let (base_l, base_r, base_f) = d.base;
                let snap = |v: f32| if alt { v } else { (v / 9.0).round() * 9.0 };
                let (col_l, col_r) = d.col;
                let delta = (px.x - d.start_x) / sc;
                let mut moved_tab = None;
                let (l, r, f) = match d.mark {
                    RulerMark::Left => {
                        let mut l = snap(base_l + delta).clamp(-360.0, 720.0);
                        // Moving the body carries the first line with it, but the
                        // first line may not cross the column start (unless the
                        // paragraph already starts outside it).
                        let min_l = -base_f;
                        if base_l >= min_l {
                            l = l.max(min_l);
                        }
                        // Keep the paragraph's text area at least 1pt wide.
                        l = l.min((col_r - base_r * sc - sc - col_l) / sc);
                        (l, base_r, base_f)
                    }
                    RulerMark::Right => {
                        let mut r = snap(base_r - delta).clamp(-360.0, 720.0);
                        r = r.min((col_r - (col_l + base_l * sc) - sc) / sc);
                        (base_l, r, base_f)
                    }
                    RulerMark::First => {
                        let mut f = snap(base_f + delta).clamp(-360.0, 720.0);
                        let new_left = col_l + base_l * sc;
                        let new_right = (col_r - base_r * sc).max(new_left);
                        let lo = (col_l - new_left) / sc;
                        let hi = ((new_right - new_left) / sc).max(lo);
                        f = f.clamp(lo, hi);
                        (base_l, base_r, f)
                    }
                    RulerMark::Tab(i) => {
                        let p = &self.doc.paras[self.cursor.p];
                        let lo = if i > 0 { p.tabs[i - 1].pos + 0.5 } else { 0.0 };
                        let hi = if i + 1 < p.tabs.len() {
                            p.tabs[i + 1].pos - 0.5
                        } else {
                            ((col_r - col_l) / sc).max(lo)
                        };
                        moved_tab = Some(snap(d.tab_pos + delta).clamp(lo, hi.max(lo)));
                        (base_l, base_r, base_f)
                    }
                };
                if let Some(np) = moved_tab {
                    let p = &mut self.doc.paras[self.cursor.p];
                    if let Some(t) = p.tabs.get_mut(match d.mark {
                        RulerMark::Tab(i) => i,
                        _ => 0,
                    }) {
                        if (t.pos - np).abs() > 0.01 {
                            t.pos = np;
                            self.rev += 1;
                            self.dirty = true;
                            self.set_status(format!("Tab stop: {:.2}\"", np / 72.0));
                        }
                    }
                } else if (l, r, f) != d.base {
                    let p = &mut self.doc.paras[self.cursor.p];
                    match d.mark {
                        RulerMark::Left => p.ind_left = l,
                        RulerMark::Right => p.ind_right = r,
                        RulerMark::First => p.ind_first = f,
                        RulerMark::Tab(_) => {}
                    }
                    self.rev += 1;
                    self.dirty = true;
                    let (label, val) = match d.mark {
                        RulerMark::Left => ("Left indent", l),
                        RulerMark::Right => ("Right indent", r),
                        RulerMark::First => ("First line", f),
                        RulerMark::Tab(_) => ("Tab stop", 0.0),
                    };
                    self.set_status(format!("{label}: {:.2}\"", val / 72.0));
                }
            }
        }

        // Right-click a stop to remove it, or empty ruler space to choose the
        // alignment/leader of a new stop.
        resp.context_menu(|ui| {
            let px = ui.ctx().input(|i| i.pointer.hover_pos());
            let near = px.and_then(|px| {
                tabs.iter()
                    .map(|(i, tx, _)| (*i, *tx, (px.x - *tx).abs()))
                    .filter(|(_, _, dx)| *dx <= 8.0 && (px.y - (page.bottom() - 6.0)).abs() <= 12.0)
                    .min_by(|a, b| a.2.total_cmp(&b.2))
            });
            if let Some((i, _, _)) = near {
                if ui.button("Remove Tab Stop").clicked() {
                    let pos = self.doc.paras[self.cursor.p].tabs.get(i).map(|t| t.pos).unwrap_or(0.0);
                    self.remove_tab_stop(pos);
                    ui.close();
                }
                ui.separator();
            }
            let pos = px.filter(|px| content.contains(*px)).map(|px| {
                ((((px.x - (x0 + col_left_pt)) / sc) / 9.0).round() * 9.0).clamp(0.0, 720.0)
            });
            let Some(pos) = pos else {
                ui.label("Point inside the ruler content area");
                return;
            };
            for (kind, label) in [
                (TabKind::Left, "Add Left Tab"),
                (TabKind::Center, "Add Center Tab"),
                (TabKind::Right, "Add Right Tab"),
                (TabKind::Decimal, "Add Decimal Tab"),
                (TabKind::Bar, "Add Bar Tab"),
            ] {
                if ui.button(label).clicked() {
                    self.add_tab_stop(pos, kind, Leader::None);
                    ui.close();
                }
            }
            ui.separator();
            for (leader, label) in [
                (Leader::Dot, "Add Dotted Leader"),
                (Leader::Hyphen, "Add Hyphen Leader"),
                (Leader::Underscore, "Add Underline Leader"),
            ] {
                if ui.button(label).clicked() {
                    self.add_tab_stop(pos, TabKind::Left, leader);
                    ui.close();
                }
            }
        });
    }

    pub fn apply_fit(&mut self, avail: Vec2) {
        if let Some(page_width) = self.fit_request.take() {
            let pw = self.doc.page.width * layout::PT_TO_PX;
            let ph = self.doc.page.height * layout::PT_TO_PX;
            let z = if page_width {
                (avail.x - 2.0 * GAP - 20.0) / pw
            } else {
                ((avail.y - 2.0 * GAP) / ph).min((avail.x - 2.0 * GAP) / pw)
            };
            self.zoom = z.clamp(0.1, 5.0);
        }
    }
}

/// Screen implementation of the shared page renderer.
///
/// Document colours are remapped for a dark sheet here — at the display layer
/// only — so the stored colours (and the PDF) are never touched.
struct EguiCanvas<'a> {
    painter: &'a egui::Painter,
    origin: Pos2,
    textures: &'a HashMap<u32, egui::TextureId>,
    /// `Some` when the sheet is dark: document colours get remapped for it.
    sheet: Option<theme::Tokens>,
}

impl EguiCanvas<'_> {
    /// Maps a document colour to its on-screen colour (remapped on dark sheets).
    fn col(&self, c: Rgba) -> Color32 {
        let c = c32(c);
        match &self.sheet {
            Some(t) => theme::remap_for_dark_sheet(c, t),
            None => c,
        }
    }
}

impl render::Canvas for EguiCanvas<'_> {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        let r = Rect::from_min_size(self.origin + vec2(x, y), vec2(w, h));
        let col = self.col(c);
        self.painter.rect_filled(r, 0.0, col);
    }

    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) {
        let col = self.col(c);
        self.painter.line_segment([self.origin + vec2(x0, y0), self.origin + vec2(x1, y1)], Stroke::new(width, col));
    }

    fn text(&mut self, x: f32, y: f32, text: &str, f: &crate::doc::CharFmt, size: f32) {
        let r = crate::fonts::resolve(f.font, f.bold, f.italic);
        let font_id = egui::FontId::new(size.max(1.0), r.family);
        let color = self.col(f.color);
        let job = LayoutJob::single_section(text.to_string(), TextFormat { font_id, color, italics: r.fake_italic, ..Default::default() });
        let galley = self.painter.layout_job(job);
        let asc = galley.rows.first().and_then(|row| row.row.glyphs.first().map(|g| row.pos.y + g.pos.y)).unwrap_or(size * 0.8);
        let pos = self.origin + vec2(x, y - asc);
        if r.fake_bold {
            self.painter.galley(pos + vec2((size * 0.04).max(0.6), 0.0), galley.clone(), color);
        }
        self.painter.galley(pos, galley, color);
    }

    fn image(&mut self, id: u32, x: f32, y: f32, w: f32, h: f32) {
        let r = Rect::from_min_size(self.origin + vec2(x, y), vec2(w, h));
        match self.textures.get(&id) {
            Some(t) => {
                // No tint: document bitmaps are never inverted or remapped.
                self.painter.image(*t, r, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), theme::NO_TINT);
            }
            None => {
                self.painter.rect_filled(r, 0.0, self.sheet.map(|t| t.sheet_border).unwrap_or(theme::light().sheet_border));
            }
        }
    }
}

/// Page-relative bounds (page, x, y, w, h) of a laid-out table, from the cells.
fn table_bounds_in(lay: &Layout, table: u32) -> Option<(usize, f32, f32, f32, f32)> {
    let cells: Vec<&layout::CellBox> = lay.cells.iter().filter(|c| c.table == table).collect();
    if cells.is_empty() {
        return None;
    }
    let page = cells[0].page;
    let x0 = cells.iter().map(|c| c.x).fold(f32::MAX, f32::min);
    let x1 = cells.iter().map(|c| c.x + c.w).fold(f32::MIN, f32::max);
    let y0 = cells.iter().map(|c| c.y).fold(f32::MAX, f32::min);
    let y1 = cells.iter().map(|c| c.y + c.h).fold(f32::MIN, f32::max);
    Some((page, x0, y0, x1 - x0, y1 - y0))
}
