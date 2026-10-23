//! Tolerant parser for HTML tables found on the system clipboard.
//!
//! Word, Excel and browsers put a CF_HTML fragment on the clipboard when the
//! user copies a table. The fragment is not always well-formed XML, so this
//! module scans tags instead of relying on an XML DOM. It only extracts the
//! first table: cell text, `colspan`, `rowspan` and basic bold/italic flags.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HtmlCell {
    pub text: String,
    pub span: u32,
    pub rowspan: u32,
    pub bold: bool,
    pub italic: bool,
    pub align: Option<crate::doc::Align>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HtmlTable {
    pub rows: Vec<Vec<HtmlCell>>,
}

impl HtmlTable {
    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| r.is_empty())
    }
}

#[derive(Default)]
struct CellDraft {
    text: String,
    span: u32,
    rowspan: u32,
    bold: bool,
    italic: bool,
    align: Option<crate::doc::Align>,
}

/// Parses the first HTML table in `html`. Returns `None` when there is no table
/// or the table has no cells.
pub fn parse(html: &str) -> Option<HtmlTable> {
    let bytes = html.as_bytes();
    let mut i = 0usize;
    let mut depth = 0usize;
    let mut rows: Vec<Vec<HtmlCell>> = Vec::new();
    let mut row: Option<Vec<HtmlCell>> = None;
    let mut cell: Option<CellDraft> = None;
    let mut bold = 0usize;
    let mut italic = 0usize;

    while i < bytes.len() {
        if html[i..].starts_with("<!--") {
            i = html[i + 4..].find("-->").map(|n| i + 4 + n + 3).unwrap_or(bytes.len());
            continue;
        }
        if bytes[i] != b'<' {
            let end = html[i..].find('<').map(|n| i + n).unwrap_or(bytes.len());
            if depth == 1 {
                if let Some(c) = cell.as_mut() {
                    append_text(&mut c.text, &html[i..end]);
                }
            }
            i = end;
            continue;
        }

        let Some(end) = tag_end(html, i + 1) else { break };
        let raw = &html[i + 1..end];
        i = end + 1;
        let raw = raw.trim();
        if raw.starts_with('!') || raw.starts_with('?') {
            continue;
        }
        let closing = raw.starts_with('/');
        let body = raw.trim_start_matches('/').trim_start();
        let name: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == ':' || *c == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }

        if name == "table" {
            if closing {
                if depth == 1 {
                    finish_cell(&mut cell, &mut row, bold, italic);
                    finish_row(&mut row, &mut rows);
                    break;
                }
                depth = depth.saturating_sub(1);
            } else {
                depth += 1;
                if depth > 1 {
                    // Ignore nested tables (layout tables inside cells).
                    continue;
                }
            }
            continue;
        }
        if depth != 1 {
            continue;
        }

        match name.as_str() {
            "tr" => {
                finish_cell(&mut cell, &mut row, bold, italic);
                if closing {
                    finish_row(&mut row, &mut rows);
                } else {
                    finish_row(&mut row, &mut rows);
                    row = Some(Vec::new());
                    cell = None;
                    bold = 0;
                    italic = 0;
                }
            }
            "td" | "th" => {
                finish_cell(&mut cell, &mut row, bold, italic);
                if closing {
                    cell = None;
                } else {
                    let mut d = CellDraft {
                        span: attr_u32(body, "colspan").unwrap_or(1).max(1),
                        rowspan: attr_u32(body, "rowspan").unwrap_or(1).max(1),
                        bold: name == "th",
                        ..Default::default()
                    };
                    d.align = attr_align(body);
                    cell = Some(d);
                    bold = 0;
                    italic = 0;
                }
            }
            "br" => {
                if let Some(c) = cell.as_mut() {
                    push_break(&mut c.text);
                }
            }
            "p" | "div" | "li" => {
                if let Some(c) = cell.as_mut() {
                    push_break(&mut c.text);
                }
            }
            "b" | "strong" => {
                if closing {
                    bold = bold.saturating_sub(1);
                } else if !body.ends_with('/') {
                    bold += 1;
                    if let Some(c) = cell.as_mut() {
                        c.bold = true;
                    }
                }
            }
            "i" | "em" => {
                if closing {
                    italic = italic.saturating_sub(1);
                } else if !body.ends_with('/') {
                    italic += 1;
                    if let Some(c) = cell.as_mut() {
                        c.italic = true;
                    }
                }
            }
            _ => {}
        }
    }

    finish_cell(&mut cell, &mut row, bold, italic);
    finish_row(&mut row, &mut rows);
    let rows: Vec<Vec<HtmlCell>> = rows.into_iter().filter(|r| !r.is_empty()).collect();
    (!rows.is_empty()).then_some(HtmlTable { rows })
}

fn tag_end(html: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (n, c) in html[start..].char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(start + n),
            _ => {}
        }
    }
    None
}

