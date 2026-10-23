//! Editing headers, footers and notes in place ("stories" other than the body).
//!
//! While one is open, its sub-document is swapped into `App::doc` so every
//! editing command works on it unchanged, and the body is kept here. The body
//! is laid out with the edited story synced into it, and the editor works on a
//! layout of just the story placed where it is shown.

use crate::app::{App, Snap};
use crate::doc::{CharFmt, Document, OBJ, Pos};
use crate::layout::{self, Layout};
use crate::pool::{self, ObjKind, RawObj};
use std::sync::Arc;

/// What is being edited.
#[derive(Clone, PartialEq, Debug)]
pub enum Target {
    /// Variants: 0 header, 1 first-page header, 2 footer, 3 first-page footer.
    Hf { section: usize, variant: usize },
    /// Footnote (false) or endnote (true) by id.
    Note((bool, String)),
    /// Nested table held by a body paragraph.
    Nested(usize),
}

/// Area of a page under the pointer.
#[derive(Clone, PartialEq, Debug)]
pub enum Zone {
    Header,
    Footer,
    Note((bool, String)),
    /// Nested table held by a body paragraph.
    Nested(usize),
    Body,
}

pub struct Story {
    pub target: Target,
    /// Page the story is edited on.
    pub page: usize,
    pub body: Document,
    pub body_layout: Option<Layout>,
    /// Body state before the story was opened (the undo point for all its edits).
    before: Snap,
    body_undo: Vec<Snap>,
    body_redo: Vec<Snap>,
    entry_rev: u64,
    created: bool,
}

impl Story {
    /// The page zone this story occupies.
    pub fn zone(&self) -> Zone {
        match &self.target {
            Target::Hf { variant, .. } if *variant < 2 => Zone::Header,
            Target::Hf { .. } => Zone::Footer,
            Target::Note(k) => Zone::Note(k.clone()),
            Target::Nested(p) => Zone::Nested(*p),
        }
    }

    pub fn is_hf(&self) -> bool {
        matches!(self.target, Target::Hf { .. })
    }

    pub fn name(&self) -> String {
        match &self.target {
            Target::Hf { section, variant } => {
                let base = match variant {
                    0 => "Header",
                    1 => "First Page Header",
                    2 => "Footer",
                    _ => "First Page Footer",
                };
                if self.body.hf.len() > 1 { format!("{base} -Section {}-", section + 1) } else { base.to_string() }
            }
            Target::Note((endnote, _)) => (if *endnote { "Endnote" } else { "Footnote" }).to_string(),
            Target::Nested(_) => "Nested Table".to_string(),
        }
    }
}

/// A request from the canvas, applied once the frame's layout is back in place.
pub enum StoryReq {
    /// Open the header (or footer) of `page`, with the caret near page-relative `at`.
    Open { page: usize, header: bool, at: Option<(f32, f32)> },
    /// Open a note, with the caret near `at` = (page, x, y).
    OpenNote { key: (bool, String), at: Option<(usize, f32, f32)> },
    /// Open the nested table of body paragraph `para`, with the caret near `at` = (page, x, y).
    OpenNested { para: usize, at: Option<(usize, f32, f32)> },
    /// Return to the body, with the caret near `at` if given.
    Close { at: Option<(usize, f32, f32)> },
}

impl App {
    /// The body document (also while a story is being edited).
    pub fn main_doc(&self) -> &Document {
        self.story.as_deref().map(|s| &s.body).unwrap_or(&self.doc)
    }

    /// The body layout (also while a story is being edited).
    pub fn main_layout(&self) -> Option<&Layout> {
        match self.story.as_deref() {
            Some(s) => s.body_layout.as_ref(),
            None => self.layout.as_ref(),
        }
    }

    /// Page showing the caret.
    pub fn caret_page(&mut self) -> usize {
        if let Some(st) = self.story.as_deref() {
            return st.page;
        }
        self.ensure_layout();
        self.layout.as_ref().map(|l| l.caret(self.cursor).0).unwrap_or(0)
    }

    pub fn apply_story_req(&mut self, req: StoryReq) {
        match req {
            StoryReq::Open { page, header, at } => self.open_hf(page, header, at),
            StoryReq::OpenNote { key, at } => self.open_note(key, at),
            StoryReq::OpenNested { para, at } => self.open_nested(para, at),
            StoryReq::Close { at } => {
                self.close_story();
                if let Some((page, x, y)) = at {
                    self.ensure_layout();
                    let pos = self.layout.as_ref().map(|l| l.hit(page, x, y)).unwrap_or_default();
                    self.set_cursor(pos, false);
                }
            }
        }
    }

    /// Starts editing the header (or footer) shown on `page`, creating it if needed.
    pub fn open_hf(&mut self, page: usize, header: bool, at: Option<(f32, f32)>) {
        // Same header on another page: just move there.
        if let Some(st) = self.story.as_deref() {
            let slot = st.body_layout.as_ref().map(|l| l.hf_slot(&st.body, header, page));
            if slot.is_some_and(|(section, variant)| st.target == Target::Hf { section, variant }) {
                self.story.as_deref_mut().unwrap().page = page;
                self.layout = None;
                self.place_caret(page, at);
                return;
            }
            self.close_story();
        }
        self.ensure_layout();
        let lay = self.layout.as_ref().unwrap();
        let page = page.min(lay.pages - 1);
        let (section, variant) = lay.hf_slot(&self.doc, header, page);
        let before = self.snap();
        let existing = layout::hf_doc(&self.doc, section, variant).cloned();
        let created = existing.is_none();
        let sub = match existing {
            Some(d) => d,
            None => {
                // New headers become the default header of the last section (see docx::docx_bytes).
                let sections = self.doc.section_props().len().max(1);
                if section + 1 != sections || variant % 2 == 1 {
                    self.set_status("Adding a header or footer to this section is not supported yet");
                    return;
                }
                let hf = Arc::make_mut(&mut self.doc.hf);
                if hf.len() < sections {
                    hf.resize_with(sections, Default::default);
                }
                let sub = crate::docx::hf_from_plain(&self.doc, header);
                if header {
                    self.doc.header.clear();
                } else {
                    self.doc.footer.clear();
                    self.doc.page_numbers = false;
                }
                self.doc.put_hf(section, variant, sub.clone());
                sub
            }
        };
        self.begin_story(Target::Hf { section, variant }, page, sub, before, created);
        self.place_caret(page, at);
    }

    /// Starts editing a footnote or endnote, with the caret near `at` = (page, x, y).
    pub fn open_note(&mut self, key: (bool, String), at: Option<(usize, f32, f32)>) {
        if self.story.as_deref().is_none_or(|s| s.target != Target::Note(key.clone())) {
            self.close_story();
            let Some(sub) = self.doc.notes.get(&key).cloned() else { return };
            self.ensure_layout();
            let page = self.layout.as_ref().and_then(|l| l.notes.iter().find(|n| n.key == key)).map(|n| n.page).unwrap_or(0);
            let before = self.snap();
            self.begin_story(Target::Note(key), page, sub, before, false);
        }
        match at {
            Some((page, x, y)) => self.place_caret(page, Some((x, y))),
            None => {
                let end = self.doc.end();
                self.set_cursor(end, false);
            }
        }
    }

    /// Starts editing the nested table held by body paragraph `para`, with the caret near `at` = (page, x, y).
    pub fn open_nested(&mut self, para: usize, at: Option<(usize, f32, f32)>) {
        if self.story.as_deref().is_none_or(|s| s.target != Target::Nested(para)) {
            self.close_story();
            let Some(sub) = self.doc.paras.get(para).and_then(|p| p.nested.as_deref()).cloned() else { return };
            self.ensure_layout();
            let page = self.layout.as_ref().and_then(|l| l.nested.iter().find(|n| n.0 == para)).map(|n| n.1).unwrap_or(0);
            let before = self.snap();
            self.begin_story(Target::Nested(para), page, sub, before, false);
        }
        match at {
            Some((page, x, y)) => self.place_caret(page, Some((x, y))),
            None => self.set_cursor(Pos::default(), false),
        }
    }

    fn snap(&self) -> Snap {
        Snap { doc: self.doc.clone(), cursor: self.cursor, anchor: self.anchor }
    }

