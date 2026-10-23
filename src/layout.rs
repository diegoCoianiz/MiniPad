//! Line breaking, table layout and pagination.
//!
//! All coordinates produced here are in screen points relative to the top-left
//! corner of the page the line lives on.

use crate::doc::{
    self, Align, CharFmt, COLUMN_BREAK, Document, LINE_BREAK, Leader, LineRule, PAGE_BREAK,
    PageSetup, Pos, Rgba, TabKind, VMerge,
};
use crate::fonts;
use crate::pool::{self, ObjKind};
use eframe::egui::{Color32, FontId, epaint::text::FontsView};
use std::collections::HashMap;
use std::ops::Range;

pub const PT_TO_PX: f32 = 96.0 / 72.0;

/// Whether tracked changes are shown with markup (insertions coloured,
/// deletions struck through) rather than as the final text.
pub static SHOW_MARKUP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn show_markup() -> bool {
    SHOW_MARKUP.load(std::sync::atomic::Ordering::Relaxed)
}
const SCRIPT_SCALE: f32 = 0.65;
const SMALL_CAPS_SCALE: f32 = 0.8;

#[derive(Clone)]
pub struct Line {
    pub para: usize,
    pub start: usize,
    pub end: usize,
    pub page: usize,
    pub y: f32,
    pub height: f32,
    /// Baseline offset from `y`.
    pub baseline: f32,
    /// Caret x positions for each boundary in `start..=end`.
    pub xs: Vec<f32>,
    pub last_in_para: bool,
    /// Line was ended by a manual line or page break character.
    pub forced: bool,
    /// Tab characters (paragraph offsets) drawn with a leader.
    pub leaders: Vec<(usize, Leader)>,
    /// Bar-tab positions (page points) drawn as vertical rules on this line.
    pub bars: Vec<f32>,
    /// Extra space of multiple line spacing below the text; at the bottom of a
    /// page it may hang into the margin, as in Word.
    pub leading: f32,
}

#[derive(Clone)]
pub struct ParaBox {
    pub lines: Range<usize>,
    /// Horizontal extent of the paragraph's text area.
    pub left: f32,
    pub right: f32,
    pub marker: Option<(String, f32)>,
}

/// Page size and margins for one page, in screen points.
#[derive(Clone, Copy, Debug)]
pub struct PageGeom {
    pub w: f32,
    pub h: f32,
    /// top, right, bottom, left
    pub margins: [f32; 4],
    pub header_dist: f32,
    pub footer_dist: f32,
}

/// A table cell's rectangle on a page.
#[derive(Clone)]
pub struct CellBox {
    pub page: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub shade: Option<Rgba>,
    /// top, right, bottom, left
    pub edges: [Option<doc::Border>; 4],
    /// Owning table id (0 = none); used for hit-testing and moving a whole table.
    pub table: u32,
}

#[derive(Clone)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub paras: Vec<ParaBox>,
    pub page_lines: Vec<Range<usize>>,
    pub cells: Vec<CellBox>,
    pub pages: usize,
    pub scale: f32,
    pub page_w: f32,
    pub page_h: f32,
    /// top, right, bottom, left in screen points.
    pub margins: [f32; 4],
    /// Per-page geometry (sections may change size, orientation and margins).
    pub page_geoms: Vec<PageGeom>,
    /// Top of each page in the stacked page view, including the gaps.
    pub page_tops: Vec<f32>,
    /// Display text for objects that show computed labels (footnote numbers...).
    pub obj_text: HashMap<(usize, usize), String>,
    /// Header text and its width.
    pub header: (String, f32),
    /// Footer text and width for each page.
    pub footers: Vec<(String, f32)>,
    /// Rich header/footer layouts per (section, variant).
    pub hf: Vec<HfBox>,
    /// Section index of each page (the section in effect at the top of the page).
    pub page_section: Vec<usize>,
    /// First page of each section.
    pub section_first_page: Vec<usize>,
    /// Whether rich (laid out) headers / footers are used instead of plain text.
    pub rich_header: bool,
    pub rich_footer: bool,
    /// PAGE / NUMPAGES field results: (paragraph, start, end, is NUMPAGES).
    pub page_fields: Vec<(usize, usize, usize, bool)>,
    /// Footnotes (bottom of their page) and endnotes (after the text).
    pub notes: Vec<NoteBox>,
    /// Note separator lines: (page, y).
    pub note_separators: Vec<(usize, f32)>,
    /// Floating pictures and shapes.
    pub floats: Vec<FloatBox>,
    /// Nested tables: (paragraph, page, x, y, layout).
    pub nested: Vec<(usize, usize, f32, f32, Box<Layout>)>,
    /// Tracked insertions: (paragraph, start, end).
    pub ins_ranges: Vec<(usize, usize, usize)>,
    /// Lines that contain tracked changes (for change bars).
    pub rev_lines: std::collections::HashSet<usize>,
    /// Commented ranges: (comment id, start, end).
    pub comment_ranges: Vec<(String, Pos, Pos)>,
}

/// Value of a `w:id="..."` attribute in raw XML.
fn xml_id(xml: &str) -> Option<String> {
    let i = xml.find(" w:id=\"")? + 7;
    let e = xml[i..].find('"')?;
    Some(xml[i..i + e].to_string())
}

/// Tracked insertions and lines with revisions.
fn find_revisions(doc: &Document) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for (pi, p) in doc.paras.iter().enumerate() {
        let mut open: Vec<usize> = Vec::new();
        for (ci, f) in p.fmts.iter().enumerate() {
            let Some(o) = pool::obj(f.obj) else { continue };
            if let ObjKind::Wrap { open: is_open } = o.kind {
                if is_open {
                    if o.xml.starts_with("<w:ins") || o.xml.starts_with("<w:moveTo") {
                        open.push(ci + 1);
                    } else {
                        open.push(usize::MAX);
                    }
                } else if let Some(s) = open.pop() {
                    if s != usize::MAX && ci > s {
                        out.push((pi, s, ci));
                    }
                }
            }
        }
    }
    out
}

/// Commented ranges from commentRangeStart/End (or the reference alone).
fn find_comments(doc: &Document) -> Vec<(String, Pos, Pos)> {
    let mut starts: HashMap<String, Pos> = HashMap::new();
    let mut out: Vec<(String, Pos, Pos)> = Vec::new();
    for (pi, p) in doc.paras.iter().enumerate() {
        for (ci, f) in p.fmts.iter().enumerate() {
            let Some(o) = pool::obj(f.obj) else { continue };
            let Some(id) = xml_id(&o.xml) else { continue };
            if o.xml.contains("commentRangeStart") {
                starts.insert(id, Pos::new(pi, ci));
            } else if o.xml.contains("commentRangeEnd") {
                let s = starts.remove(&id).unwrap_or(Pos::new(pi, ci));
                out.push((id, s, Pos::new(pi, ci)));
            } else if o.xml.contains("commentReference") && !out.iter().any(|(i, _, _)| *i == id) {
                out.push((id, Pos::new(pi, ci), Pos::new(pi, ci)));
            }
        }
    }
    out
}

type BTreeMapPages = std::collections::BTreeMap<usize, Vec<((bool, String), Box<Layout>, f32)>>;

/// A laid-out header or footer. Variants: 0 header, 1 first-page header, 2 footer, 3 first-page footer.
#[derive(Clone)]
pub struct HfBox {
    pub section: usize,
    pub variant: usize,
    pub lay: Box<Layout>,
    pub dy: f32,
}

/// The document of a header/footer variant of a section.
pub fn hf_doc(doc: &Document, section: usize, variant: usize) -> Option<&Document> {
    let h = doc.hf.get(section)?;
    match variant {
        0 => h.header.as_deref(),
        1 => h.header_first.as_deref(),
        2 => h.footer.as_deref(),
        _ => h.footer_first.as_deref(),
    }
}

impl Layout {
    /// Geometry of `page`, falling back to the first page.
    pub fn geom(&self, page: usize) -> PageGeom {
        self.page_geoms.get(page).copied().unwrap_or_else(|| {
            PageGeom {
                w: self.page_w,
                h: self.page_h,
                margins: self.margins,
                header_dist: 36.0,
                footer_dist: 36.0,
            }
        })
    }

    /// Top of `page` in the stacked canvas, including the gaps between pages.
    pub fn page_top(&self, page: usize) -> f32 {
        self.page_tops.get(page).copied().unwrap_or_else(|| {
            page as f32 * (self.page_h + 24.0)
        })
    }

    /// Page under a y coordinate measured from the first page's top (gaps included).
    pub fn page_at_y(&self, rel: f32) -> usize {
        let rel = rel.max(0.0);
        (0..self.pages)
            .rev()
            .find(|p| rel >= self.page_top(*p))
            .unwrap_or(0)
    }

    /// The rich header (or footer) to draw on `page`, with its document.
    pub fn hf_for<'a>(&'a self, doc: &'a Document, header: bool, page: usize) -> Option<(&'a HfBox, &'a Document)> {
        let section = *self.page_section.get(page)?;
        let first = self.section_first_page.get(section) == Some(&page) && doc.hf.get(section)?.title_page;
        let variant = match (header, first) {
            (true, false) => 0,
            (true, true) => 1,
            (false, false) => 2,
            (false, true) => 3,
        };
        let d = hf_doc(doc, section, variant)?;
        let b = self.hf.iter().find(|b| b.section == section && b.variant == variant)?;
        Some((b, d))
    }

    /// Section and variant of the header (or footer) shown on `page`, whether or not it has content.
    pub fn hf_slot(&self, doc: &Document, header: bool, page: usize) -> (usize, usize) {
        let section = self.page_section.get(page).copied().unwrap_or(0);
        let first = self.section_first_page.get(section) == Some(&page) && doc.hf.get(section).is_some_and(|h| h.title_page);
        (section, if header { 0 } else { 2 } + first as usize)
    }

    /// Layout of a header/footer placed on `page` of this (body) layout, for editing it in place.
    pub fn story_layout(&self, section: usize, variant: usize, page: usize) -> Option<Layout> {
        let b = self.hf.iter().find(|b| b.section == section && b.variant == variant)?;
        Some(self.placed(&b.lay, page, 0.0, b.dy))
    }

    /// Layout of a footnote/endnote where it is shown, with its page.
    pub fn note_story_layout(&self, key: &(bool, String)) -> Option<(Layout, usize)> {
        let n = self.notes.iter().find(|n| &n.key == key)?;
        Some((self.placed(&n.lay, n.page, 0.0, n.y), n.page))
    }

    /// Layout of the nested table held by body paragraph `para` where it is shown, with its page.
    pub fn nested_story_layout(&self, para: usize) -> Option<(Layout, usize)> {
        let (_, page, x, y, nl) = self.nested.iter().find(|n| n.0 == para)?;
        Some((self.placed(nl, *page, *x, *y), *page))
    }

    /// Body paragraph of the nested table under page-relative (`x`, `y`).
    pub fn nested_at(&self, page: usize, x: f32, y: f32) -> Option<usize> {
        self.nested.iter().find_map(|(para, pg, nx, ny, nl)| {
            let w = nl.cells.iter().map(|c| c.x + c.w).fold(0.0f32, f32::max);
            let h = content_height(nl);
            (*pg == page && x >= *nx && x <= nx + w && y >= *ny && y <= ny + h).then_some(*para)
        })
    }

    /// Vertical extent of a footnote/endnote on its page.
    pub fn note_extent(n: &NoteBox) -> (f32, f32) {
        (n.y, n.y + content_height(&n.lay))
    }

    /// A sub-layout (header, note, nested table) moved onto `page` at offset (`dx`, `dy`);
    /// every other page is empty.
    fn placed(&self, sub: &Layout, page: usize, dx: f32, dy: f32) -> Layout {
        let mut l = sub.clone();
        for line in &mut l.lines {
            line.page = page;
            line.y += dy;
            line.xs.iter_mut().for_each(|x| *x += dx);
        }
        for p in &mut l.paras {
            p.left += dx;
            p.right += dx;
            if let Some((_, mx)) = &mut p.marker {
                *mx += dx;
            }
        }
        for c in &mut l.cells {
            c.page = page;
            c.x += dx;
            c.y += dy;
        }
        for f in &mut l.floats {
            f.page = page;
            f.x += dx;
            f.y += dy;
        }
        for n in &mut l.nested {
            n.1 = page;
            n.2 += dx;
            n.3 += dy;
        }
        l.pages = self.pages;
        l.page_w = self.page_w;
        l.page_h = self.page_h;
        l.page_geoms = self.page_geoms.clone();
        l.page_tops = self.page_tops.clone();
        l.page_lines = vec![0..0; self.pages];
        if let Some(r) = l.page_lines.get_mut(page) {
            *r = 0..l.lines.len();
        }
        l.notes.clear();
        l.note_separators.clear();
        l.hf.clear();
        l
    }
}