fn finish_cell(cell: &mut Option<CellDraft>, row: &mut Option<Vec<HtmlCell>>, bold: usize, italic: usize) {
    let Some(mut c) = cell.take() else { return };
    c.text = normalize_cell_text(&c.text);
    c.bold |= bold > 0;
    c.italic |= italic > 0;
    row.get_or_insert_with(Vec::new).push(HtmlCell {
        text: c.text,
        span: c.span.max(1),
        rowspan: c.rowspan.max(1),
        bold: c.bold,
        italic: c.italic,
        align: c.align,
    });
}

fn finish_row(row: &mut Option<Vec<HtmlCell>>, rows: &mut Vec<Vec<HtmlCell>>) {
    if let Some(r) = row.take() {
        if !r.is_empty() {
            rows.push(r);
        }
    }
}

fn push_break(s: &mut String) {
    while s.ends_with(' ') || s.ends_with('\t') {
        s.pop();
    }
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
}

fn append_text(out: &mut String, raw: &str) {
    let decoded = decode_entities(raw);
    for c in decoded.chars() {
        match c {
            '\r' | '\n' | '\t' => {
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
            }
            c if c.is_whitespace() => {
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
            }
            c => out.push(c),
        }
    }
}

fn normalize_cell_text(s: &str) -> String {
    let mut out = String::new();
    for line in s.replace('\r', "\n").split('\n') {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&line);
        }
    }
    out.trim().to_string()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|e| *e <= 12) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..end];
        let ch = match ent.to_ascii_lowercase().as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        if let Some(ch) = ch {
            out.push(ch);
            rest = &tail[end + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn attr_u32(tag: &str, name: &str) -> Option<u32> {
    let v = attr(tag, name)?;
    v.trim().trim_matches('"').trim_matches('\'').parse().ok()
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(n) = lower[from..].find(name) {
        let at = from + n;
        let before = lower[..at].chars().next_back();
        let after = lower[at + name.len()..].chars().next();
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
            || after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            from = at + name.len();
            continue;
        }
        let mut rest = &tag[at + name.len()..];
        rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            from = at + name.len();
            continue;
        };
        let rest = rest.trim_start();
        let val = if let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let mut chars = rest[1..].char_indices();
            let end = chars.find(|(_, c)| *c == q).map(|(i, _)| i).unwrap_or(rest.len());
            &rest[1..1 + end]
        } else {
            rest.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or("")
        };
        return Some(val.trim_end_matches('/').to_string());
    }
    None
}

fn attr_align(tag: &str) -> Option<crate::doc::Align> {
    use crate::doc::Align;
    let style = attr(tag, "style").unwrap_or_default().to_ascii_lowercase();
    if style.contains("text-align:center") || style.contains("text-align: center") {
        return Some(Align::Center);
    }
    if style.contains("text-align:right") || style.contains("text-align: right") {
        return Some(Align::Right);
    }
    if style.contains("text-align:justify") || style.contains("text-align: justify") {
        return Some(Align::Justify);
    }
    attr(tag, "align").and_then(|a| match a.to_ascii_lowercase().as_str() {
        "center" => Some(Align::Center),
        "right" => Some(Align::Right),
        "justify" => Some(Align::Justify),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_word_and_excel_tables() {
        let html = r#"<html><body>
            <!--StartFragment-->
            <table border=1>
              <tr><th>Name</th><th colspan="2">Values</th></tr>
              <tr><td>A &amp; B</td><td>1</td><td><b>2</b></td></tr>
              <tr><td>Next</td><td colspan="2">wide</td></tr>
            </table>
            <!--EndFragment-->
        </body></html>"#;
        let t = parse(html).expect("table");
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.rows[0][0].text, "Name");
        assert!(t.rows[0][0].bold);
        assert_eq!(t.rows[0][1].span, 2);
        assert_eq!(t.rows[1][0].text, "A & B");
        assert!(t.rows[1][2].bold);
        assert_eq!(t.rows[2][1].text, "wide");
        assert_eq!(t.rows[2][1].span, 2);
    }

    #[test]
    fn parses_rowspan_and_breaks() {
        let html = "<table><tr><td rowspan=2>left<br>top</td><td>a</td></tr><tr><td>b</td></tr></table>";
        let t = parse(html).expect("table");
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0][0].rowspan, 2);
        assert_eq!(t.rows[0][0].text, "left\ntop");
        assert_eq!(t.rows[1][0].text, "b");
    }

    #[test]
    fn ignores_nested_tables_and_non_tables() {
        assert!(parse("<p>no table</p>").is_none());
        let t = parse("<table><tr><td>outer<table><tr><td>inner</td></tr></table></td></tr></table>").unwrap();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0][0].text, "outer");
    }

    #[test]
    fn decodes_numeric_entities_and_align() {
        let t = parse("<table><tr><td style='text-align:center'>&#65;&#x42;</td></tr></table>").unwrap();
        assert_eq!(t.rows[0][0].text, "AB");
        assert_eq!(t.rows[0][0].align, Some(crate::doc::Align::Center));
    }
}