    /// Swaps `sub` in as the edited document.
    fn begin_story(&mut self, target: Target, page: usize, mut sub: Document, before: Snap, created: bool) {
        sub.page = self.doc.page.clone();
        // List definitions are shared with the body (only its numbering part is saved).
        sub.numbering = self.doc.numbering.clone();
        let body = std::mem::replace(&mut self.doc, sub);
        self.story = Some(Box::new(Story {
            target,
            page,
            body,
            body_layout: None,
            before,
            body_undo: std::mem::take(&mut self.undo),
            body_redo: std::mem::take(&mut self.redo),
            entry_rev: 0,
            created,
        }));
        self.rev += 1;
        self.story.as_deref_mut().unwrap().entry_rev = self.rev;
        self.layout = None;
        self.pending_fmt = None;
        self.goal_x = None;
    }

    fn place_caret(&mut self, page: usize, at: Option<(f32, f32)>) {
        self.ensure_layout();
        let pos = match (at, self.layout.as_ref()) {
            (Some((x, y)), Some(lay)) if !lay.lines.is_empty() => lay.hit(page, x, y),
            _ => Pos::default(),
        };
        self.set_cursor(pos, false);
    }

    /// Whether the open story was changed.
    fn story_changed(&self) -> bool {
        self.story.as_deref().is_some_and(|s| s.created || self.rev != s.entry_rev)
    }

    /// Puts the edited story into the body document (called before laying out the body).
    pub fn sync_story(&mut self) {
        let changed = self.story_changed();
        let Some(st) = self.story.as_deref_mut() else { return };
        let mut sub = self.doc.clone();
        sub.edited |= changed;
        st.body.numbering = sub.numbering.clone();
        match &st.target {
            Target::Hf { section, variant } => st.body.put_hf(*section, *variant, sub),
            Target::Note(key) => {
                Arc::make_mut(&mut st.body.notes).insert(key.clone(), sub);
            }
            Target::Nested(p) => {
                if let Some(para) = st.body.paras.get_mut(*p) {
                    para.nested = Some(Arc::new(sub));
                }
            }
        }
    }

    /// Lays out the body with the edited story and returns the story's own layout.
    pub fn story_layouts(&mut self) -> Option<Layout> {
        self.sync_story();
        let zoom = self.zoom;
        let st = self.story.as_deref_mut()?;
        let body = &st.body;
        let bl = self.ctx.fonts_mut(|f| layout::layout(body, f, zoom));
        st.page = st.page.min(bl.pages - 1);
        let l = match &st.target {
            Target::Hf { section, variant } => bl.story_layout(*section, *variant, st.page)?,
            Target::Note(key) => {
                let (l, page) = bl.note_story_layout(key)?;
                st.page = page;
                l
            }
            Target::Nested(p) => {
                let (l, page) = bl.nested_story_layout(*p)?;
                st.page = page;
                l
            }
        };
        st.body_layout = Some(bl);
        Some(l)
    }

    /// Stops editing the story and returns to the body.
    pub fn close_story(&mut self) {
        if self.story.is_none() {
            return;
        }
        let changed = self.story_changed();
        self.sync_story();
        let st = *self.story.take().unwrap();
        let sub = std::mem::replace(&mut self.doc, st.body);
        self.undo = st.body_undo;
        self.redo = st.body_redo;
        if changed {
            self.undo.push(st.before.clone());
            self.redo.clear();
            self.dirty = true;
        }
        drop(sub);
        self.cursor = self.doc.clamp(st.before.cursor);
        self.anchor = self.doc.clamp(st.before.anchor);
        self.pending_fmt = None;
        self.goal_x = None;
        self.rev += 1;
        self.layout = None;
        self.moved();
    }

    /// Inserts a footnote (or endnote) reference at the caret and opens the new note.
    pub fn insert_note(&mut self, endnote: bool) {
        self.close_story();
        let before = self.snap();
        self.delete_selection();
        let ids = self.doc.notes.keys().filter(|(e, _)| *e == endnote).filter_map(|(_, id)| id.parse::<i64>().ok());
        let id = (ids.max().unwrap_or(0).max(0) + 1).to_string();
        let (tag, style) = if endnote { ("endnoteReference", "EndnoteReference") } else { ("footnoteReference", "FootnoteReference") };
        let rpr = if self.doc.styles.styles.contains_key(style) {
            format!("<w:rStyle w:val=\"{style}\"/>")
        } else {
            "<w:vertAlign w:val=\"superscript\"/>".to_string()
        };
        let xml = format!("<w:r><w:rPr>{rpr}</w:rPr><w:{tag} w:id=\"{id}\"/></w:r>");
        let kind = ObjKind::NoteRef { endnote, id: id.clone() };
        let fmt = self.pending_fmt.take().unwrap_or_else(|| self.current_fmt()).without_object();
        let f = CharFmt { obj: pool::add_obj(RawObj { xml, kind, pkg: self.doc.pkg_id() }), ..fmt };
        let c = self.cursor;
        let para = &mut self.doc.paras[c.p];
        para.chars.insert(c.o, OBJ);
        para.fmts.insert(c.o, f);
        self.cursor = Pos::new(c.p, c.o + 1);
        self.anchor = self.cursor;
        let key = (endnote, id);
        let sub = crate::docx::new_note(&self.doc, endnote, &key.1);
        Arc::make_mut(&mut self.doc.notes).insert(key.clone(), sub.clone());
        self.rev += 1;
        self.ensure_layout();
        let page = self.layout.as_ref().and_then(|l| l.notes.iter().find(|n| n.key == key)).map(|n| n.page).unwrap_or(0);
        self.begin_story(Target::Note(key), page, sub, before, true);
        let end = self.doc.end();
        self.set_cursor(end, false);
    }