/// Finds the results of PAGE and NUMPAGES fields.
fn find_page_fields(doc: &Document) -> Vec<(usize, usize, usize, bool)> {
    let mut out = Vec::new();
    for (pi, p) in doc.paras.iter().enumerate() {
        let mut instr = String::new();
        let mut result_start: Option<(usize, bool)> = None;
        let mut in_instr = false;
        for (ci, f) in p.fmts.iter().enumerate() {
            let Some(o) = pool::obj(f.obj) else { continue };
            match o.kind {
                ObjKind::FieldBegin => {
                    instr.clear();
                    in_instr = true;
                    result_start = None;
                }
                ObjKind::Hidden if in_instr && o.xml.contains("instrText") => {
                    if let Ok(el) = crate::xml::parse(&o.xml) {
                        instr.push_str(&el.run_text());
                    }
                }
                ObjKind::FieldSep => {
                    in_instr = false;
                    let first = instr.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                    if first == "PAGE" || first == "NUMPAGES" || first == "SECTIONPAGES" {
                        result_start = Some((ci + 1, first != "PAGE"));
                    }
                }
                ObjKind::FieldEnd => {
                    if let Some((s, total)) = result_start.take() {
                        if ci > s {
                            out.push((pi, s, ci, total));
                        }
                    }
                    in_instr = false;
                }
                _ => {}
            }
        }
    }
    out
}

/// Lays out a header/footer sub-document across the page width.
fn hf_layout(_main: &Document, sub: &Document, fonts: &mut FontsView<'_>, zoom: f32, setup: &PageSetup, top: f32) -> Box<Layout> {
    let mut d = sub.clone();
    d.page = setup.clone();
    d.page.height = 20000.0;
    d.page.margins = [top, setup.margins[1], 0.0, setup.margins[3]];
    d.header.clear();
    d.footer.clear();
    d.page_numbers = false;
    d.hf = Default::default();
    d.notes = Default::default();
    Box::new(layout(&d, fonts, zoom))
}

/// Height of the content of a header/footer layout below its top margin.
fn content_height(l: &Layout) -> f32 {
    let lines = l.lines.iter().map(|x| x.y + x.height).fold(0.0f32, f32::max);
    let cells = l.cells.iter().map(|c| c.y + c.h).fold(0.0f32, f32::max);
    (lines.max(cells) - l.margins[0]).max(0.0)
}

/// Format used for header and footer text.
pub fn hf_fmt() -> CharFmt {
    CharFmt { size: 10.0, color: [0x59, 0x59, 0x59, 255], ..CharFmt::body() }
}

/// Font used to draw a character with format `f` at `scale` points-per-pt.
pub fn font_for(f: &CharFmt, scale: f32) -> (FontId, fonts::Resolved) {
    let r = fonts::resolve(f.font, f.bold, f.italic);
    let mut size = f.size * scale;
    if f.script != 0 {
        size *= SCRIPT_SCALE;
    }
    (FontId::new(size.max(1.0), r.family.clone()), r)
}

/// Font for a character, taking small caps into account.
pub fn font_for_char(f: &CharFmt, c: char, scale: f32) -> FontId {
    let (mut id, _) = font_for(f, scale);
    if f.small_caps && !f.caps && c.is_lowercase() {
        id.size *= SMALL_CAPS_SCALE;
    }
    id
}

/// Character as displayed (caps / small caps).
pub fn display_char(f: &CharFmt, c: char) -> char {
    if (f.caps || f.small_caps) && c.is_lowercase() {
        c.to_uppercase().next().unwrap_or(c)
    } else {
        c
    }
}

/// Vertical baseline shift for super/subscript (negative is up).
pub fn script_shift(f: &CharFmt, scale: f32) -> f32 {
    match f.script {
        1 => -f.size * scale * 0.33,
        -1 => f.size * scale * 0.15,
        _ => 0.0,
    }
}

type MetricCache = HashMap<(String, u32), (f32, f32)>;

/// (ascent, row height) for a font.
fn metrics(fonts: &mut FontsView<'_>, cache: &mut MetricCache, id: &FontId) -> (f32, f32) {
    let key = (format!("{:?}", id.family), id.size.to_bits());
    if let Some(m) = cache.get(&key) {
        return *m;
    }
    // Measured at 16x and scaled down: egui snaps metrics to whole pixels, which
    // would make 11pt Calibri lines 13pt instead of Word's 13.43pt.
    const K: f32 = 16.0;
    let big = FontId { size: id.size * K, family: id.family.clone() };
    let g = fonts.layout_no_wrap("Hg".into(), big, Color32::PLACEHOLDER);
    let h = g.size().y / K;
    let asc = g
        .rows
        .first()
        .and_then(|r| r.row.glyphs.first().map(|gl| (r.pos.y + gl.pos.y) / K))
        .unwrap_or(h * 0.8);
    cache.insert(key, (asc, h));
    (asc, h)
}

/// Text shown for an opaque object, if it takes space.
pub fn obj_label(lay_text: Option<&String>, kind: &ObjKind) -> Option<String> {
    match kind {
        ObjKind::Label(s) => Some(s.clone()),
        ObjKind::NoteRef { .. } => lay_text.cloned().or(Some("*".into())),
        _ => None,
    }
}

/// Free horizontal intervals on a row of the page: `[left, right]` with every
/// obstacle (x0, x1) overlapping the vertical range (y0, y1) cut out, `gap`
/// clear on each side of an obstacle.
fn free_intervals(obs: &[[f32; 4]], y0: f32, y1: f32, left: f32, right: f32, gap: f32) -> Vec<(f32, f32)> {
    let mut ivs = vec![(left, right)];
    for o in obs {
        if o[2] >= y1 || o[3] <= y0 {
            continue;
        }
        let (a, b) = ((o[0] - gap).max(left), (o[1] + gap).min(right));
        let mut next = Vec::with_capacity(ivs.len() + 1);
        for (s, e) in ivs {
            if b <= s || a >= e {
                next.push((s, e));
                continue;
            }
            if s < a {
                next.push((s, a));
            }
            if e > b {
                next.push((b, e));
            }
        }
        ivs = next;
    }
    ivs.retain(|&(s, e)| e - s > 0.5);
    ivs
}

struct Ctx<'a, 'b> {
    fonts: &'a mut FontsView<'b>,
    cache: MetricCache,
    sc: f32,
    default_tab: f32,
    obj_text: HashMap<(usize, usize), String>,
    footnotes: u32,
    endnotes: u32,
}

struct ParaLines {
    lines: Vec<Line>,
    marker: Option<(String, f32)>,
    left: f32,
    right: f32,
}

impl Ctx<'_, '_> {
    fn extent(&mut self, f: &CharFmt) -> (f32, f32) {
        let (id, _) = font_for(f, self.sc);
        let (a, h) = metrics(self.fonts, &mut self.cache, &id);
        let shift = script_shift(f, self.sc);
        ((a - shift).max(0.0), (h - a + shift).max(0.0))
    }

    fn text_width(&mut self, f: &CharFmt, s: &str) -> f32 {
        let mut w = 0.0;
        for c in s.chars() {
            let id = font_for_char(f, c, self.sc);
            w += self.fonts.glyph_width(&id, display_char(f, c)) + f.char_spacing * self.sc;
        }
        w
    }