    /// Inserts a PAGE (or NUMPAGES) field at the caret.
    pub fn insert_page_field(&mut self, total: bool) {
        self.checkpoint(crate::app::EditKind::Other);
        self.delete_selection();
        let fmt = self.pending_fmt.take().unwrap_or_else(|| self.current_fmt()).without_object();
        let pkg = self.doc.pkg_id();
        let obj = |xml: &str, kind: ObjKind| pool::add_obj(RawObj { xml: xml.to_string(), kind, pkg });
        let instr = if total { " NUMPAGES " } else { " PAGE " };
        let parts: Vec<(char, u32)> = vec![
            (OBJ, obj("<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>", ObjKind::FieldBegin)),
            (OBJ, obj(&format!("<w:r><w:instrText xml:space=\"preserve\">{instr}</w:instrText></w:r>"), ObjKind::Hidden)),
            (OBJ, obj("<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>", ObjKind::FieldSep)),
            ('1', 0),
            (OBJ, obj("<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>", ObjKind::FieldEnd)),
        ];
        let c = self.cursor;
        let para = &mut self.doc.paras[c.p];
        for (k, (ch, id)) in parts.iter().enumerate() {
            let f = CharFmt { obj: *id, ..fmt };
            para.chars.insert(c.o + k, *ch);
            para.fmts.insert(c.o + k, f);
        }
        let end = Pos::new(c.p, c.o + parts.len());
        self.set_cursor(end, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    fn app() -> App {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
        out.textures_delta.clear();
        App::with_ctx(ctx)
    }

    fn hf_texts(doc: &Document) -> String {
        doc.hf
            .iter()
            .flat_map(|h| [&h.header, &h.header_first, &h.footer, &h.footer_first])
            .flatten()
            .map(|d| d.paras.iter().map(|p| p.text()).collect::<Vec<_>>().join("\n"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Opens the header (or footer) of page 1, types, closes and round-trips through .docx.
    /// The saved file is kept in `tests/out/hf` for `tests/validate_word.ps1`.
    fn edit_and_reload(app: &mut App, header: bool, text: &str) -> Document {
        app.open_hf(0, header, None);
        assert!(app.story.is_some(), "story did not open");
        let end = app.doc.end();
        app.set_cursor(end, false);
        app.insert_typed(text);
        app.close_story();
        assert!(app.story.is_none());
        assert!(hf_texts(&app.doc).contains(text), "edit not stored in the body document");
        let bytes = crate::docx::docx_bytes(&app.doc).expect("save");
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/out/hf");
        let _ = std::fs::create_dir_all(&out);
        let stem = app.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or("new".into());
        let _ = std::fs::write(out.join(format!("{stem}.{text}.docx")), &bytes);
        crate::docx::load_docx_bytes(&bytes).expect("reload")
    }

    #[test]
    fn new_document_header_and_footer() {
        let mut a = app();
        let back = edit_and_reload(&mut a, true, "NewHeaderText");
        assert!(hf_texts(&back).contains("NewHeaderText"));
        a.doc = back;
        a.rev += 1;
        let back = edit_and_reload(&mut a, false, "NewFooterText");
        let t = hf_texts(&back);
        assert!(t.contains("NewHeaderText") && t.contains("NewFooterText"), "{t}");
    }

    fn note_texts(doc: &Document) -> String {
        doc.notes.values().flat_map(|d| d.paras.iter().map(|p| p.text())).collect::<Vec<_>>().join("\n")
    }

    fn save_reload(app: &App, name: &str) -> Document {
        let bytes = crate::docx::docx_bytes(&app.doc).expect("save");
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/out/hf");
        let _ = std::fs::create_dir_all(&out);
        let _ = std::fs::write(out.join(format!("{name}.docx")), &bytes);
        crate::docx::load_docx_bytes(&bytes).expect("reload")
    }

    fn note_refs(doc: &Document) -> usize {
        doc.paras
            .iter()
            .flat_map(|p| p.fmts.iter())
            .filter(|f| pool::obj(f.obj).is_some_and(|o| matches!(o.kind, ObjKind::NoteRef { ref id, .. } if id != "self")))
            .count()
    }

    #[test]
    fn insert_notes_in_new_document() {
        let mut a = app();
        a.insert_typed("Body text");
        a.insert_note(false);
        assert!(a.story.is_some());
        a.insert_typed("FootOne");
        a.close_story();
        a.insert_typed(" more");
        a.insert_note(true);
        a.insert_typed("EndOne");
        a.close_story();
        let back = save_reload(&a, "new.notes");
        let t = note_texts(&back);
        assert!(t.contains("FootOne") && t.contains("EndOne"), "{t}");
        assert_eq!(note_refs(&back), 2);
        // One undo removes the endnote and its reference.
        a.undo();
        assert_eq!(note_refs(&a.doc), 1);
    }

    #[test]
    fn edit_and_add_notes_in_corpus() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/04_fields.docx");
        if !path.exists() {
            return;
        }
        let mut a = app();
        a.open_path(&path);
        let key = a.doc.notes.keys().find(|k| !k.0).cloned().expect("corpus footnote");
        a.open_note(key.clone(), None);
        a.insert_typed(" EditedNote");
        a.close_story();
        let end = a.doc.end();
        a.set_cursor(end, false);
        a.insert_note(false);
        a.insert_typed("AddedNote");
        a.close_story();
        let back = save_reload(&a, "04_fields.notes");
        let t = note_texts(&back);
        assert!(t.contains("EditedNote") && t.contains("AddedNote"), "{t}");
        assert_eq!(note_refs(&back), note_refs(&crate::docx::load_docx(&path).unwrap()) + 1);
    }

    #[test]
    fn undo_restores_body_after_header_edit() {
        let mut a = app();
        a.insert_typed("body");
        a.open_hf(0, true, None);
        a.insert_typed("H1");
        a.close_story();
        assert!(hf_texts(&a.doc).contains("H1"));
        a.undo();
        assert!(!hf_texts(&a.doc).contains("H1"));
        assert_eq!(a.doc.paras[0].text(), "body");
    }

    #[test]
    fn corpus_header_footer_round_trip() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
        if !dir.exists() {
            return;
        }
        let mut edited = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "docx") {
                continue;
            }
            for header in [true, false] {
                let mut a = app();
                a.open_path(&path);
                assert!(a.dlg.error.is_none(), "{}", path.display());
                let tag = if header { "ZzHeadEdit" } else { "ZzFootEdit" };
                a.ensure_layout();
                let (section, variant) = a.layout.as_ref().unwrap().hf_slot(&a.doc, header, 0);
                let sections = a.doc.section_props().len().max(1);
                let exists = layout::hf_doc(&a.doc, section, variant).is_some();
                if !exists && (section + 1 != sections || variant % 2 == 1) {
                    continue; // creating headers there is not supported
                }
                let back = edit_and_reload(&mut a, header, tag);
                assert!(hf_texts(&back).contains(tag), "{}: {tag} lost after save", path.display());
                // Body text is untouched.
                let orig = crate::docx::load_docx(&path).unwrap();
                assert_eq!(orig.paras.len(), back.paras.len(), "{}", path.display());
                edited += 1;
            }
        }
        assert!(edited >= 16, "only {edited} headers/footers edited");
    }

    fn nested_texts(doc: &Document) -> String {
        doc.paras.iter().filter_map(|p| p.nested.as_deref()).flat_map(|d| d.paras.iter().map(|p| p.text())).collect::<Vec<_>>().join("
")
    }

    #[test]
    fn edit_nested_tables_in_corpus() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
        if !dir.exists() {
            return;
        }
        let mut edited = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "docx") {
                continue;
            }
            let mut a = app();
            a.open_path(&path);
            let Some(para) = a.doc.paras.iter().position(|p| p.nested.is_some()) else { continue };
            // The nested table is found under the pointer.
            a.ensure_layout();
            let lay = a.layout.as_ref().unwrap();
            let &(_, page, x, y, _) = lay.nested.iter().find(|n| n.0 == para).expect("nested table laid out");
            assert_eq!(lay.nested_at(page, x + 2.0, y + 2.0), Some(para), "{}", path.display());
            a.open_nested(para, Some((page, x + 2.0, y + 2.0)));
            assert!(a.story.is_some(), "{}: story did not open", path.display());
            assert!(a.doc.paras[a.cursor.p].cell.is_some(), "caret not in a cell");
            a.insert_typed("ZzNestedEdit");
            a.close_story();
            assert!(nested_texts(&a.doc).contains("ZzNestedEdit"), "edit not stored in the body");
            let orig_paras = a.doc.paras.len();
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            let back = save_reload(&a, &format!("{stem}.nested"));
            assert!(nested_texts(&back).contains("ZzNestedEdit"), "{}: edit lost after save", path.display());
            assert_eq!(back.paras.len(), orig_paras, "{}", path.display());
            // .fwd files keep the edit too.
            let fwd = std::env::temp_dir().join(format!("fw_nested_{stem}.fwd"));
            crate::io::save_native(&a.doc, &fwd).expect("save fwd");
            let back = crate::io::load_native(&fwd).expect("load fwd");
            let _ = std::fs::remove_file(&fwd);
            assert!(nested_texts(&back).contains("ZzNestedEdit"), "{}: edit lost in .fwd", path.display());
            // One undo restores the nested table.
            a.undo();
            assert!(!nested_texts(&a.doc).contains("ZzNestedEdit"));
            edited += 1;
        }
        assert!(edited >= 1, "no corpus document has a nested table");
    }

    #[test]
    fn dangling_comment_markers_dropped_on_save() {
        let mut a = app();
        a.insert_typed("text");
        let pkg = a.doc.pkg_id();
        let para = &mut a.doc.paras[0];
        for xml in ["<w:commentRangeStart w:id=\"7\"/>", "<w:r><w:commentReference w:id=\"7\"/></w:r>"] {
            let id = pool::add_obj(RawObj { xml: xml.into(), kind: ObjKind::Hidden, pkg });
            para.chars.push(OBJ);
            para.fmts.push(CharFmt { obj: id, ..CharFmt::body() });
        }
        let bytes = crate::docx::docx_bytes(&a.doc).expect("save");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut xml).unwrap();
        assert!(xml.contains("text") && !xml.contains("comment"), "{xml}");
    }

    /// Lays out `doc` and returns (page, top) of the first line of each paragraph.
    fn para_tops(a: &mut App) -> Vec<(usize, f32)> {
        a.rev += 1;
        a.layout = None;
        a.ensure_layout();
        let lay = a.layout.as_ref().unwrap();
        (0..a.doc.paras.len())
            .map(|p| lay.lines.iter().find(|l| l.para == p).map(|l| (l.page, l.y)).unwrap())
            .collect()
    }

    #[test]
    fn word_paragraph_spacing_rules() {
        let mut a = app();
        let mut d = Document::new();
        d.paras = ["Alpha", "Bravo", "Charlie"].iter().map(|t| crate::doc::Para::with_text(t, crate::doc::Style::Normal)).collect();
        for (p, (before, after)) in d.paras.iter_mut().zip([(12.0, 12.0), (18.0, 6.0), (6.0, 0.0)]) {
            p.space_before = before;
            p.space_after = after;
        }
        a.doc = d;
        let tops = para_tops(&mut a);
        let lay = a.layout.as_ref().unwrap();
        let (sc, top) = (lay.scale, lay.margins[0]);
        let h = lay.lines[0].height;
        // Space before applies at the start of the document.
        assert!((tops[0].1 - (top + 12.0 * sc)).abs() < 0.01, "{tops:?}");
        // Adjacent paragraphs are separated by the larger of after and before.
        assert!((tops[1].1 - tops[0].1 - h - 18.0 * sc).abs() < 0.01, "{tops:?}");
        assert!((tops[2].1 - tops[1].1 - h - 6.0 * sc).abs() < 0.01, "{tops:?}");
    }

    #[test]
    fn nothing_follows_a_trailing_page_break() {
        let mut a = app();
        let mut d = Document::new();
        let mut first = crate::doc::Para::with_text("Alpha", crate::doc::Style::Normal);
        first.chars.push(crate::doc::PAGE_BREAK);
        first.fmts.push(CharFmt::body());
        let mut second = crate::doc::Para::with_text("Heading", crate::doc::Style::Normal);
        second.space_before = 24.0;
        d.paras = vec![first, second];
        a.doc = d;
        let tops = para_tops(&mut a);
        let lay = a.layout.as_ref().unwrap();
        // The heading starts the next page with no blank line or space before it.
        assert_eq!(tops[1].0, 1);
        assert!((tops[1].1 - lay.margins[0]).abs() < 0.01, "{tops:?}");
        assert_eq!(lay.lines.iter().filter(|l| l.para == 0).count(), 1);
    }

    #[test]
    fn notes_without_reference_are_dropped_on_save() {
        let mut a = app();
        a.insert_typed("Body");
        a.insert_note(false);
        a.insert_typed("KeptNote");
        a.close_story();
        a.insert_note(false);
        a.insert_typed("OrphanNote");
        a.close_story();
        // Delete the orphan's reference.
        let key = a.doc.notes.iter().find(|(_, d)| d.paras.iter().any(|p| p.text().contains("OrphanNote"))).map(|(k, _)| k.clone()).unwrap();
        let o = a.doc.paras[0]
            .fmts
            .iter()
            .position(|f| pool::obj(f.obj).is_some_and(|o| matches!(&o.kind, ObjKind::NoteRef { id, .. } if *id == key.1)))
            .unwrap();
        a.doc.delete(Pos::new(0, o), Pos::new(0, o + 1));
        let bytes = crate::docx::docx_bytes(&a.doc).expect("save");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let name = zip.file_names().flatten().find(|f| f.contains("footnotes")).expect("footnotes part").to_string();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut zip.by_name(&name).unwrap(), &mut xml).unwrap();
        assert!(xml.contains("KeptNote") && !xml.contains("OrphanNote"), "{xml}");
        assert!(xml.contains("separator"));
    }

    fn square_float_doc() -> Document {
        let mut d = Document::new();
        let text = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november \
                    oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        let mut p = crate::doc::Para::with_text(&text.repeat(3).trim_end(), crate::doc::Style::Normal);
        p.chars.push(crate::doc::OBJ);
        p.fmts.push(CharFmt { image: 1, ..CharFmt::body() });
        d.paras = vec![p];
        d.images.insert(
            1,
            crate::doc::ImageData {
                bytes: Arc::new(Vec::new()),
                px_w: 400,
                px_h: 500,
                w: 120.0,
                h: 160.0,
                raw: None,
                anchor: Some(crate::doc::Anchor {
                    h_rel: crate::doc::HRel::Margin,
                    h_pos: crate::doc::PosSpec::Offset(60.0),
                    v_rel: crate::doc::VRel::Paragraph,
                    v_pos: crate::doc::PosSpec::Offset(0.0),
                    wrap: crate::doc::Wrap::Side,
                    behind: false,
                    dist: [0.0; 4],
                }),
            },
        );
        d
    }

    #[test]
    fn text_flows_beside_side_wrap_picture() {
        let mut a = app();
        a.doc = square_float_doc();
        let tops = para_tops(&mut a);
        assert_eq!(tops.len(), 1);
        let lay = a.layout.as_ref().unwrap();
        let fb = lay.floats.iter().find(|f| f.image == 1).expect("float laid out");
        assert!(fb.side && !fb.clear, "square wrap must not become a band");
        let right = lay.page_w - lay.margins[1];
        let para_lines: Vec<&_> = lay.lines.iter().filter(|l| l.para == 0).collect();
        assert!(para_lines.len() > 3, "expected several lines, got {}", para_lines.len());
        let mut beside = 0;
        for l in &para_lines {
            let intersects = l.y < fb.y + fb.h && l.y + l.height > fb.y;
            if intersects {
                // No glyph ever sits under the picture.
                for &x in &l.xs {
                    assert!(
                        !(x > fb.x + 0.5 && x < fb.x + fb.w - 0.5),
                        "glyph at {x} sits inside the float [{}, {}] (line y {})",
                        fb.x,
                        fb.x + fb.w,
                        l.y
                    );
                }
                if l.xs.last().is_some_and(|&x| x > fb.x + fb.w) {
                    beside += 1;
                }
            } else if l.y >= fb.y + fb.h && !l.last_in_para {
                // Full width resumes below the picture.
                assert!(l.xs.last().is_some_and(|&x| x >= right - 40.0), "line below float ends at {:?}", l.xs.last());
            }
        }
        assert!(beside >= 3, "expected lines continuing past the float, got {beside}");
    }

    #[test]
    fn floating_picture_written_as_wp_anchor() {
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
            .unwrap();
        let mut a = app();
        let mut d = square_float_doc();
        d.images.get_mut(&1).unwrap().bytes = Arc::new(png);
        a.doc = d;
        let bytes = crate::docx::docx_bytes(&a.doc).expect("save");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut xml).unwrap();
        assert!(xml.contains("wp:anchor"), "app-made float must be written as wp:anchor");
        assert!(xml.contains("wrapSquare"), "square wrap must survive: {xml}");
        let back = crate::docx::load_docx_bytes(&bytes).expect("reload");
        let img = back.images.values().find(|i| i.anchor.is_some()).expect("anchor kept");
        assert_eq!(img.anchor.unwrap().wrap, crate::doc::Wrap::Side);
    }

    /// Enter after the picture's anchor character: the text moves to a new
    /// paragraph, which must still flow beside the picture.
    #[test]
    fn new_paragraph_after_the_picture_flows_beside_it() {
        let mut a = app();
        a.doc = square_float_doc();
        a.set_cursor(a.doc.end(), false);
        a.enter();
        assert_eq!(a.doc.paras.len(), 2);
        a.insert_typed(&"Gamma delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec ".repeat(8));
        let tops = para_tops(&mut a);
        assert_eq!(tops.len(), 2);
        let lay = a.layout.as_ref().unwrap();
        let fb = lay.floats.iter().find(|f| f.image == 1).expect("float laid out");
        let mut checked = 0;
        for l in lay.lines.iter().filter(|l| l.para == 1) {
            if l.y < fb.y + fb.h && l.y + l.height > fb.y {
                for &x in &l.xs {
                    assert!(
                        !(x > fb.x + 0.5 && x < fb.x + fb.w - 0.5),
                        "new paragraph: glyph at {x} inside the float (line y {})",
                        l.y
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 0, "the new paragraph should reach the float's rows");
    }

    /// Enter above the picture's anchor character: the picture must stay glued
    /// to its character's line instead of drifting down the new paragraph.
    #[test]
    fn picture_follows_its_anchor_when_splitting_above_it() {
        let mut a = app();
        // The user's flow: an inline picture at the end of the text, then Square.
        let mut d = Document::new();
        let text = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec ";
        let mut p = crate::doc::Para::with_text(&text.repeat(5).trim_end(), crate::doc::Style::Normal);
        p.chars.push(crate::doc::OBJ);
        p.fmts.push(CharFmt { image: 1, ..CharFmt::body() });
        d.paras = vec![p];
        d.images.insert(
            1,
            crate::doc::ImageData {
                bytes: Arc::new(Vec::new()),
                px_w: 400,
                px_h: 500,
                w: 120.0,
                h: 160.0,
                raw: None,
                anchor: None,
            },
        );
        a.doc = d;
        a.ensure_layout();
        let line_h = a.layout.as_ref().unwrap().lines[0].height;
        a.set_image_wrap(1, Some(crate::doc::Wrap::Side));
        a.ensure_layout();
        let old_y = a.layout.as_ref().unwrap().floats.iter().find(|f| f.image == 1).expect("float after Square").y;
        // Enter in the middle of the text, before the picture's anchor character.
        let mid = a.doc.paras[0].len() / 2;
        a.set_cursor(Pos::new(0, mid), false);
        a.enter();
        assert_eq!(a.doc.paras.len(), 2);
        a.ensure_layout();
        let new_y = a
            .layout
            .as_ref()
            .unwrap()
            .floats
            .iter()
            .find(|f| f.image == 1)
            .expect("float kept")
            .y;
        assert!(
            (new_y - old_y).abs() <= 1.5 * line_h,
            "picture drifted: y {old_y} -> {new_y} (line height {line_h})"
        );
        // The document is saved with the line-anchored float for Word validation.
        let bytes = crate::docx::docx_bytes(&a.doc).expect("save");
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/out/hf");
        let _ = std::fs::create_dir_all(&out);
        let _ = std::fs::write(out.join("line_anchor.docx"), &bytes);
    }

    /// Two paragraphs with a Square-wrapped picture on the second one; used to
    /// check that moving the picture reflows text laid out *before* its anchor.
    fn page_float_two_paras() -> Document {
        let mut d = Document::new();
        let text = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november \
                    oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        let p1 = crate::doc::Para::with_text(&text.repeat(4).trim_end(), crate::doc::Style::Normal);
        let mut p2 = crate::doc::Para::with_text(&text.repeat(2).trim_end(), crate::doc::Style::Normal);
        p2.chars.push(crate::doc::OBJ);
        p2.fmts.push(CharFmt { image: 1, ..CharFmt::body() });
        d.paras = vec![p1, p2];
        d.images.insert(
            1,
            crate::doc::ImageData {
                bytes: Arc::new(Vec::new()),
                px_w: 400,
                px_h: 500,
                w: 120.0,
                h: 160.0,
                raw: None,
                anchor: Some(crate::doc::Anchor {
                    h_rel: crate::doc::HRel::Margin,
                    h_pos: crate::doc::PosSpec::Offset(0.0),
                    v_rel: crate::doc::VRel::Line,
                    v_pos: crate::doc::PosSpec::Offset(0.0),
                    wrap: crate::doc::Wrap::Side,
                    behind: false,
                    dist: [0.0; 4],
                }),
            },
        );
        d
    }

    #[test]
    fn moving_a_float_reflows_text_above_it() {
        let mut a = app();
        a.doc = page_float_two_paras();
        let (px, py) = (250.0, 100.0);
        a.move_image_to_page(1, px, py);
        let an = a.doc.images.get(&1).unwrap().anchor.unwrap();
        assert_eq!(an.h_rel, crate::doc::HRel::Page);
        assert_eq!(an.h_pos, crate::doc::PosSpec::Offset(px));
        assert_eq!(an.v_rel, crate::doc::VRel::Page);
        assert_eq!(an.v_pos, crate::doc::PosSpec::Offset(py));

        para_tops(&mut a);
        let lay = a.layout.as_ref().unwrap();
        let sc = lay.scale;
        let fb = lay.floats.iter().find(|f| f.image == 1).expect("float laid out");
        assert!((fb.x - px * sc).abs() < 0.5, "x {} vs {}", fb.x, px * sc);
        assert!((fb.y - py * sc).abs() < 0.5, "y {} vs {}", fb.y, py * sc);
        // Paragraph 0 is placed *before* the float's anchor paragraph, but it must
        // still flow around the picture wherever it lands on the page.
        let mut beside = 0;
        for l in lay.lines.iter().filter(|l| l.para == 0) {
            if l.y < fb.y + fb.h && l.y + l.height > fb.y {
                for &x in &l.xs {
                    assert!(
                        !(x > fb.x + 0.5 && x < fb.x + fb.w - 0.5),
                        "glyph at {x} inside the float [{}, {}] (para 0, line y {})",
                        fb.x,
                        fb.x + fb.w,
                        l.y
                    );
                }
                beside += 1;
            }
        }
        assert!(beside > 0, "no paragraph-0 line reaches the picture's rows");
    }

    #[test]
    fn moving_a_float_clamps_to_the_page() {
        let mut a = app();
        a.doc = page_float_two_paras();
        a.move_image_to_page(1, 10_000.0, -50.0);
        let an = a.doc.images.get(&1).unwrap().anchor.unwrap();
        let pw = a.doc.page.width;
        assert_eq!(an.h_pos, crate::doc::PosSpec::Offset(pw - 120.0));
        assert_eq!(an.v_pos, crate::doc::PosSpec::Offset(0.0));
        // An inline picture is not movable.
        a.doc.images.get_mut(&1).unwrap().anchor = None;
        a.move_image_to_page(1, 5.0, 5.0);
        assert!(a.doc.images.get(&1).unwrap().anchor.is_none());
    }

    #[test]
    fn nudging_moves_a_floating_picture_by_points() {
        let mut a = app();
        a.doc = page_float_two_paras();
        a.move_image_to_page(1, 200.0, 150.0);
        para_tops(&mut a);
        let (bx, by, sc) = {
            let lay = a.layout.as_ref().unwrap();
            let f = lay.floats.iter().find(|f| f.image == 1).unwrap();
            (f.x, f.y, lay.scale)
        };
        a.nudge_image(1, 5.0, -3.0);
        para_tops(&mut a);
        let fb = a.layout.as_ref().unwrap().floats.iter().find(|f| f.image == 1).unwrap();
        assert!((fb.x - (bx + 5.0 * sc)).abs() < 0.5, "x {} vs {}", fb.x, bx + 5.0 * sc);
        assert!((fb.y - (by - 3.0 * sc)).abs() < 0.5, "y {} vs {}", fb.y, by - 3.0 * sc);
    }

    #[test]
    fn quiet_selection_does_not_scroll_the_view() {
        let mut a = app();
        a.doc = page_float_two_paras();
        a.scroll_to_caret = false;
        a.select_quiet(Pos::new(1, 0), Pos::new(1, 1));
        assert!(!a.scroll_to_caret, "select_quiet must not scroll to the caret");
        a.select(Pos::new(1, 0), Pos::new(1, 1));
        assert!(a.scroll_to_caret, "select must still scroll to the caret");
    }

    #[test]
    fn float_at_the_page_bottom_still_reflows_text() {
        let mut a = app();
        // One long paragraph that nearly fills a page, with the picture anchored
        // on its last line; the picture is then dragged up over the lower lines.
        let text = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november \
                    oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        let mut d = Document::new();
        let mut p = crate::doc::Para::with_text(&text.repeat(15).trim_end(), crate::doc::Style::Normal);
        p.chars.push(crate::doc::OBJ);
        p.fmts.push(CharFmt { image: 1, ..CharFmt::body() });
        d.paras = vec![p];
        d.images.insert(
            1,
            crate::doc::ImageData {
                bytes: Arc::new(Vec::new()),
                px_w: 400,
                px_h: 500,
                w: 120.0,
                h: 160.0,
                raw: None,
                anchor: Some(crate::doc::Anchor {
                    h_rel: crate::doc::HRel::Margin,
                    h_pos: crate::doc::PosSpec::Offset(0.0),
                    v_rel: crate::doc::VRel::Line,
                    v_pos: crate::doc::PosSpec::Offset(0.0),
                    wrap: crate::doc::Wrap::Side,
                    behind: false,
                    dist: [0.0; 4],
                }),
            },
        );
        a.doc = d;
        // Move the picture over the lower lines of the page.
        a.move_image_to_page(1, 250.0, 340.0);
        para_tops(&mut a);
        let lay = a.layout.as_ref().unwrap();
        let fb = lay.floats.iter().find(|f| f.image == 1).expect("float laid out");
        assert_eq!(fb.page, 0, "the long paragraph should fit on one page");
        let mut beside = 0;
        for l in lay.lines.iter() {
            if l.y < fb.y + fb.h && l.y + l.height > fb.y {
                beside += 1;
                for &x in &l.xs {
                    assert!(
                        !(x > fb.x + 0.5 && x < fb.x + fb.w - 0.5),
                        "glyph at {x} inside the float [{}, {}] (line y {})",
                        fb.x,
                        fb.x + fb.w,
                        l.y
                    );
                }
            }
        }
        assert!(beside > 0, "no line reaches the picture's rows");
    }

    #[test]
    fn converts_a_csv_selection_into_a_table() {
        let mut a = app();
        let mut d = Document::new();
        d.paras = vec![
            crate::doc::Para::with_text("\"Pregunta\",\"Para qu� sirve\"", crate::doc::Style::Normal),
            crate::doc::Para::with_text("\"�Cu�l es la idea central?\",\"Da foco a todo lo dem�s\"", crate::doc::Style::Normal),
            crate::doc::Para::with_text("\"�Qu� conflicto sostiene la trama?\",\"Motor narrativo\"", crate::doc::Style::Normal),
        ];
        a.doc = d;
        a.select(Pos::default(), Pos::new(2, a.doc.paras[2].len()));
        assert!(a.csv_rows_selection().is_some(), "selection should look like CSV");
        a.insert_table_from_csv();
        assert_eq!(a.doc.tables.len(), 1, "one table inserted");
        let t = a.doc.tables.values().next().unwrap();
        assert_eq!(t.grid.len(), 2, "two columns");
        let cells: Vec<&crate::doc::Para> = a.doc.paras.iter().filter(|p| p.cell.is_some()).collect();
        assert_eq!(cells.len(), 6, "3 rows x 2 columns");
        let texts: Vec<String> = cells.iter().map(|p| p.text()).collect();
        assert!(texts.contains(&"Pregunta".to_string()));
        assert!(texts.contains(&"Da foco a todo lo dem�s".to_string()));
        assert!(texts.contains(&"Motor narrativo".to_string()));
        // The table round-trips through .docx.
        let back = save_reload(&a, "csv_table");
        assert_eq!(back.tables.len(), 1);
        assert!(back.paras.iter().any(|p| p.cell.is_some() && p.text() == "Motor narrativo"));
    }

    /// The layout right after a CSV conversion must be self-consistent: every line
    /// points at a real paragraph and within its characters (a stale layout here is
    /// what used to crash the app while painting).
    #[test]
    fn layout_after_csv_table_is_consistent() {
        let mut a = app();
        let mut d = Document::new();
        d.paras = vec![
            crate::doc::Para::with_text("Name,Role,City", crate::doc::Style::Normal),
            crate::doc::Para::with_text("Ada,Engineer,London", crate::doc::Style::Normal),
            crate::doc::Para::with_text("Linus,Kernel,Helsinki", crate::doc::Style::Normal),
        ];
        a.doc = d;
        a.select(Pos::default(), Pos::new(2, a.doc.paras[2].len()));
        a.insert_table_from_csv();
        a.rev += 1;
        a.layout = None;
        a.ensure_layout();
        let lay = a.layout.as_ref().unwrap();
        for l in &lay.lines {
            assert!(l.para < a.doc.paras.len(), "line references a missing paragraph");
            assert!(l.start <= l.end, "line has an inverted range");
            assert!(l.end <= a.doc.paras[l.para].len(), "line end past its paragraph");
            assert!(l.xs.len() >= l.end - l.start + 1, "line lacks x positions for its characters");
        }
        // The new table is actually rendered on the page.
        assert!(!lay.cells.is_empty(), "the table should have laid-out cells");
    }

    /// A floating table leaves the flow, is placed at its page offset and the body
    /// text wraps around it (same pipeline as floating pictures).
    #[test]
    fn floating_table_reflows_body_text() {
        let mut a = app();
        let text = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november \
                    oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        a.doc.paras = vec![crate::doc::Para::with_text(&text.repeat(20).trim_end(), crate::doc::Style::Normal)];
        a.set_cursor(Pos::default(), false);
        a.insert_table(2, 2);
        let tid = a.doc.tables.keys().next_back().copied().unwrap();
        // Narrow the columns so body text can flow on both sides of the table.
        a.doc.tables.get_mut(&tid).unwrap().grid = vec![120.0, 120.0];
        a.set_table_wrap(tid, Some(crate::doc::Wrap::Side));
        a.move_table_to_page(tid, 250.0, 120.0);
        a.rev += 1;
        a.layout = None;
        a.ensure_layout();
        let lay = a.layout.as_ref().unwrap();
        let sc = lay.scale;
        let cells: Vec<&crate::layout::CellBox> = lay.cells.iter().filter(|c| c.table == tid).collect();
        assert!(!cells.is_empty(), "floating table cells should be laid out");
        let x0 = cells.iter().map(|c| c.x).fold(f32::MAX, f32::min);
        let y0 = cells.iter().map(|c| c.y).fold(f32::MAX, f32::min);
        let x1 = cells.iter().map(|c| c.x + c.w).fold(f32::MIN, f32::max);
        let y1 = cells.iter().map(|c| c.y + c.h).fold(f32::MIN, f32::max);
        assert!((x0 - 250.0 * sc).abs() < 1.0, "x {} vs {}", x0, 250.0 * sc);
        assert!((y0 - 120.0 * sc).abs() < 1.0, "y {} vs {}", y0, 120.0 * sc);
        // No body (non-table) text sits inside the table's rectangle.
        let mut beside = 0;
        for l in lay.lines.iter() {
            if a.doc.paras[l.para].cell.is_some() {
                continue;
            }
            if l.y < y1 && l.y + l.height > y0 {
                beside += 1;
                for &x in &l.xs {
                    assert!(
                        !(x > x0 + 0.5 && x < x1 - 0.5),
                        "body glyph at {x} inside the table [{}, {}] (line y {})",
                        x0,
                        x1,
                        l.y
                    );
                }
            }
        }
        assert!(beside > 0, "no body line reaches the table's rows");
    }

    #[test]
    fn floating_table_round_trips_tblp_pr() {
        let mut a = app();
        a.doc.paras = vec![crate::doc::Para::with_text("Cells", crate::doc::Style::Normal)];
        a.set_cursor(Pos::default(), false);
        a.insert_table(1, 2);
        let tid = a.doc.tables.keys().next_back().copied().unwrap();
        a.doc.tables.get_mut(&tid).unwrap().grid = vec![100.0, 100.0];
        a.set_table_wrap(tid, Some(crate::doc::Wrap::Side));
        a.move_table_to_page(tid, 200.0, 150.0);
        let back = save_reload(&a, "floating_table");
        let t = back.tables.values().find(|t| t.anchor.is_some()).expect("anchor kept");
        let an = t.anchor.unwrap();
        assert_eq!(an.wrap, crate::doc::Wrap::Side);
        assert!((match an.h_pos { crate::doc::PosSpec::Offset(o) => o, _ => 0.0 } - 200.0).abs() < 1.0);
        assert!((match an.v_pos { crate::doc::PosSpec::Offset(o) => o, _ => 0.0 } - 150.0).abs() < 1.0);
    }

    #[test]
    fn resizing_a_table_scales_columns_proportionally() {
        let mut a = app();
        a.doc.paras = vec![crate::doc::Para::with_text("x", crate::doc::Style::Normal)];
        a.set_cursor(Pos::default(), false);
        a.insert_table(1, 2);
        let tid = a.doc.tables.keys().next_back().copied().unwrap();
        let base = vec![100.0f32, 200.0];
        a.doc.tables.get_mut(&tid).unwrap().grid = base.clone();
        // Half the total width -> factor 0.5, keeping the 1:2 ratio.
        a.resize_table_width(tid, &base, 150.0);
        let g = a.doc.tables[&tid].grid.clone();
        assert!((g[0] - 50.0).abs() < 0.01 && (g[1] - 100.0).abs() < 0.01, "{g:?}");
        // Clamped to a minimum total (18pt per column) but the ratio holds.
        a.resize_table_width(tid, &base, 1.0);
        let g = a.doc.tables[&tid].grid.clone();
        let sum: f32 = g.iter().sum();
        assert!((sum - 36.0).abs() < 0.01, "sum {sum}");
        assert!((g[0] / g[1] - 0.5).abs() < 0.01, "ratio {g:?}");
    }

    /// "Top & Bottom" (Clear) wrap: body text never sits beside the table; it stays
    /// above and continues below (band), like a picture with the same wrap mode.
    #[test]
    fn clear_wrap_table_keeps_text_above_and_below() {
        let mut a = app();
        let text = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november \
                    oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        a.doc.paras = vec![crate::doc::Para::with_text(&text.repeat(20).trim_end(), crate::doc::Style::Normal)];
        a.set_cursor(Pos::default(), false);
        a.insert_table(2, 2);
        let tid = a.doc.tables.keys().next_back().copied().unwrap();
        a.doc.tables.get_mut(&tid).unwrap().grid = vec![120.0, 120.0];
        a.set_table_wrap(tid, Some(crate::doc::Wrap::Clear));
        a.move_table_to_page(tid, 220.0, 140.0);
        a.rev += 1;
        a.layout = None;
        a.ensure_layout();
        let lay = a.layout.as_ref().unwrap();
        let cells: Vec<&crate::layout::CellBox> = lay.cells.iter().filter(|c| c.table == tid).collect();
        assert!(!cells.is_empty(), "floating table cells should be laid out");
        let x0 = cells.iter().map(|c| c.x).fold(f32::MAX, f32::min);
        let y0 = cells.iter().map(|c| c.y).fold(f32::MAX, f32::min);
        let x1 = cells.iter().map(|c| c.x + c.w).fold(f32::MIN, f32::max);
        let y1 = cells.iter().map(|c| c.y + c.h).fold(f32::MIN, f32::max);
        let mut below = 0;
        for l in lay.lines.iter() {
            if a.doc.paras[l.para].cell.is_some() {
                continue;
            }
            if l.y + l.height > y0 && l.y < y1 {
                // No body line may sit beside/inside the table.
                for &x in &l.xs {
                    assert!(
                        !(x > x0 + 0.5 && x < x1 - 0.5),
                        "body glyph at {x} beside the Top&Bottom table [{}, {}] (line y {})",
                        x0,
                        x1,
                        l.y
                    );
                }
            }
            if l.y >= y1 - 1.0 {
                below += 1;
            }
        }
        assert!(below > 0, "body text should continue below the table");
    }

    /// Wrap margins push the text further from the object (image and table share it).
    #[test]
    fn table_wrap_margins_push_text_away() {
        let mut a = app();
        let text = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november \
                    oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        a.doc.paras = vec![crate::doc::Para::with_text(&text.repeat(20).trim_end(), crate::doc::Style::Normal)];
        a.set_cursor(Pos::default(), false);
        a.insert_table(2, 2);
        let tid = a.doc.tables.keys().next_back().copied().unwrap();
        a.doc.tables.get_mut(&tid).unwrap().grid = vec![120.0, 120.0];
        a.set_table_wrap(tid, Some(crate::doc::Wrap::Side));
        a.move_table_to_page(tid, 250.0, 120.0);
        a.set_table_margins(tid, 0.0, 30.0, 0.0, 30.0);
        a.rev += 1;
        a.layout = None;
        a.ensure_layout();
        let lay = a.layout.as_ref().unwrap();
        let sc = lay.scale;
        let cells: Vec<&crate::layout::CellBox> = lay.cells.iter().filter(|c| c.table == tid).collect();
        let (x0, y0) = (
            cells.iter().map(|c| c.x).fold(f32::MAX, f32::min),
            cells.iter().map(|c| c.y).fold(f32::MAX, f32::min),
        );
        let (x1, y1) = (
            cells.iter().map(|c| c.x + c.w).fold(f32::MIN, f32::max),
            cells.iter().map(|c| c.y + c.h).fold(f32::MIN, f32::max),
        );
        let m = 30.0 * sc;
        for l in lay.lines.iter() {
            if a.doc.paras[l.para].cell.is_some() {
                continue;
            }
            if l.y + l.height > y0 && l.y < y1 {
                for &x in &l.xs {
                    assert!(
                        !(x > x0 - m + 1.0 && x < x1 + m - 1.0),
                        "glyph at {x} within the {m}-pt margin of the table [{}, {}]",
                        x0 - m,
                        x1 + m
                    );
                }
            }
        }
    }

    /// Moving an element to where it already is must not shift it (guards against
    /// the screen-vs-page reference bug that made elements jump when grabbed).
    #[test]
    fn moving_to_current_position_keeps_the_element() {
        let mut a = app();
        // Picture.
        a.doc = square_float_doc();
        a.move_image_to_page(1, 120.0, 90.0);
        para_tops(&mut a);
        let (bx, by, sc) = {
            let lay = a.layout.as_ref().unwrap();
            let f = lay.floats.iter().find(|f| f.image == 1).unwrap();
            (f.x, f.y, lay.scale)
        };
        a.move_image_to_page(1, bx / sc, by / sc);
        para_tops(&mut a);
        let f = a.layout.as_ref().unwrap().floats.iter().find(|f| f.image == 1).unwrap();
        assert!((f.x - bx).abs() < 0.5 && (f.y - by).abs() < 0.5, "picture moved to {} {}", f.x, f.y);
        // Table.
        a.doc.paras = vec![crate::doc::Para::with_text("x", crate::doc::Style::Normal)];
        a.set_cursor(Pos::default(), false);
        a.insert_table(1, 2);
        let tid = a.doc.tables.keys().next_back().copied().unwrap();
        a.doc.tables.get_mut(&tid).unwrap().grid = vec![100.0, 100.0];
        a.set_table_wrap(tid, Some(crate::doc::Wrap::Side));
        a.move_table_to_page(tid, 200.0, 150.0);
        a.rev += 1;
        a.layout = None;
        a.ensure_layout();
        let (tx, ty, sc) = {
            let lay = a.layout.as_ref().unwrap();
            let cells: Vec<&crate::layout::CellBox> = lay.cells.iter().filter(|c| c.table == tid).collect();
            let x = cells.iter().map(|c| c.x).fold(f32::MAX, f32::min);
            let y = cells.iter().map(|c| c.y).fold(f32::MAX, f32::min);
            (x, y, lay.scale)
        };
        a.move_table_to_page(tid, tx / sc, ty / sc);
        a.rev += 1;
        a.layout = None;
        a.ensure_layout();
        let lay = a.layout.as_ref().unwrap();
        let cells: Vec<&crate::layout::CellBox> = lay.cells.iter().filter(|c| c.table == tid).collect();
        let x = cells.iter().map(|c| c.x).fold(f32::MAX, f32::min);
        let y = cells.iter().map(|c| c.y).fold(f32::MAX, f32::min);
        assert!((x - tx).abs() < 0.5 && (y - ty).abs() < 0.5, "table moved to {x} {y}");
    }

    #[test]
    fn table_margins_round_trip() {
        let mut a = app();
        a.doc.paras = vec![crate::doc::Para::with_text("Cells", crate::doc::Style::Normal)];
        a.set_cursor(Pos::default(), false);
        a.insert_table(1, 2);
        let tid = a.doc.tables.keys().next_back().copied().unwrap();
        a.doc.tables.get_mut(&tid).unwrap().grid = vec![100.0, 100.0];
        a.set_table_wrap(tid, Some(crate::doc::Wrap::Side));
        a.set_table_margins(tid, 5.0, 6.0, 7.0, 8.0);
        let back = save_reload(&a, "table_margins");
        let t = back.tables.values().find(|t| t.anchor.is_some()).expect("anchor kept");
        assert_eq!(t.anchor.unwrap().dist, [5.0, 6.0, 7.0, 8.0]);
    }

    fn anchor_near_bottom_doc(rep: usize, fy: f32, h: f32) -> Document {
        let words = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        let mut d = Document::new();
        let p0 = crate::doc::Para::with_text(&words.repeat(rep).trim_end(), crate::doc::Style::Normal);
        let mut p1 = crate::doc::Para::with_text(&words.repeat(3).trim_end(), crate::doc::Style::Normal);
        p1.chars.push(crate::doc::OBJ);
        p1.fmts.push(CharFmt { image: 1, ..CharFmt::body() });
        d.paras = vec![p0, p1];
        d.images.insert(
            1,
            crate::doc::ImageData {
                bytes: Arc::new(Vec::new()),
                px_w: 400,
                px_h: 300,
                w: 220.0,
                h,
                raw: None,
                anchor: Some(crate::doc::Anchor {
                    h_rel: crate::doc::HRel::Page,
                    h_pos: crate::doc::PosSpec::Offset(190.0),
                    v_rel: crate::doc::VRel::Page,
                    v_pos: crate::doc::PosSpec::Offset(fy),
                    wrap: crate::doc::Wrap::Side,
                    behind: false,
                    dist: [0.0; 4],
                }),
            },
        );
        d
    }

    /// Regression: the picture's own wrap pushes the paragraph that anchors it
    /// onto the second page. A page-relative picture must nonetheless stay on
    /// the page it was placed on — if it migrated with the paragraph it would
    /// start influencing the text of the page it moved to.
    #[test]
    fn page_relative_float_stays_on_its_page() {
        for (rep, fy, h) in [(23usize, 80.0f32, 300.0f32), (23, 300.0, 300.0), (24, 80.0, 300.0)] {
            let mut a = app();
            a.doc = anchor_near_bottom_doc(rep, fy, h);
            para_tops(&mut a);
            let lay = a.layout.as_ref().unwrap();
            assert!(lay.pages >= 2, "rep={rep}: expected the text to spill onto a second page");
            let f = lay.floats.iter().find(|f| f.image == 1).expect("picture laid out");
            assert_eq!(f.page, 0, "rep={rep}: a page-relative picture must stay on the page it was placed on");
            for l in lay.lines.iter().filter(|l| l.page != f.page) {
                let overlaps = l.y < f.y + f.h && l.y + l.height > f.y;
                let short = !l.last_in_para
                    && (l.xs.last().copied().unwrap_or(f32::MAX) < f.x - 1.0 || l.xs.first().copied().unwrap_or(0.0) > f.x + f.w + 1.0);
                assert!(!(overlaps && short), "rep={rep}: line on page {} was shaped by the page-1 picture", l.page);
            }
        }
    }

    /// A page-relative, square-wrapped picture must not influence the text of a
    /// page it is not drawn on. Its wrap may only reshape the lines of the page
    /// it is actually drawn on; a line that flows onto the next page is free of
    /// it (it may still receive the text that overflows for lack of room).
    #[test]
    fn page_relative_float_does_not_wrap_the_next_page() {
        let words = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        let mut d = Document::new();
        // Two square-wrapped pictures on the first page: one top-left, one flush
        // with the lower right, both measured from the page, exactly like the
        // reported document.
        let mut p0 = crate::doc::Para::with_text(&words.repeat(4).trim_end(), crate::doc::Style::Normal);
        p0.chars.push(crate::doc::OBJ);
        p0.fmts.push(CharFmt { image: 1, ..CharFmt::body() });
        p0.chars.push(crate::doc::OBJ);
        p0.fmts.push(CharFmt { image: 2, ..CharFmt::body() });
        let filler: Vec<_> = [40usize, 30].iter().map(|n| crate::doc::Para::with_text(&words.repeat(*n).trim_end(), crate::doc::Style::Normal)).collect();
        d.paras = std::iter::once(p0).chain(filler).collect();
        let page_float = |w: f32, h: f32, x: f32, y: f32| crate::doc::ImageData {
            bytes: Arc::new(Vec::new()),
            px_w: 400,
            px_h: 300,
            w,
            h,
            raw: None,
            anchor: Some(crate::doc::Anchor {
                h_rel: crate::doc::HRel::Page,
                h_pos: crate::doc::PosSpec::Offset(x),
                v_rel: crate::doc::VRel::Page,
                v_pos: crate::doc::PosSpec::Offset(y),
                wrap: crate::doc::Wrap::Side,
                behind: false,
                dist: [0.0; 4],
            }),
        };
        d.images.insert(1, page_float(229.0, 407.0, 0.0, 0.0));
        d.images.insert(2, page_float(216.0, 250.0, 324.0, 450.0));

        let mut a = app();
        a.doc = d;
        para_tops(&mut a);
        let lay = a.layout.as_ref().unwrap();
        assert!(lay.pages >= 2, "expected the text to spill onto a second page, got {} page(s)", lay.pages);
        let f2 = lay.floats.iter().find(|f| f.image == 2).expect("image 2 laid out");
        assert_eq!(f2.page, 0, "a page-relative picture anchored on page 1 must stay on page 1");
        for l in lay.lines.iter().filter(|l| l.page != f2.page) {
            let overlaps = l.y < f2.y + f2.h && l.y + l.height > f2.y;
            let wraps = l.xs.windows(2).any(|w| w[0] < f2.x && w[1] > f2.x + f2.w - 1.0);
            assert!(!(overlaps && wraps), "line on page {} wraps around the page-1 picture: {:?}", l.page, l.xs);
        }
    }

    /// Reports lines whose right edge stops short of the text margin although they
    /// are not the last line of their paragraph: the sign of a page's wrap leaking
    /// into the next page.
    /// Lines on one page that were reshaped by a float drawn on another page.
    fn page_leaks(lay: &crate::layout::Layout) -> Vec<String> {
        let mut out = Vec::new();
        for fl in lay.floats.iter().filter(|f| f.side && !f.behind) {
            for l in lay.lines.iter().filter(|l| l.page != fl.page && !l.last_in_para) {
                let overlaps = l.y < fl.y + fl.h && l.y + l.height > fl.y;
                let first = l.xs.first().copied().unwrap_or(0.0);
                let last = l.xs.last().copied().unwrap_or(0.0);
                let ends_before = last <= fl.x + 6.0;
                let starts_after = first >= fl.x + fl.w - 6.0;
                let gap = l.xs.windows(2).any(|w| w[0] < fl.x - 1.0 && w[1] > fl.x + fl.w - 1.0);
                if overlaps && (ends_before || starts_after || gap) {
                    out.push(format!("pg{} para{} y{:.0} first{:.0} last{:.0} fx{:.0} gap{}", l.page, l.para, l.y, first, last, fl.x, gap));
                }
            }
        }
        out
    }

    /// A square-wrapped picture whose rectangle reaches into the bottom margin
    /// must not reshape the text of the next page: a paragraph that spills over
    /// the page break must start the new page with full-width lines.
    #[test]
    fn float_reaching_the_bottom_margin_does_not_wrap_the_next_page() {
        let words = "Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ";
        let mut a = app();
        let mut spanned = false;
        for rep in 18usize..40 {
            let mut d = Document::new();
            let mut p = crate::doc::Para::with_text("", crate::doc::Style::Normal);
            p.chars.push(crate::doc::OBJ);
            p.fmts.push(CharFmt { image: 1, ..CharFmt::body() });
            let body: String = words.repeat(rep).trim_end().into();
            let n = body.chars().count();
            p.chars.extend(body.chars());
            p.fmts.extend(std::iter::repeat_with(CharFmt::body).take(n));
            d.paras = vec![p];
            d.images.insert(
                1,
                crate::doc::ImageData {
                    bytes: Arc::new(Vec::new()),
                    px_w: 300,
                    px_h: 400,
                    w: 150.0,
                    h: 200.0,
                    raw: None,
                    anchor: Some(crate::doc::Anchor {
                        h_rel: crate::doc::HRel::Page,
                        h_pos: crate::doc::PosSpec::Offset(300.0),
                        v_rel: crate::doc::VRel::Page,
                        v_pos: crate::doc::PosSpec::Offset(637.0),
                        wrap: crate::doc::Wrap::Side,
                        behind: false,
                        dist: [0.0; 4],
                    }),
                },
            );
            a.doc = d;
            para_tops(&mut a);
            let lay = a.layout.as_ref().unwrap();
            let leaks = page_leaks(lay);
            assert!(leaks.is_empty(), "rep={rep}: a page-1 picture reshaped page-2 text: {leaks:?}");
            spanned |= lay.pages >= 2;
        }
        assert!(spanned, "the scan must include a paragraph that spills onto a second page");
    }

    #[test]
    fn copy_paste_keeps_tables_whole_and_remaps_ids() {
        let mut a = app();
        a.insert_table(2, 2);
        a.doc.paras[0].chars = "A".chars().collect();
        a.doc.paras[0].fmts = vec![CharFmt::body()];
        let end = a.doc.end();
        a.select(Pos::new(0, 0), end);
        a.copy();
        let text = a.clip.as_ref().expect("clip").text.clone();
        a.set_cursor(a.doc.end(), false);
        a.paste_text(&text);
        assert_eq!(a.doc.tables.len(), 2, "the copied table must be a separate table");
        let ids: Vec<u32> = a.doc.tables.keys().copied().collect();
        assert_ne!(ids[0], ids[1]);
        for id in ids {
            assert!(!a.doc.table_range(id).is_empty(), "every table has its paragraphs");
        }
        for p in &a.doc.paras {
            if let Some(c) = p.cell {
                assert!(a.doc.tables.contains_key(&c.table), "cell points at a live table");
            }
        }
    }

    #[test]
    fn html_table_spans_become_cells() {
        let mut a = app();
        let html = "<table><tr><td rowspan=2>A</td><td>B</td></tr><tr><td colspan=2>C</td></tr></table>";
        a.insert_html_table(crate::htmltable::parse(html).unwrap());
        let t = a.doc.tables.values().next().expect("table");
        assert_eq!(t.grid.len(), 3, "rowspan/colspan widest row has three columns");
        assert_eq!(a.doc.paras.iter().filter(|p| p.cell.is_some()).count(), 4, "four physical cells");
        assert_eq!(t.cells.get(&(0, 0)).unwrap().vmerge, crate::doc::VMerge::Restart);
        assert_eq!(t.cells.get(&(1, 0)).unwrap().vmerge, crate::doc::VMerge::Continue);
        assert_eq!(t.cells.get(&(1, 1)).unwrap().span, 2);
        assert!(a.doc.paras.iter().any(|p| p.cell.is_some() && p.text() == "C"));
    }

    #[test]
    fn headings_use_word_levels_and_builtin_styles() {
        let mut d = Document::new();
        let mut p1 = crate::doc::Para::with_text("One", crate::doc::Style::Heading1);
        p1.style_id = Some("Heading1".into());
        let mut p4 = crate::doc::Para::with_text("Four", crate::doc::Style::Normal);
        p4.style_id = Some("Heading4".into());
        let p2 = crate::doc::Para::with_text("Two", crate::doc::Style::Heading2);
        let body = crate::doc::Para::with_text("Body", crate::doc::Style::Normal);
        d.paras = vec![p1, body, p4, p2];
        let hs = d.headings();
        assert_eq!(hs.iter().map(|h| (h.para, h.level, h.text.as_str())).collect::<Vec<_>>(), vec![
            (0, 1, "One"),
            (2, 4, "Four"),
            (3, 2, "Two"),
        ]);
    }
}