    /// Lays out paragraph `pi` between `left_edge` and `right_edge` (page-relative);
    /// line y values are relative to the paragraph top.
    ///
    /// `obs` are side-wrap obstacle rects (x0, x1, y0, y1) in page coordinates;
    /// `page_y` is the paragraph's top on the page, used to place lines beside them.
    fn para(&mut self, doc: &Document, pi: usize, label: Option<&String>, left_edge: f32, right_edge: f32, obs: &[[f32; 4]], page_y: f32) -> ParaLines {
        let sc = self.sc;
        let para = &doc.paras[pi];
        let n = para.len();
        let left = left_edge + para.ind_left * sc;
        let right = (right_edge - para.ind_right * sc).max(left);
        let first_x = left + para.ind_first * sc;

        // Per-character metrics.
        let mut widths = vec![0.0f32; n];
        let mut asc = vec![0.0f32; n];
        let mut desc = vec![0.0f32; n];
        let mut is_img = vec![false; n];
        let mut cur: Option<(CharFmt, f32, f32)> = None;
        for i in 0..n {
            let f = para.fmts[i];
            let c = para.chars[i];
            if f.hidden {
                continue;
            }
            if f.image != 0 {
                if let Some(img) = doc.images.get(&f.image) {
                    if img.anchor.is_some() {
                        continue;
                    }
                    widths[i] = img.w * sc;
                    asc[i] = img.h * sc;
                    is_img[i] = true;
                }
                continue;
            }
            if f.obj != 0 {
                if let Some(o) = pool::obj(f.obj) {
                    match &o.kind {
                        ObjKind::Box { anchor: Some(_), .. } => {}
                        ObjKind::Box { w, h, .. } => {
                            widths[i] = w * sc;
                            asc[i] = h * sc;
                            is_img[i] = true;
                        }
                        ObjKind::NoteRef { endnote, id } => {
                            let self_label = if id == "self" { NOTE_SELF.with(|s| s.borrow().clone()) } else { None };
                            let label = if let Some(l) = self_label {
                                l
                            } else if *endnote {
                                self.endnotes += 1;
                                roman(self.endnotes)
                            } else {
                                self.footnotes += 1;
                                self.footnotes.to_string()
                            };
                            let mut lf = f;
                            lf.script = 1;
                            widths[i] = self.text_width(&lf, &label);
                            let (a, d) = self.extent(&lf);
                            asc[i] = a;
                            desc[i] = d;
                            self.obj_text.insert((pi, i), label);
                        }
                        ObjKind::Deleted(s) if show_markup() => {
                            widths[i] = self.text_width(&f, s);
                            let (a, d) = self.extent(&f);
                            asc[i] = a;
                            desc[i] = d;
                        }
                        ObjKind::Label(s) => {
                            widths[i] = self.text_width(&f, s);
                            let (a, d) = self.extent(&f);
                            asc[i] = a;
                            desc[i] = d;
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if cur.as_ref().map(|c| c.0) != Some(f) {
                let (a, d) = self.extent(&f);
                cur = Some((f, a, d));
            }
            let (_, a, d) = cur.unwrap();
            widths[i] = match c {
                '\t' | PAGE_BREAK | LINE_BREAK | COLUMN_BREAK | '\u{00AD}' => 0.0,
                _ => {
                    let id = font_for_char(&f, c, sc);
                    self.fonts.glyph_width(&id, display_char(&f, c)) + f.char_spacing * sc
                }
            };
            asc[i] = a;
            desc[i] = d;
        }
        let (end_a, end_d) = self.extent(&para.end_fmt);

        // List marker.
        let mut marker = None;
        let mut first_text_x = first_x;
        if let Some(l) = label {
            let mut mf = para.fmt_at(0);
            mf.script = 0;
            mf.underline = false;
            let mw = self.text_width(&mf, l);
            marker = Some((l.clone(), first_x));
            first_text_x = if para.ind_first < 0.0 && first_x + mw + 2.0 <= left {
                left
            } else {
                let stop = self.default_tab * sc;
                let after = first_x + mw + 0.25 * stop / 2.0;
                if para.ind_first < 0.0 { after.max(left) } else { after }
            };
        }

        let segment_width = |from: usize, decimal: bool| -> f32 {
            let mut w = 0.0;
            for k in from..n {
                let c = para.chars[k];
                if c == '\t' || c == PAGE_BREAK || c == LINE_BREAK || c == COLUMN_BREAK {
                    break;
                }
                if decimal && (c == '.' || c == ',') {
                    break;
                }
                w += widths[k];
            }
            w
        };

        let (pa, pd) = self.extent(&para.fmt_at(0));
        let probe0 = (pa + pd).max(1.0);
        let mut lines: Vec<Line> = Vec::new();
        let mut y = 0.0f32;
        let mut i = 0usize;
        loop {
            let line_start = i;
            let mut x0 = if lines.is_empty() { first_text_x } else { left };
            // Free x intervals for this row when side-wrap obstacles are around:
            // cur is the interval the line lives in, next_iv the one past the image.
            let mut cur: (f32, f32) = (x0, right);
            let mut next_iv: Option<(f32, f32)> = None;
            let mut limit = (right - x0).max(0.0);
            // Right edge of the segment where the last placed character sits.
            let mut placed_end = right;
            let mut ivs: Vec<(f32, f32)> = Vec::new();
            if !obs.is_empty() {
                let row_h = lines.last().map(|l| l.height).unwrap_or(probe0).max(1.0);
                let ry0 = page_y + y;
                ivs = free_intervals(obs, ry0, ry0 + row_h, left, right, 4.0 * sc);
                cur = ivs
                    .iter()
                    .copied()
                    .find(|(s, e)| x0 >= *s && x0 < *e)
                    .or_else(|| ivs.iter().copied().find(|(s, _)| *s >= x0))
                    .or_else(|| ivs.last().copied())
                    .unwrap_or((left, right));
                if x0 < cur.0 {
                    x0 = cur.0;
                }
                next_iv = ivs.iter().copied().find(|(s, _)| *s >= cur.1 - 0.01);
                limit = (cur.1 - x0).max(0.0);
                placed_end = cur.1;
            }
            let mut x = 0.0f32;
            let mut last_break: Option<usize> = None;
            let mut ws: Vec<f32> = Vec::new();
            let mut pending = 0.0f32;
            let mut leaders = Vec::new();
            let mut j = i;
            let mut forced: Option<char> = None;
            let end;
            loop {
                if j >= n {
                    end = n;
                    break;
                }
                let c = para.chars[j];
                if c == PAGE_BREAK || c == LINE_BREAK || c == COLUMN_BREAK {
                    ws.push(0.0);
                    end = j + 1;
                    forced = Some(c);
                    break;
                }
                let w = if c == '\t' && !para.fmts[j].hidden {
                    let abs = x0 + x - left_edge;
                    let mut stops: Vec<doc::TabStop> = para.tabs.clone();
                    if para.ind_first < 0.0 {
                        stops.push(doc::TabStop { pos: para.ind_left, kind: TabKind::Left, leader: Leader::None });
                        stops.sort_by(|a, b| a.pos.total_cmp(&b.pos));
                    }
                    let found = stops
                        .iter()
                        .find(|t| t.pos * sc > abs + 0.5 && !matches!(t.kind, TabKind::Clear | TabKind::Bar));
                    match found {
                        Some(t) => {
                            let stop = t.pos * sc;
                            if t.leader != Leader::None {
                                leaders.push((j, t.leader));
                            }
                            let w = match t.kind {
                                TabKind::Right => stop - abs - segment_width(j + 1, false),
                                TabKind::Center => stop - abs - segment_width(j + 1, false) / 2.0,
                                TabKind::Decimal => stop - abs - segment_width(j + 1, true),
                                _ => stop - abs,
                            };
                            w.max(0.0)
                        }
                        None => {
                            let stop = (self.default_tab * sc).max(4.0);
                            let mut next = ((abs / stop).floor() + 1.0) * stop;
                            if next - abs < 1.0 {
                                next += stop;
                            }
                            next - abs
                        }
                    }
                } else {
                    widths[j]
                };
                // Spaces may hang past the margin, unless the line already overflows
                // (Word puts one character per line in a column narrower than that).
                if x + w > limit && j > line_start && (c != ' ' || x > limit) && !para.fmts[j].hidden {
                    // At the edge of an image with free space past it on this row,
                    // move the word across (square wrap) instead of ending the line.
                    if let Some(nxt) = next_iv {
                        let b = last_break.unwrap_or(line_start);
                        let seg_ok = b <= j
                            && nxt.1 - nxt.0 >= 6.0 * sc
                            && !para.chars[b..j]
                                .iter()
                                .any(|&ch| matches!(ch, '\t' | PAGE_BREAK | LINE_BREAK | COLUMN_BREAK));
                        if seg_ok {
                            ws.truncate(b - line_start);
                            leaders.retain(|(k, _)| *k < b);
                            let x_trunc: f32 = ws.iter().sum();
                            let delta = nxt.0 - x0 - x_trunc;
                            if b < j {
                                for k in b..j {
                                    let wk = widths[k] + if k == b { delta } else { 0.0 };
                                    ws.push(wk);
                                    x += wk;
                                }
                            } else {
                                pending = delta;
                            }
                            cur = nxt;
                            next_iv = ivs.iter().copied().find(|(s, _)| *s >= cur.1 - 0.01);
                            limit = (cur.1 - x0).max(0.0);
                            if b < j {
                                placed_end = cur.1;
                            }
                            continue;
                        }
                    }
                    match last_break {
                        Some(b) => {
                            end = b;
                            ws.truncate(b - line_start);
                            leaders.retain(|(k, _)| *k < b);
                        }
                        None => end = j,
                    }
                    break;
                }
                let wp = w + pending;
                pending = 0.0;
                ws.push(wp);
                x += wp;
                placed_end = cur.1;
                j += 1;
                if c == ' ' || c == '-' || c == '\t' || c == '\u{00AD}' || c == '\u{200B}' {
                    last_break = Some(j);
                }
            }

            // Vertical metrics.
            let mut a = 0.0f32;
            let mut d = 0.0f32;
            let mut text_h = 0.0f32;
            let mut any = false;
            for k in line_start..end {
                if widths[k] == 0.0 && asc[k] == 0.0 {
                    continue;
                }
                if matches!(para.chars[k], PAGE_BREAK | LINE_BREAK | COLUMN_BREAK) && end - line_start > 1 {
                    continue;
                }
                any = true;
                a = a.max(asc[k]);
                d = d.max(desc[k]);
                if !is_img[k] {
                    text_h = text_h.max(asc[k] + desc[k]);
                }
            }
            if !any {
                a = end_a;
                d = end_d;
                text_h = a + d;
            }
            let leading = if para.line_rule == LineRule::Auto { text_h * (para.line_spacing - 1.0).max(0.0) } else { 0.0 };
            let (height, baseline) = match para.line_rule {
                LineRule::Auto => (a + d + leading, a),
                LineRule::Exact => {
                    let h = (para.line_spacing * sc).max(1.0);
                    (h, (h - d).max(0.0))
                }
                LineRule::AtLeast => {
                    let h = (para.line_spacing * sc).max(a + d);
                    (h, h - d)
                }
            };

            // Horizontal alignment.
            let len = end - line_start;
            let mut trailing = 0;
            while trailing < len {
                let c = para.chars[end - 1 - trailing];
                if c == ' ' || c == PAGE_BREAK || c == LINE_BREAK || c == COLUMN_BREAK {
                    trailing += 1;
                } else {
                    break;
                }
            }
            // Width available for alignment: up to the right edge of the segment
            // where this line's content ends (the image's edge, or the column).
            let avail = (placed_end - x0).max(0.0);
            let natural: f32 = ws[..len - trailing].iter().sum();
            let is_last = end >= n && forced.is_none();
            let slack = (avail - natural).max(0.0);
            let mut offset = 0.0;
            let mut per_space = 0.0;
            // Text after a tab is positioned by the tab; alignment applies to the whole line otherwise.
            let has_tab = para.chars[line_start..end].contains(&'\t');
            match para.align {
                Align::Left => {}
                Align::Center if !has_tab => offset = slack / 2.0,
                Align::Right if !has_tab => offset = slack,
                Align::Justify => {
                    let spaces = para.chars[line_start..end - trailing].iter().filter(|c| **c == ' ').count();
                    if !is_last && forced.is_none() && spaces > 0 && !has_tab {
                        per_space = slack / spaces as f32;
                    }
                }
                _ => {}
            }
            let mut xs = Vec::with_capacity(len + 1);
            let mut cx = x0 + offset;
            xs.push(cx);
            for k in 0..len {
                cx += ws[k];
                if per_space > 0.0 && k < len - trailing && para.chars[line_start + k] == ' ' {
                    cx += per_space;
                }
                xs.push(cx);
            }
            let bars: Vec<f32> = para
                .tabs
                .iter()
                .filter(|t| t.kind == TabKind::Bar)
                .map(|t| left_edge + t.pos * sc)
                .filter(|x| *x >= left_edge - 0.5 && *x <= right_edge + 0.5)
                .collect();

            lines.push(Line {
                para: pi,
                start: line_start,
                end,
                page: 0,
                y,
                height,
                baseline,
                xs,
                last_in_para: false,
                forced: forced.is_some(),
                leaders,
                bars,
                leading,
            });
            y += height;
            i = end;
            // Nothing follows a page break at the end of a paragraph: Word shows its
            // paragraph mark beside the break, not on the next page.
            if i >= n && forced != Some(LINE_BREAK) {
                break;
            }
        }
        if let Some(l) = lines.last_mut() {
            l.last_in_para = true;
        }
        ParaLines { lines, marker, left, right }
    }
}

fn roman(mut n: u32) -> String {
    let table = [(10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
    let mut s = String::new();
    while n >= 40 {
        s.push_str("xl");
        n -= 40;
    }
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// Whether a section break of this paragraph starts a new page.
/// Whether the section starting after break number `n` begins on a new page.
/// The break type lives in the *following* section's `w:sectPr`.
fn section_breaks_page(sects: &[String], n: usize) -> bool {
    !sects.get(n + 1).is_some_and(|s| s.contains("w:type w:val=\"continuous\""))
}

struct Pager {
    page: usize,
    y: f32,
    content_h: f32,
    mt: f32,
    /// Geometry of the section currently being laid out.
    geom: PageGeom,
    force: bool,
    /// Height reserved for footnotes at the bottom of the current page.
    notes_h: f32,
    /// Number of text columns and the current one.
    cols: usize,
    col: usize,
    /// Where the current set of columns starts on this page.
    col_top: f32,
    /// Lowest point reached on the page (across columns).
    max_y: f32,
    /// Move to the next column (column break).
    force_col: bool,
    /// Vertical ranges (relative to the top margin) kept clear by floating objects on this page.
    bands: Vec<(f32, f32)>,
    /// Side-wrap floating objects on this page: rects (x0, x1, y0, y1) in page
    /// coordinates that lines must flow beside (square/tight wrap).
    obstacles: Vec<[f32; 4]>,
}

impl Pager {
    fn place(&mut self, h: f32) -> (usize, f32) {
        self.place_with(h, 0.0, 0.0)
    }

    fn new_page(&mut self) {
        self.page += 1;
        self.y = 0.0;
        self.notes_h = 0.0;
        self.col = 0;
        self.col_top = 0.0;
        self.max_y = 0.0;
        self.force = false;
        self.force_col = false;
        self.bands.clear();
        self.obstacles.clear();
    }

    /// Places a block of height `h` that brings `notes` of footnote height with it.
    /// `hang` is the part of `h` that may extend into the bottom margin.
    fn place_with(&mut self, h: f32, notes: f32, hang: f32) -> (usize, f32) {
        // Skip past floating objects that text must keep clear of.
        for _ in 0..8 {
            let hit = self.bands.iter().copied().find(|&(t, b)| self.y < b && self.y + h > t && !self.force);
            match hit {
                Some((_, b)) => self.y = b,
                None => break,
            }
        }
        let full = self.y + h - hang + self.notes_h + notes > self.content_h && self.y > self.col_top;
        if self.force {
            self.new_page();
        } else if full || self.force_col {
            self.force_col = false;
            if self.col + 1 < self.cols {
                self.col += 1;
                self.y = self.col_top;
            } else {
                self.new_page();
            }
        }
        let at = (self.page, self.mt + self.y);
        self.y += h;
        self.max_y = self.max_y.max(self.y);
        self.notes_h += notes;
        at
    }

    /// Whether a block of height `h` fits below the current position.
    fn fits(&self, h: f32) -> bool {
        self.y + h + self.notes_h <= self.content_h
    }

    /// Moves to the next column or page (a pagination-rule break).
    fn advance(&mut self) {
        if self.y <= self.col_top {
            return;
        }
        if self.col + 1 < self.cols {
            self.col += 1;
            self.y = self.col_top;
        } else {
            self.force = true;
        }
    }

    /// Starts a new set of columns below everything on the current page.
    fn set_columns(&mut self, cols: usize) {
        if cols != self.cols || self.col > 0 {
            self.y = self.max_y.max(self.y);
            self.col_top = self.y;
            self.col = 0;
        }
        self.cols = cols.max(1);
    }
}

/// Rebalances the lines of a multi-column section on its last page so the
/// columns end at about the same height (Word does this before a continuous
/// section break). Returns the new bottom y.
#[allow(clippy::too_many_arguments)]
fn balance_columns(
    lines: &mut [Line],
    line_cols: &mut [usize],
    boxes: &mut [Option<ParaBox>],
    from: usize,
    ncols: usize,
    col_w: f32,
    gap: f32,
    page: usize,
) -> Option<f32> {
    let idx: Vec<usize> = (from..lines.len()).filter(|i| lines[*i].page == page).collect();
    if ncols < 2 || idx.len() < 2 {
        return None;
    }
    let top = idx.iter().map(|i| lines[*i].y).fold(f32::MAX, f32::min);
    // Vertical advance of each line within its original column (keeps paragraph spacing).
    let adv: Vec<f32> = idx
        .iter()
        .enumerate()
        .map(|(k, &i)| match idx.get(k + 1) {
            Some(&n) if line_cols[n] == line_cols[i] && lines[n].y > lines[i].y => lines[n].y - lines[i].y,
            _ => lines[i].height,
        })
        .collect();
    let total: f32 = adv.iter().sum();
    let target = total / ncols as f32;
    let old_cols: Vec<usize> = idx.iter().map(|i| line_cols[*i]).collect();
    let mut col = 0;
    let mut acc = 0.0;
    let mut bottom = top;
    for (k, &i) in idx.iter().enumerate() {
        if acc > 0.0 && acc + adv[k] / 2.0 > target && col + 1 < ncols {
            col += 1;
            acc = 0.0;
        }
        let dx = (col as f32 - old_cols[k] as f32) * (col_w + gap);
        for x in &mut lines[i].xs {
            *x += dx;
        }
        lines[i].y = top + acc;
        line_cols[i] = col;
        acc += adv[k];
        bottom = bottom.max(lines[i].y + lines[i].height);
    }
    // Paragraph boxes follow their first line.
    for b in boxes.iter_mut().flatten() {
        if let Some(&first) = idx.iter().find(|i| b.lines.start == **i) {
            let k = idx.iter().position(|i| *i == first).unwrap();
            let dx = (line_cols[first] as f32 - old_cols[k] as f32) * (col_w + gap);
            b.left += dx;
            b.right += dx;
            if let Some(m) = &mut b.marker {
                m.1 += dx;
            }
        }
    }
    Some(bottom)
}

/// (column count, gap in points) of a section's `w:cols`.
fn section_columns(sect: Option<&String>) -> (usize, f32) {
    let Some(el) = sect.and_then(|s| crate::xml::parse(s).ok()) else { return (1, 36.0) };
    let Some(cols) = el.child("cols") else { return (1, 36.0) };
    let explicit = cols.elems().filter(|c| c.name == "col").count();
    let num = cols.attr("num").and_then(|v| v.parse::<usize>().ok()).unwrap_or(explicit.max(1));
    let space = cols.attr("space").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 20.0).unwrap_or(36.0);
    (num.clamp(1, 12), space)
}

thread_local! {
    /// Number shown by a note's own `footnoteRef` while laying out its text.
    static NOTE_SELF: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// A floating picture or shape placed on a page.
#[derive(Clone)]
pub struct FloatBox {
    pub page: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Anchor character.
    pub para: usize,
    pub off: usize,
    pub behind: bool,
    /// Text keeps clear of the object (above/below only).
    pub clear: bool,
    /// Text flows beside the object (square/tight wrap).
    pub side: bool,
    pub image: u32,
    pub label: String,
    /// Table id when the float is a floating table (0 = picture/shape). Floating
    /// tables share this list so text wraps around them with the same code as
    /// pictures; their cells are drawn separately.
    pub table: u32,
    /// Extra clearance from the text, in pixels: top, right, bottom, left.
    pub dist: [f32; 4],
    /// Vertical position measured from the page (or its margins), so the object
    /// belongs to a page of its own choosing rather than to its anchor line's
    /// page. Such floats are pinned to one page across layout passes so their
    /// wrap can never leak into a page they are not drawn on.
    pub abs_page: bool,
}

/// Reference areas for positioning floats.
struct Frame {
    pw: f32,
    ph: f32,
    margins: [f32; 4],
    col_left: f32,
    col_w: f32,
    sc: f32,
}

/// Anchored object of a character: (anchor, width pt, height pt, label).
fn anchored(doc: &Document, f: &CharFmt) -> Option<(doc::Anchor, f32, f32, String)> {
    if f.image != 0 {
        let img = doc.images.get(&f.image)?;
        return img.anchor.map(|a| (a, img.w, img.h, String::new()));
    }
    match pool::obj(f.obj)?.kind {
        ObjKind::Box { anchor: Some(a), w, h, label } => Some((a, w, h, label)),
        _ => None,
    }
}

/// Page of the float anchored by `(para, off)`, for vertical positions measured
/// from the page: pinned so it does not hop between passes. Falls back to the
/// line's own page when unpinned.
fn float_page(pinned: &HashMap<(usize, usize), usize>, para: usize, off: usize, line_page: usize) -> usize {
    *pinned.get(&(para, off)).unwrap_or(&line_page)
}

/// Floats anchored on a placed line.
fn floats_on_line(doc: &Document, pi: usize, l: &Line, para_top: f32, fr: &Frame, pinned: &HashMap<(usize, usize), usize>) -> Vec<FloatBox> {
    use doc::{HRel, PosSpec, VRel};
    let para = &doc.paras[pi];
    let sc = fr.sc;
    let [mt, mr, mb, ml] = fr.margins;
    let mut out = Vec::new();
    for k in l.start..l.end {
        let f = para.fmts[k];
        let Some((a, w, h, label)) = anchored(doc, &f) else { continue };
        let (w, h) = (w * sc, h * sc);
        let (fx, fw) = match a.h_rel {
            HRel::Page => (0.0, fr.pw),
            HRel::Margin => (ml, fr.pw - ml - mr),
            HRel::Column => (fr.col_left, fr.col_w),
            HRel::Character => (l.xs[k - l.start], 0.0),
        };
        let x = match a.h_pos {
            PosSpec::Offset(o) => fx + o * sc,
            PosSpec::Start => fx,
            PosSpec::Center => fx + (fw - w) / 2.0,
            PosSpec::End => fx + fw - w,
        };
        let (fy, fh) = match a.v_rel {
            VRel::Page => (0.0, fr.ph),
            VRel::Margin => (mt, fr.ph - mt - mb),
            VRel::Paragraph => (para_top, 0.0),
            VRel::Line => (l.y, l.height),
        };
        let y = match a.v_pos {
            PosSpec::Offset(o) => fy + o * sc,
            PosSpec::Start => fy,
            PosSpec::Center => fy + (fh - h) / 2.0,
            PosSpec::End => fy + fh - h,
        };
        let abs_page = matches!(a.v_rel, VRel::Page | VRel::Margin);
        out.push(FloatBox {
            page: if abs_page { float_page(pinned, pi, k, l.page) } else { l.page },
            x,
            y,
            w,
            h,
            para: pi,
            off: k,
            behind: a.behind,
            clear: a.wrap == doc::Wrap::Clear,
            side: a.wrap == doc::Wrap::Side,
            image: f.image,
            label,
            table: 0,
            dist: [a.dist[0] * sc, a.dist[1] * sc, a.dist[2] * sc, a.dist[3] * sc],
            abs_page,
        });
    }
    out
}

/// A laid-out footnote or endnote.
#[derive(Clone)]
pub struct NoteBox {
    pub page: usize,
    pub y: f32,
    pub key: (bool, String),
    pub lay: Box<Layout>,
}

/// Lays out a note's text across the body width, numbered `label`.
fn note_layout(main: &Document, note: &Document, fonts: &mut FontsView<'_>, zoom: f32, label: &str) -> Box<Layout> {
    NOTE_SELF.with(|s| *s.borrow_mut() = Some(label.to_string()));
    let l = hf_layout(main, note, fonts, zoom, &main.page, 0.0);
    NOTE_SELF.with(|s| *s.borrow_mut() = None);
    l
}

/// Footnote references (offset, label, id) on a line.
fn line_note_refs(doc: &Document, obj_text: &HashMap<(usize, usize), String>, pi: usize, l: &Line, endnote: bool) -> Vec<(String, String)> {
    let para = &doc.paras[pi];
    (l.start..l.end)
        .filter_map(|k| {
            let o = pool::obj(para.fmts[k].obj)?;
            match o.kind {
                ObjKind::NoteRef { endnote: e, id } if e == endnote && id != "self" => {
                    Some((obj_text.get(&(pi, k)).cloned().unwrap_or_default(), id))
                }
                _ => None,
            }
        })
        .collect()
}

/// Side-wrap obstacle rects of freshly estimated floats that sit on `page`.
fn side_rects(floats_local: &[FloatBox], page: usize) -> Vec<[f32; 4]> {
    floats_local
        .iter()
        .filter(|f| f.side && !f.behind && f.page == page)
        .map(|f| [f.x - f.dist[3], f.x + f.w + f.dist[1], f.y - f.dist[0], f.y + f.h + f.dist[2]])
        .collect()
}

/// Pagination rules for paragraph `pi` (keep lines together, keep with next,
/// widow/orphan control): advances the pager past columns/pages that cannot
/// hold the paragraph's line block.
#[allow(clippy::too_many_arguments)]
fn keep_rules(pager: &mut Pager, doc: &Document, ctx: &mut Ctx, pi: usize, lines: &[Line], labels: &[Option<String>], ml: f32, col_w: f32) {
    let heights: Vec<f32> = lines.iter().map(|l| l.height).collect();
    let n_lines = heights.len();
    let total_h: f32 = heights.iter().sum();
    let para = &doc.paras[pi];
    let sc = ctx.sc;
    // Keep rules apply anywhere but at the top of a page (also at the top of a
    // set of columns started further down by a continuous section break).
    if !pager.force && pager.y > 0.0 {
        let mut need = if para.keep_lines || (para.widow && n_lines <= 3) {
            total_h
        } else if para.widow && n_lines >= 2 {
            heights[0] + heights[1]
        } else {
            heights.first().copied().unwrap_or(0.0)
        };
        if para.keep_next {
            // This paragraph plus the start of the next ones in the chain.
            need = total_h;
            let mut j = pi + 1;
            while j < doc.paras.len() && doc.paras[j].cell.is_none() && j < pi + 8 {
                let next = &doc.paras[j];
                // Heights only: side-wrap obstacles are not threaded through here.
                let nl = ctx.para(doc, j, labels[j].as_ref(), ml, ml + col_w, &[], 0.0);
                let nh: Vec<f32> = nl.lines.iter().map(|l| l.height).collect();
                need += next.space_before * sc;
                if next.keep_next {
                    need += nh.iter().sum::<f32>() + next.space_after * sc;
                    j += 1;
                    continue;
                }
                need += if next.widow && nh.len() >= 2 { nh[0] + nh[1] } else { nh.first().copied().unwrap_or(0.0) };
                break;
            }
        }
        // Only break if the block would fit on an empty page; skip columns of this
        // page that are too short for it, like Word.
        if !pager.fits(need) && need <= pager.content_h {
            pager.advance();
            while !pager.force && !pager.fits(need) {
                if pager.col + 1 < pager.cols {
                    pager.col += 1;
                    pager.y = pager.col_top;
                } else {
                    pager.force = true;
                }
            }
        }
    }
}

/// Estimates where paragraph `pi`'s floats will land on the page after it is
/// placed, filling `floats_local` (all of them) and `bands_local` (the clear
/// ranges on the current page).
#[allow(clippy::too_many_arguments)]
fn estimate_para_floats(
    doc: &Document,
    pi: usize,
    pl: &ParaLines,
    pager: &Pager,
    est_page: &mut usize,
    est_top: &mut f32,
    floats_local: &mut Vec<FloatBox>,
    bands_local: &mut Vec<(f32, f32)>,
    pw: f32,
    ph: f32,
    page_margins: [f32; 4],
    ml: f32,
    col_w: f32,
    gap_sc: f32,
    sc: f32,
    pinned: &HashMap<(usize, usize), usize>,
) {
    floats_local.clear();
    bands_local.clear();
    let first_h = pl.lines.first().map(|l| l.height).unwrap_or(0.0);
    let next_page = pager.force || (pager.y + first_h + pager.notes_h > pager.content_h && pager.y > pager.col_top);
    *est_page = if next_page { pager.page + 1 } else { pager.page };
    *est_top = if next_page { pager.mt } else { pager.mt + pager.y };
    let shift = pager.col as f32 * (col_w + gap_sc);
    let fr = Frame { pw, ph, margins: page_margins, col_left: ml + shift, col_w, sc };
    let mut y = *est_top;
    for l in &pl.lines {
        let mut est = Line {
            page: *est_page,
            y,
            xs: l.xs.iter().map(|x| x + shift).collect(),
            bars: l.bars.iter().map(|x| x + shift).collect(),
            leaders: Vec::new(),
            ..*l
        };
        est.para = pi;
        for fb in floats_on_line(doc, pi, &est, *est_top, &fr, pinned) {
            if fb.clear && fb.page == pager.page {
                bands_local.push((fb.y - pager.mt - 2.0 * sc - fb.dist[0], fb.y + fb.h - pager.mt + 4.0 * sc + fb.dist[2]));
            }
            floats_local.push(fb);
        }
        y += l.height;
    }
}

/// Side-wrap obstacle rects grouped by page for the next layout pass, so that
/// every paragraph on a page flows around the floating objects that land there
/// (not only the paragraphs after the float's own anchor paragraph).
fn obstacles_by_page(floats: &[FloatBox]) -> Vec<Vec<[f32; 4]>> {
    let pages = floats.iter().map(|f| f.page).max().map_or(0, |p| p + 1);
    let mut out = vec![Vec::new(); pages];
    for f in floats.iter().filter(|f| f.side && !f.behind) {
        if let Some(v) = out.get_mut(f.page) {
            v.push([f.x - f.dist[3], f.x + f.w + f.dist[1], f.y - f.dist[0], f.y + f.h + f.dist[2]]);
        }
    }
    out
}

/// Lays out the document. Runs up to three passes: the first has no side-wrap
/// obstacles, the following ones feed the previous pass's floating objects back
/// as per-page obstacles so text above and around them reflows too. Only
/// documents with side-wrap floats pay for the extra passes.
pub fn layout(doc: &Document, fonts: &mut FontsView<'_>, zoom: f32) -> Layout {
    let empty: HashMap<(usize, usize), usize> = HashMap::new();
    // Feed the side-wrap obstacles of one pass back into the next so text above
    // and around a floating object reflows, then stop. This is the historical
    // behaviour: it settles for every ordinary document.
    let mut obs: Vec<Vec<[f32; 4]>> = Vec::new();
    let mut lay = layout_pass(doc, fonts, zoom, &obs, &empty);
    for _ in 0..2 {
        let next = obstacles_by_page(&lay.floats);
        if next == obs {
            break;
        }
        if next.iter().all(|v| v.is_empty()) {
            break;
        }
        obs = next;
        lay = layout_pass(doc, fonts, zoom, &obs, &empty);
    }
    // Self-consistent when the obstacles a pass produced are the ones it was laid
    // out with: every float then wraps exactly the text of the page it is drawn
    // on. When they differ, a page-relative float kept migrating because its own
    // wrap moves the paragraph that anchors it (page N could wrap the text of
    // page N+1). Pin every page-relative float to the page its anchor naturally
    // landed on in a pass without feedback obstacles, which takes those floats
    // out of the loop and settles the passes into one consistent layout.
    if obstacles_by_page(&lay.floats) == obs {
        return lay;
    }
    let base = layout_pass(doc, fonts, zoom, &[], &empty);
    let pinned: HashMap<(usize, usize), usize> =
        base.floats.iter().filter(|f| f.abs_page).map(|f| ((f.para, f.off), f.page)).collect();
    let mut obs: Vec<Vec<[f32; 4]>> = Vec::new();
    let mut lay = layout_pass(doc, fonts, zoom, &obs, &pinned);
    for _ in 0..4 {
        let next = obstacles_by_page(&lay.floats);
        if next == obs {
            break;
        }
        obs = next;
        lay = layout_pass(doc, fonts, zoom, &obs, &pinned);
    }
    lay
}

fn layout_pass(doc: &Document, fonts: &mut FontsView<'_>, zoom: f32, obstacles_by_page: &[Vec<[f32; 4]>], pinned: &HashMap<(usize, usize), usize>) -> Layout {
    let sc = zoom * PT_TO_PX;
    let setups = doc.section_setups();
    let mut geoms: Vec<PageGeom> = setups
        .iter()
        .map(|s| PageGeom {
            w: s.width * sc,
            h: s.height * sc,
            margins: s.margins.map(|m| m * sc),
            header_dist: s.header_dist * sc,
            footer_dist: s.footer_dist * sc,
        })
        .collect();
    if geoms.is_empty() {
        geoms.push(PageGeom {
            w: doc.page.width * sc,
            h: doc.page.height * sc,
            margins: doc.page.margins.map(|m| m * sc),
            header_dist: doc.page.header_dist * sc,
            footer_dist: doc.page.footer_dist * sc,
        });
    }

    // Rich headers and footers; the body moves down/up if they don't fit in the margins.
    let any = |v: [usize; 2]| (0..doc.hf.len()).any(|s| v.iter().any(|x| hf_doc(doc, s, *x).is_some()));
    let rich_header = any([0, 1]);
    let rich_footer = any([2, 3]);
    let mut hf: Vec<HfBox> = Vec::new();
    for section in 0..doc.hf.len() {
        let setup = setups.get(section).cloned().unwrap_or_else(|| doc.page.clone());
        let gi = section.min(geoms.len() - 1);
        let Some(g) = geoms.get_mut(gi) else { continue };
        for variant in 0..4 {
            let is_header = variant < 2;
            if (is_header && !rich_header) || (!is_header && !rich_footer) {
                continue;
            }
            let Some(sub) = hf_doc(doc, section, variant) else { continue };
            if is_header {
                let l = hf_layout(doc, sub, fonts, zoom, &setup, setup.header_dist);
                let bottom = setup.header_dist * sc + content_height(&l);
                g.margins[0] = g.margins[0].max(bottom + 4.0 * sc);
                hf.push(HfBox { section, variant, lay: l, dy: 0.0 });
            } else {
                let l = hf_layout(doc, sub, fonts, zoom, &setup, 0.0);
                let h = content_height(&l);
                g.margins[2] = g.margins[2].max(setup.footer_dist * sc + h + 4.0 * sc);
                hf.push(HfBox { section, variant, lay: l, dy: g.h - setup.footer_dist * sc - h });
            }
        }
    }
    let first = geoms[0];
    let mut pw = first.w;
    let mut ph = first.h;
    let [mut mt, mut mr, mut mb, mut ml] = first.margins;

    let labels = doc.list_labels();
    let mut ctx = Ctx {
        fonts,
        cache: MetricCache::new(),
        sc,
        default_tab: doc.page.default_tab,
        obj_text: HashMap::new(),
        footnotes: 0,
        endnotes: 0,
    };
    let mut pager = Pager {
        page: 0,
        y: 0.0,
        content_h: (ph - mt - mb).max(20.0),
        mt,
        geom: first,
        force: false,
        notes_h: 0.0,
        cols: 1,
        col: 0,
        col_top: 0.0,
        max_y: 0.0,
        force_col: false,
        bands: Vec::new(),
        obstacles: Vec::new(),
    };
    let mut floats: Vec<FloatBox> = Vec::new();
    let mut nested: Vec<(usize, usize, f32, f32, Box<Layout>)> = Vec::new();
    let mut page_margins = first.margins;
    // Footnotes per page: (page, key, layout, height).
    let mut page_notes: Vec<(usize, (bool, String), Box<Layout>, f32)> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut boxes: Vec<Option<ParaBox>> = (0..doc.paras.len()).map(|_| None).collect();
    let mut cells: Vec<CellBox> = Vec::new();
    let mut pending_after = 0.0f32;
    // Space after of an empty section-break paragraph at the top of a page, which
    // Word subtracts from the next paragraph's space before.
    let mut top_after = 0.0f32;
    // Index of the first line of each section after the first.
    let mut section_starts: Vec<usize> = Vec::new();
    let sects = doc.section_props();
    // Column of each line, and the first line of the current section.
    let mut line_cols: Vec<usize> = Vec::new();
    let mut section_first_line = 0usize;
    let has_endnotes = doc.notes.keys().any(|(e, _)| *e);
    let mut prev_style: Option<(Option<String>, bool)> = None;

    let mut pi = 0;
    while pi < doc.paras.len() {
        let para = &doc.paras[pi];
        if let Some(c) = para.cell {
            // ---------------------------------------------------- table
            let range = {
                let mut e = pi;
                while e < doc.paras.len() && doc.paras[e].cell.is_some_and(|x| x.table == c.table) {
                    e += 1;
                }
                pi..e
            };
            let table = doc.tables.get(&c.table).cloned().unwrap_or_default();
            // A floating table leaves the flow: body text wraps around it via the
            // same FloatBox/obstacle pipeline as pictures.
            let float_anchor = table.anchor.filter(|a| a.wrap != doc::Wrap::None);
            if float_anchor.is_none() {
                pager.y += pending_after;
                pending_after = 0.0;
                if para.page_break_before && pager.y > 0.0 {
                    pager.force = true;
                }
            }
            let avail = pw - ml - mr;
            let total: f32 = table.grid.iter().sum::<f32>() * sc;
            let mut widths: Vec<f32> = table.grid.iter().map(|g| g * sc).collect();
            let max_w = pw - ml - (mr * 0.25);
            if total > max_w && total > 0.0 {
                for w in &mut widths {
                    *w *= max_w / total;
                }
            }
            let tw: f32 = widths.iter().sum();
            // (page, x, y) of a floating table, or None when it sits in the flow.
            let mut float_pos: Option<(usize, f32, f32)> = None;
            let x0 = if let Some(a) = float_anchor {
                let (fx, fw) = match a.h_rel {
                    doc::HRel::Page => (0.0, pw),
                    _ => (ml, avail),
                };
                let x = match a.h_pos {
                    doc::PosSpec::Offset(o) => fx + o * sc,
                    doc::PosSpec::Start => fx,
                    doc::PosSpec::Center => fx + (fw - tw) / 2.0,
                    doc::PosSpec::End => fx + fw - tw,
                };
                let fy = match a.v_rel {
                    doc::VRel::Page => 0.0,
                    doc::VRel::Margin => pager.mt,
                    _ => pager.mt + pager.y,
                };
                let y = match a.v_pos {
                    doc::PosSpec::Offset(o) => fy + o * sc,
                    _ => fy,
                };
                // A table measured from the page stays on one page across passes,
                // exactly like a page-relative picture.
                let page = if matches!(a.v_rel, doc::VRel::Page | doc::VRel::Margin) {
                    float_page(pinned, range.start, 0, pager.page)
                } else {
                    pager.page
                };
                float_pos = Some((page, x, y));
                x
            } else {
                match table.align {
                    Some(Align::Center) => ml + (avail - tw) / 2.0,
                    Some(Align::Right) => pw - mr - tw,
                    _ => ml + table.indent * sc - table.cell_margins[1] * sc,
                }
            };
            let col_x = |col: u32| x0 + widths.iter().take(col as usize).sum::<f32>();
            let tedges = table.display_edges();
            let ncols = table.grid.len().max(1) as u32;
            let last_row = doc.paras[range.end - 1].cell.unwrap().row;
            // Resolved borders of a cell: top, right, bottom, left.
            let edges_of = |cref: doc::CellRef| -> [Option<doc::Border>; 4] {
                let cp = table.cell(cref.row, cref.col);
                let span = cp.span.max(1);
                // Imported cells carry resolved edges; generated tables use the table's.
                let mut edges = if cp.edges.iter().any(|e| e.is_some()) {
                    cp.edges
                } else {
                    [
                        if cref.row == 0 { tedges.top } else { tedges.inside_h },
                        if cref.col + span >= ncols { tedges.right } else { tedges.inside_v },
                        if cref.row == last_row { tedges.bottom } else { tedges.inside_h },
                        if cref.col == 0 { tedges.left } else { tedges.inside_v },
                    ]
                };
                if cp.vmerge == VMerge::Continue {
                    edges[0] = None;
                }
                if cp.vmerge != VMerge::None && table.cells.get(&(cref.row + 1, cref.col)).is_some_and(|n| n.vmerge == VMerge::Continue) {
                    edges[2] = None;
                }
                edges
            };
            let border_w = |b: &Option<doc::Border>| if doc::Border::visible(b) { b.map(|b| b.width * sc).unwrap_or(0.0) } else { 0.0 };
            // Word makes room for horizontal borders: each row starts below the wider of
            // the border above it, and the last row's bottom border follows the table.
            let mut prev_bottom = 0.0f32;
            // Stacking offset for a floating table (ignored for in-flow tables).
            let mut fstack = 0.0f32;
            let mut k = range.start;
            while k < range.end {
                let row = doc.paras[k].cell.unwrap().row;
                let row_props = table.rows.get(&row).cloned().unwrap_or_default();
                // Lay out each cell of the row.
                let mut row_cells: Vec<(doc::CellRef, Vec<(usize, ParaLines, f32)>, f32)> = Vec::new();
                let mut nested_here: HashMap<usize, Box<Layout>> = HashMap::new();
                while k < range.end && doc.paras[k].cell.unwrap().row == row {
                    let cref = doc.paras[k].cell.unwrap();
                    let cp = table.cell(cref.row, cref.col);
                    let m = cp.margins.unwrap_or(table.cell_margins);
                    let cx = col_x(cref.col);
                    let cw: f32 = widths.iter().skip(cref.col as usize).take(cp.span.max(1) as usize).sum();
                    let inner_l = cx + m[1] * sc;
                    let inner_r = (cx + cw - m[3] * sc).max(inner_l + 4.0);
                    let mut paras_in_cell = Vec::new();
                    let mut cy = m[0] * sc;
                    let mut prev_after = 0.0f32;
                    let mut first = true;
                    while k < range.end && doc.paras[k].cell == Some(cref) {
                        let p = &doc.paras[k];
                        if !first {
                            cy += prev_after.max(p.space_before * sc);
                        }
                        let pl = match &p.nested {
                            Some(sub) => {
                                // A nested table: lay it out at the cell's inner width.
                                let mut d = (**sub).clone();
                                d.page = doc.page.clone();
                                d.page.width = (inner_r - inner_l) / sc;
                                d.page.height = 20000.0;
                                d.page.margins = [0.0; 4];
                                let nl = Box::new(layout(&d, ctx.fonts, zoom));
                                let h = content_height(&nl).max(4.0 * sc);
                                let len = p.chars.len();
                                nested_here.insert(k, nl);
                                ParaLines {
                                    lines: vec![Line {
                                        para: k,
                                        start: 0,
                                        end: len,
                                        page: 0,
                                        y: 0.0,
                                        height: h,
                                        baseline: h,
                                        xs: vec![inner_l; len + 1],
                                        last_in_para: true,
                                        forced: false,
                                        leaders: Vec::new(),
                                        bars: Vec::new(),
                                        leading: 0.0,
                                    }],
                                    marker: None,
                                    left: inner_l,
                                    right: inner_r,
                                }
                            }
                            None => ctx.para(doc, k, labels[k].as_ref(), inner_l, inner_r, &[], 0.0),
                        };
                        let h: f32 = pl.lines.iter().map(|l| l.height).sum();
                        paras_in_cell.push((k, pl, cy));
                        cy += h;
                        prev_after = p.space_after * sc;
                        first = false;
                        k += 1;
                    }
                    cy += prev_after + m[2] * sc;
                    let content_h = if cp.vmerge == VMerge::Continue { 0.0 } else { cy };
                    row_cells.push((cref, paras_in_cell, content_h));
                }
                let mut row_h = row_cells.iter().map(|c| c.2).fold(0.0f32, f32::max).max(4.0 * sc);
                let row_edges: Vec<[Option<doc::Border>; 4]> = row_cells.iter().map(|c| edges_of(c.0)).collect();
                let top_w = row_edges.iter().map(|e| border_w(&e[0])).fold(prev_bottom, f32::max);
                prev_bottom = row_edges.iter().map(|e| border_w(&e[2])).fold(0.0, f32::max);
                let mut border_top = top_w;
                if row_props.height > 0.0 {
                    if row_props.exact {
                        row_h = row_props.height * sc;
                        border_top = 0.0;
                    } else {
                        row_h = row_h.max(row_props.height * sc - top_w);
                    }
                }
                let (page, top) = match float_pos {
                    Some((fp, _, fy)) => {
                        let t = fy + fstack;
                        fstack += row_h + border_top;
                        (fp, t)
                    }
                    None => pager.place(row_h + border_top),
                };
                for ((cref, paras_in_cell, content_h), edges) in row_cells.into_iter().zip(row_edges) {
                    let cp = table.cell(cref.row, cref.col);
                    let span = cp.span.max(1);
                    let cx = col_x(cref.col);
                    let cw: f32 = widths.iter().skip(cref.col as usize).take(span as usize).sum();
                    cells.push(CellBox {
                        page,
                        x: cx,
                        y: top,
                        w: cw,
                        h: row_h + border_top,
                        shade: cp.shade.or(cp.style_shade),
                        edges,
                        table: c.table,
                    });
                    let dy = match cp.valign {
                        doc::VAlign::Top => 0.0,
                        doc::VAlign::Center => ((row_h - content_h) / 2.0).max(0.0),
                        doc::VAlign::Bottom => (row_h - content_h).max(0.0),
                    };
                    let dy = dy + border_top;
                    for (idx, pl, py) in paras_in_cell {
                        let first_line = lines.len();
                        let para_top = top + py + dy;
                        for mut l in pl.lines {
                            l.page = page;
                            l.y += top + py + dy;
                            let fr = Frame { pw, ph, margins: page_margins, col_left: pl.left, col_w: pl.right - pl.left, sc };
                            floats.extend(floats_on_line(doc, idx, &l, para_top, &fr, pinned));
                            if let Some(nl) = nested_here.remove(&idx) {
                                nested.push((idx, page, pl.left, l.y, nl));
                            }
                            lines.push(l);
                        }
                        boxes[idx] = Some(ParaBox { lines: first_line..lines.len(), left: pl.left, right: pl.right, marker: pl.marker });
                    }
                }
            }
            if let Some((fp, fx, fy)) = float_pos {
                // Register the floating table exactly like a picture so text wraps
                // with the same code: "Around" adds a side obstacle, "Top & Bottom"
                // (Clear) keeps text only above and below via a band.
                let th = fstack;
                let wrap = float_anchor.map(|a| a.wrap).unwrap_or(doc::Wrap::Side);
                let dist = float_anchor.map(|a| [a.dist[0] * sc, a.dist[1] * sc, a.dist[2] * sc, a.dist[3] * sc]).unwrap_or([0.0; 4]);
                let (dt, dr, db, dl) = (dist[0], dist[1], dist[2], dist[3]);
                floats.push(FloatBox {
                    page: fp,
                    x: fx,
                    y: fy,
                    w: tw,
                    h: th,
                    para: range.start,
                    off: 0,
                    behind: false,
                    clear: wrap == doc::Wrap::Clear,
                    side: wrap == doc::Wrap::Side,
                    image: 0,
                    label: String::new(),
                    table: c.table,
                    dist,
                    abs_page: float_anchor.is_some_and(|a| matches!(a.v_rel, doc::VRel::Page | doc::VRel::Margin)),
                });
                if fp == pager.page {
                    if wrap == doc::Wrap::Clear {
                        pager.bands.push((fy - pager.mt - 2.0 * sc - dt, fy + th - pager.mt + 4.0 * sc + db));
                    } else {
                        pager.obstacles.push([fx - dl, fx + tw + dr, fy - dt, fy + th + db]);
                    }
                }
            } else {
                pager.y += prev_bottom;
            }
            prev_style = None;
            pi = range.end;
            continue;
        }

        // ------------------------------------------------------ paragraph
        let same_style = prev_style.as_ref().is_some_and(|(s, _)| *s == para.style_id);
        let prev_contextual = prev_style.as_ref().is_some_and(|(_, c)| *c);
        // Word separates paragraphs by the larger of the space after and the space
        // before (not their sum), and keeps the space before at the start of the document.
        let after = if same_style && prev_contextual { 0.0 } else { pending_after };
        let absorbed = std::mem::take(&mut top_after);
        if para.page_break_before && pager.y + after > 0.0 {
            pager.force = true;
        }
        let doc_start = pager.page == 0 && pager.y == 0.0 && lines.is_empty();
        let before = if (pager.y + after > 0.0 || doc_start) && !pager.force && !(same_style && para.contextual) {
            para.space_before * sc
        } else if absorbed > 0.0 && !pager.force && !(same_style && para.contextual) {
            (para.space_before * sc - absorbed).max(0.0)
        } else {
            0.0
        };
        pager.y += after.max(before);
        let sec = section_starts.len().min(geoms.len() - 1);
        let g = geoms[sec];
        let changed = (g.w - pager.geom.w).abs() > 0.01
            || (g.h - pager.geom.h).abs() > 0.01
            || (0..4).any(|i| (g.margins[i] - pager.geom.margins[i]).abs() > 0.01);
        if changed {
            let had_content = pager.y > 0.0 || pager.page > 0 || !lines.is_empty();
            pager.geom = g;
            pager.mt = g.margins[0];
            pager.content_h = (g.h - g.margins[0] - g.margins[2]).max(20.0);
            pw = g.w;
            ph = g.h;
            [mt, mr, mb, ml] = g.margins;
            page_margins = g.margins;
            if had_content {
                pager.force = true;
                pager.y = 0.0;
            }
        }
        let (ncols, gap) = section_columns(sects.get(section_starts.len()));
        if ncols != pager.cols {
            pager.set_columns(ncols);
        }
        let col_w = (((pw - ml - mr) - gap * sc * (ncols as f32 - 1.0)) / ncols as f32).max(0.0);
        let mut pl = ctx.para(doc, pi, labels[pi].as_ref(), ml, ml + col_w, &[], 0.0);
        // An empty paragraph that only ends a continuous section takes no room at the
        // top of a page (Word).
        let zero_top = para.chars.is_empty()
            && para.sect_break
            && !section_breaks_page(&sects, section_starts.len())
            && (pager.force || pager.y == 0.0);
        if zero_top {
            for l in &mut pl.lines {
                l.height = 0.0;
                l.baseline = 0.0;
                l.leading = 0.0;
            }
        }
        // Pagination rules: keep lines together, keep with next, orphan control.
        keep_rules(&mut pager, doc, &mut ctx, pi, &pl.lines, &labels, ml, col_w);
        let first_line = lines.len();
        let mut first_shift = None;
        // Floats are positioned relative to where the paragraph starts, before its
        // own lines are placed, so the paragraph itself can flow around them.
        let mut est_page = pager.page;
        let mut est_top = 0.0;
        let mut floats_local: Vec<FloatBox> = Vec::new();
        let mut bands_local: Vec<(f32, f32)> = Vec::new();
        estimate_para_floats(
            doc, pi, &pl, &pager, &mut est_page, &mut est_top, &mut floats_local, &mut bands_local, pw, ph,
            page_margins, ml, col_w, gap * sc, sc, pinned,
        );
        // Side-wrap obstacles: break the paragraph again so its lines flow beside
        // its own floats, floats left over from earlier ones, and the floats the
        // previous pass found on this page (so text above a freely moved picture
        // reflows too; line-relative float positions are estimates from pass one).
        // The obstacles are chosen by the page the lines land on, so a paragraph
        // that starts on the next page still flows around that page's pictures.
        let mut own_side = side_rects(&floats_local, est_page);
        let page_obs: &[[f32; 4]] = obstacles_by_page.get(est_page).map(|v| v.as_slice()).unwrap_or(&[]);
        let same_page = est_page == pager.page;
        if !own_side.is_empty() || !page_obs.is_empty() || (same_page && !pager.obstacles.is_empty()) {
            // Obstacles from earlier paragraphs only belong to this page; when the
            // paragraph starts on the next page it sits in column 0 (no shift).
            let shift = if same_page { pager.col as f32 * (col_w + gap * sc) } else { 0.0 };
            let earlier: &[[f32; 4]] = if same_page { &pager.obstacles } else { &[] };
            // Clip every obstacle to this page's text area `[ml, mt]..[pw-mr, ph-mb]`.
            // A square-wrapped float that reaches into the bottom (or side) margin
            // would otherwise keep blocking at y beyond the text area, so a
            // paragraph that spills onto the next page would still be reshaped by
            // it — the picture on page N must never wrap the text of page N+1.
            let [ax0, ay0, ax1, ay1] = [ml, mt, pw - mr, ph - mb];
            let obs: Vec<[f32; 4]> = earlier
                .iter()
                .chain(own_side.iter())
                .chain(page_obs.iter())
                .map(|o| [o[0] - shift, o[1] - shift, o[2], o[3]])
                .filter_map(|o| {
                    let (x0, x1) = (o[0].max(ax0), o[1].min(ax1));
                    let (y0, y1) = (o[2].max(ay0), o[3].min(ay1));
                    (x1 - x0 > 0.5 && y1 - y0 > 0.5).then_some([x0, x1, y0, y1])
                })
                .collect();
            pl = ctx.para(doc, pi, labels[pi].as_ref(), ml, ml + col_w, &obs, est_top);
            if zero_top {
                for l in &mut pl.lines {
                    l.height = 0.0;
                    l.baseline = 0.0;
                    l.leading = 0.0;
                }
            }
            keep_rules(&mut pager, doc, &mut ctx, pi, &pl.lines, &labels, ml, col_w);
            estimate_para_floats(
                doc, pi, &pl, &pager, &mut est_page, &mut est_top, &mut floats_local, &mut bands_local, pw, ph,
                page_margins, ml, col_w, gap * sc, sc, pinned,
            );
            own_side = side_rects(&floats_local, est_page);
        }
        floats.extend(floats_local.drain(..));
        for b in bands_local.drain(..) {
            pager.bands.push(b);
        }
        if est_page == pager.page {
            pager.obstacles.extend(own_side.iter().copied());
        }
        let heights: Vec<f32> = pl.lines.iter().map(|l| l.height).collect();
        let n_lines = heights.len();
        for (li_idx, mut l) in pl.lines.into_iter().enumerate() {
            // Widow control: don't leave the last line alone at the top of a page.
            if para.widow && n_lines >= 3 && li_idx == n_lines - 2 && !pager.force {
                if pager.fits(heights[li_idx]) && !pager.fits(heights[li_idx] + heights[li_idx + 1]) {
                    pager.advance();
                }
            }
            // Footnotes referenced on this line must fit on the same page.
            let mut new_notes = Vec::new();
            let mut notes_h = 0.0;
            if !doc.notes.is_empty() {
                for (label, id) in line_note_refs(doc, &ctx.obj_text, pi, &l, false) {
                    if let Some(nd) = doc.notes.get(&(false, id.clone())) {
                        let nl = note_layout(doc, nd, ctx.fonts, zoom, &label);
                        let h = content_height(&nl);
                        notes_h += h;
                        new_notes.push(((false, id), nl, h));
                    }
                }
            }
            let separator = if !new_notes.is_empty() { 14.0 * sc } else { 0.0 };
            let (page, y) = pager.place_with(l.height, notes_h + if pager.notes_h == 0.0 { separator } else { 0.0 }, l.leading);
            for (key, nl, h) in new_notes {
                page_notes.push((page, key, nl, h));
            }
            l.page = page;
            l.y = y;
            // Shift lines into their column.
            let shift = pager.col as f32 * (col_w + gap * sc);
            if shift != 0.0 {
                for x in &mut l.xs {
                    *x += shift;
                }
            }
            first_shift.get_or_insert(shift);
            line_cols.resize(lines.len(), 0);
            line_cols.push(pager.col);
            let last = doc.paras[pi].chars.get(l.end.wrapping_sub(1)).copied();
            let page_break = l.forced && last == Some(PAGE_BREAK);
            let column_break = l.forced && last == Some(COLUMN_BREAK);
            lines.push(l);
            if page_break {
                pager.force = true;
            }
            if column_break {
                pager.force_col = true;
            }
        }
        let shift = first_shift.unwrap_or(0.0);
        let marker = pl.marker.map(|(t, x)| (t, x + shift));
        boxes[pi] = Some(ParaBox { lines: first_line..lines.len(), left: pl.left + shift, right: pl.right + shift, marker });
        pending_after = para.space_after * sc;
        if zero_top {
            top_after = pending_after;
            pending_after = 0.0;
        }
        prev_style = Some((para.style_id.clone(), para.contextual));
        if para.sect_break && !section_breaks_page(&sects, section_starts.len()) && pager.cols > 1 {
            line_cols.resize(lines.len(), 0);
            if let Some(b) = balance_columns(&mut lines, &mut line_cols, &mut boxes, section_first_line, pager.cols, col_w, gap * sc, pager.page) {
                pager.y = b - pager.mt;
                pager.max_y = pager.y;
                pager.col = 0;
                pager.col_top = pager.y;
            }
        }
        if para.sect_break && section_breaks_page(&sects, section_starts.len()) {
            pager.force = true;
            pending_after = 0.0;
        }
        if para.sect_break {
            section_starts.push(lines.len());
            section_first_line = lines.len();
        }
        pi += 1;
    }

    // Endnotes follow the last paragraph (below all columns), after a separator.
    if pager.cols > 1 && has_endnotes {
        line_cols.resize(lines.len(), 0);
        let (ncols, gap) = section_columns(sects.get(section_starts.len()));
        let col_w = (((pw - ml - mr) - gap * sc * (ncols as f32 - 1.0)) / ncols as f32).max(0.0);
        if let Some(b) = balance_columns(&mut lines, &mut line_cols, &mut boxes, section_first_line, ncols, col_w, gap * sc, pager.page) {
            pager.y = b - pager.mt;
            pager.max_y = pager.y;
        }
    }
    pager.set_columns(1);
    let mut notes: Vec<NoteBox> = Vec::new();
    let mut separators: Vec<(usize, f32)> = Vec::new();
    if doc.notes.keys().any(|(e, _)| *e) {
        let mut first = true;
        for li in 0..lines.len() {
            let pi = lines[li].para;
            let refs = line_note_refs(doc, &ctx.obj_text, pi, &lines[li], true);
            for (label, id) in refs {
                let Some(nd) = doc.notes.get(&(true, id.clone())) else { continue };
                if first {
                    pager.y += 12.0 * sc;
                    let (page, y) = pager.place(6.0 * sc);
                    separators.push((page, y));
                    first = false;
                }
                let nl = note_layout(doc, nd, ctx.fonts, zoom, &label);
                let h = content_height(&nl);
                let (page, y) = pager.place(h);
                notes.push(NoteBox { page, y, key: (true, id), lay: nl });
            }
        }
    }
    // Footnotes stack up from the bottom margin of their page.
    let body_bottom = ph - mb;
    let mut by_page: BTreeMapPages = BTreeMapPages::new();
    for (page, key, nl, h) in page_notes {
        by_page.entry(page).or_default().push((key, nl, h));
    }
    for (page, list) in by_page {
        let total: f32 = list.iter().map(|x| x.2).sum();
        let mut y = body_bottom - total;
        separators.push((page, y - 7.0 * sc));
        for (key, nl, h) in list {
            notes.push(NoteBox { page, y, key, lay: nl });
            y += h;
        }
    }

    // Lines with tracked changes get a change bar.
    let ins_ranges = find_revisions(doc);
    let mut rev_lines = std::collections::HashSet::new();
    for (li, l) in lines.iter().enumerate() {
        let p = &doc.paras[l.para];
        let ins = ins_ranges.iter().any(|(pp, s, e)| *pp == l.para && *s < l.end.max(l.start + 1) && *e > l.start);
        let del = (l.start..l.end).any(|k| pool::obj(p.fmts[k].obj).is_some_and(|o| matches!(o.kind, ObjKind::Deleted(_))));
        if ins || del {
            rev_lines.insert(li);
        }
    }
    let pages = pager.page + 1;
    let section_of_line = |li: usize| section_starts.iter().filter(|s| **s <= li).count();
    let mut page_section = vec![0usize; pages];
    let mut cur = 0;
    for (pg, slot) in page_section.iter_mut().enumerate() {
        if let Some(li) = lines.iter().position(|l| l.page == pg) {
            cur = section_of_line(li);
        }
        *slot = cur;
    }
    let mut section_first_page = vec![0usize];
    for s in &section_starts {
        let pg = lines.get(*s).map(|l| l.page).unwrap_or(pages - 1);
        section_first_page.push(pg);
    }
    // Lines inside tables are emitted cell by cell; keep the page index ordered.
    let mut page_lines = vec![0..0; pages];
    for pg in 0..pages {
        let s = lines.iter().position(|l| l.page == pg).unwrap_or(lines.len());
        let e = lines.iter().rposition(|l| l.page == pg).map(|i| i + 1).unwrap_or(s);
        page_lines[pg] = s..e.max(s);
    }
    let page_geoms: Vec<PageGeom> = (0..pages)
        .map(|p| geoms[(*page_section.get(p).unwrap_or(&0)).min(geoms.len() - 1)])
        .collect();
    let mut page_tops = vec![0.0f32; pages];
    for p in 1..pages {
        page_tops[p] = page_tops[p - 1] + page_geoms[p - 1].h + 24.0;
    }
    let paras = boxes
        .into_iter()
        .map(|b| b.unwrap_or(ParaBox { lines: 0..0, left: first.margins[3], right: first.w - first.margins[1], marker: None }))
        .collect();
    let hff = hf_fmt();
    let header = (doc.header.clone(), ctx.text_width(&hff, &doc.header));
    let footers = (0..pages)
        .map(|pg| {
            let mut s = doc.footer.clone();
            if doc.page_numbers {
                if !s.is_empty() {
                    s.push_str("    ");
                }
                s.push_str(&format!("Page {} of {}", pg + 1, pages));
            }
            let w = ctx.text_width(&hff, &s);
            (s, w)
        })
        .collect();

    Layout {
        lines,
        paras,
        page_lines,
        cells,
        pages,
        scale: sc,
        page_w: first.w,
        page_h: first.h,
        margins: first.margins,
        page_geoms,
        page_tops,
        obj_text: ctx.obj_text,
        header,
        footers,
        hf,
        page_section,
        section_first_page,
        rich_header,
        rich_footer,
        page_fields: find_page_fields(doc),
        notes,
        note_separators: separators,
        floats,
        nested,
        rev_lines,
        ins_ranges,
        comment_ranges: if doc.comments.is_empty() { Vec::new() } else { find_comments(doc) },
    }
}

/// A drawable piece of a line: consecutive characters with one format.
/// Whitespace, objects and small-caps case changes get their own segments.
pub struct Seg {
    pub start: usize,
    pub end: usize,
    pub fmt: CharFmt,
    pub blank: bool,
    /// Lowercase letters drawn as reduced capitals.
    pub small: bool,
}

pub fn segments(para: &doc::Para, line: &Line) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    // A stale line (from mid-frame edits) may point past the paragraph: clamp.
    let start = line.start.min(para.chars.len());
    let end = line.end.min(para.chars.len());
    for k in start..end {
        let c = para.chars[k];
        let f = para.fmts[k];
        if f.hidden {
            continue;
        }
        let blank = c.is_whitespace() || c == PAGE_BREAK || c == LINE_BREAK || c == COLUMN_BREAK || c == '\u{00AD}';
        let small = f.small_caps && !f.caps && c.is_lowercase();
        if let Some(last) = out.last_mut() {
            if last.fmt == f && last.blank == blank && !blank && !f.is_object() && last.end == k && last.small == small {
                last.end = k + 1;
                continue;
            }
        }
        out.push(Seg { start: k, end: k + 1, fmt: f, blank, small });
    }
    out
}

/// Text of a segment as displayed.
pub fn seg_text(para: &doc::Para, s: &Seg) -> String {
    para.chars[s.start..s.end].iter().map(|c| display_char(&s.fmt, *c)).collect()
}

impl Layout {
    /// x range of chars `[s, e)` on a line.
    pub fn x_range(line: &Line, s: usize, e: usize) -> (f32, f32) {
        (line.xs[s - line.start], line.xs[e - line.start])
    }

    /// Index of the line that displays the caret for `pos`.
    pub fn line_of(&self, pos: Pos) -> usize {
        let pb = &self.paras[pos.p.min(self.paras.len() - 1)];
        for li in pb.lines.clone() {
            let l = &self.lines[li];
            if pos.o < l.end || l.last_in_para {
                return li;
            }
        }
        pb.lines.end.saturating_sub(1).min(self.lines.len().saturating_sub(1))
    }

    /// (page, x, top y, height) of the caret.
    pub fn caret(&self, pos: Pos) -> (usize, f32, f32, f32) {
        if self.lines.is_empty() {
            return (0, 0.0, 0.0, 10.0);
        }
        let l = &self.lines[self.line_of(pos)];
        let k = pos.o.saturating_sub(l.start).min(l.xs.len() - 1);
        (l.page, l.xs[k], l.y, l.height)
    }

    /// Closest document position on line `li` to page-relative `x`.
    pub fn pos_in_line(&self, li: usize, x: f32) -> Pos {
        let l = &self.lines[li];
        let len = l.end - l.start;
        let mut max_k = len;
        if len > 0 && (l.forced || !l.last_in_para) {
            max_k = len - 1;
        }
        let mut best = 0;
        let mut best_d = f32::MAX;
        for k in 0..=max_k {
            let d = (l.xs[k] - x).abs();
            if d < best_d {
                best_d = d;
                best = k;
            }
        }
        Pos::new(l.para, l.start + best)
    }

    fn line_box(&self, li: usize) -> (f32, f32, f32, f32) {
        let l = &self.lines[li];
        let pb = &self.paras[l.para];
        let left = pb.left.min(l.xs[0]) - 2.0;
        let right = pb.right.max(*l.xs.last().unwrap()) + 2.0;
        (left, right, l.y, l.y + l.height)
    }

    /// Hit-test a page-relative point.
    pub fn hit(&self, page: usize, x: f32, y: f32) -> Pos {
        let page = page.min(self.pages - 1);
        let range = self.page_lines[page].clone();
        let mut best: Option<(f32, usize)> = None;
        for li in range.clone() {
            if self.lines[li].page != page {
                continue;
            }
            let (l, r, t, b) = self.line_box(li);
            let dy = if y < t { t - y } else if y > b { y - b } else { 0.0 };
            let dx = if x < l { l - x } else if x > r { x - r } else { 0.0 };
            let d = dy * 4.0 + dx;
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, li));
            }
        }
        match best {
            Some((_, li)) => self.pos_in_line(li, x),
            None => {
                let li = self.lines.iter().rposition(|l| l.page <= page).unwrap_or(0);
                self.pos_in_line(li, x)
            }
        }
    }

    /// The line visually above (dir < 0) or below (dir > 0) line `li` at `x`.
    pub fn vertical(&self, li: usize, x: f32, dir: i32) -> Option<usize> {
        let cur = &self.lines[li];
        let key = |l: &Line| (l.page as f32) * 1.0e6 + l.y;
        let cur_key = key(cur);
        // Nearest line in that direction that overlaps x; otherwise the nearest at all.
        let mut overlap: Option<(f32, usize)> = None;
        let mut any: Option<(f32, f32, usize)> = None;
        for (i, l) in self.lines.iter().enumerate() {
            if i == li {
                continue;
            }
            let k = key(l);
            let ahead = if dir > 0 { k > cur_key + 0.5 } else { k < cur_key - 0.5 };
            if !ahead {
                continue;
            }
            let (left, right, _, _) = self.line_box(i);
            let dx = if x < left { left - x } else if x > right { x - right } else { 0.0 };
            let dist = (k - cur_key).abs();
            if dx < 1.0 && overlap.is_none_or(|(d, _)| dist < d) {
                overlap = Some((dist, i));
            }
            if any.is_none_or(|(d, ddx, _)| dist < d - 0.5 || ((dist - d).abs() <= 0.5 && dx < ddx)) {
                any = Some((dist, dx, i));
            }
        }
        overlap.map(|o| o.1).or(any.map(|a| a.2))
    }
}
