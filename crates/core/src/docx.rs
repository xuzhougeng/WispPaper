//! Word files (.docx) for 写作. One is read into its paragraphs, its
//! headings and the comments on them (`read`), and written again saying what
//! the manuscript says now, with the manuscript's comments (`rewrite`).
//!
//! Writing again changes as little as it can. The paragraphs of the file
//! and of the manuscript are matched by what they say (`align`); one that
//! says the same is left as it is to the byte, with its formatting, fields
//! and pictures; one that changed is changed inside the runs it has
//! (`Para::edited`), so the words that stayed keep their formatting and
//! what was put in takes that of the words before it. Tables, pictures,
//! footnotes, headers and everything else the manuscript does not hold stay
//! where they were. With `Options::track` the changes are written as Word's
//! revisions (`w:ins`, `w:del`), for the reader to accept or reject.
//!
//! A comment is Word's own (`word/comments.xml`, with the range it is about
//! marked in the text); those the file came with are never touched.
//!
//! The archive and the XML are read by hand: a paragraph that is not
//! changed must come out as it went in, which a parser that builds a tree
//! and writes it again would not promise.

use crate::ai::diff_tokens;
use crate::manuscript::{crc32, xml_escape as escape};
use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::ops::Range;

const NOT_DOCX: &str = "这不是 Word 的 .docx 文件（旧的 .doc 和加了密码的文件读不了）。";

// ---- the archive

struct Entry {
    name: String,
    /// 0 stored, 8 deflated: `data` is as it stands in the archive.
    method: u16,
    crc: u32,
    size: u32,
    data: Vec<u8>,
}

/// A zip archive: read whole, its entries kept as they are packed, so one that is not changed is written back unchanged.
pub struct Zip {
    entries: Vec<Entry>,
}

fn le(bytes: &[u8], at: usize, width: usize) -> Result<usize> {
    let piece = at.checked_add(width).and_then(|end| bytes.get(at..end)).ok_or_else(|| anyhow!(NOT_DOCX))?;
    Ok(piece.iter().rev().fold(0usize, |n, b| n << 8 | *b as usize))
}

impl Zip {
    pub fn read(bytes: &[u8]) -> Result<Zip> {
        // The directory is named at the end of the file, before at most a comment.
        let end = (0..bytes.len().saturating_sub(21)).rev().take(70_000).find(|&at| bytes[at..].starts_with(b"PK\x05\x06")).ok_or_else(|| anyhow!(NOT_DOCX))?;
        let (count, mut at) = (le(bytes, end + 10, 2)?, le(bytes, end + 16, 4)?);
        if count == 0xFFFF || at == 0xFFFF_FFFF { bail!("这个文件太大，读不了。") }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            if le(bytes, at, 4)? != 0x0201_4b50 { bail!(NOT_DOCX) }
            let (flags, method, crc, packed, size) = (le(bytes, at + 8, 2)?, le(bytes, at + 10, 2)?, le(bytes, at + 16, 4)?, le(bytes, at + 20, 4)?, le(bytes, at + 24, 4)?);
            let (name, extra, comment, local) = (le(bytes, at + 28, 2)?, le(bytes, at + 30, 2)?, le(bytes, at + 32, 2)?, le(bytes, at + 42, 4)?);
            if flags & 1 != 0 { bail!("这个 Word 文件加了密码，读不了。") }
            if method != 0 && method != 8 { bail!(NOT_DOCX) }
            let start = local + 30 + le(bytes, local + 26, 2)? + le(bytes, local + 28, 2)?;
            let data = start.checked_add(packed).and_then(|end| bytes.get(start..end)).ok_or_else(|| anyhow!(NOT_DOCX))?.to_vec();
            let name_bytes = bytes.get(at + 46..at + 46 + name).ok_or_else(|| anyhow!(NOT_DOCX))?;
            entries.push(Entry { name: String::from_utf8_lossy(name_bytes).into_owned(), method: method as u16, crc: crc as u32, size: size as u32, data });
            at += 46 + name + extra + comment;
        }
        Ok(Zip { entries })
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|entry| entry.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.name.as_str())
    }

    /// An entry's content, unpacked.
    pub fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let Some(entry) = self.entries.iter().find(|entry| entry.name == name) else { return Ok(None) };
        if entry.method == 0 { return Ok(Some(entry.data.clone())) }
        let mut out = Vec::with_capacity(entry.size as usize);
        // No part of a document is this large: an archive that says so is not one.
        flate2::read::DeflateDecoder::new(&entry.data[..]).take(1 << 29).read_to_end(&mut out).map_err(|_| anyhow!(NOT_DOCX))?;
        Ok(Some(out))
    }

    /// An entry as text; empty when it is not there.
    pub fn text(&self, name: &str) -> Result<String> {
        Ok(self.get(name)?.map(|bytes| String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_string()).unwrap_or_default())
    }

    /// Put an entry in, or another content in its place.
    pub fn set(&mut self, name: &str, data: &[u8]) {
        let mut packer = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        let packed = packer.write_all(data).and_then(|_| packer.finish()).ok().filter(|packed| packed.len() < data.len());
        let entry = Entry { name: name.to_string(), method: if packed.is_some() { 8 } else { 0 }, crc: crc32(data), size: data.len() as u32, data: packed.unwrap_or_else(|| data.to_vec()) };
        match self.entries.iter_mut().find(|entry| entry.name == name) {
            Some(found) => *found = entry,
            None => self.entries.push(entry),
        }
    }

    pub fn write(&self) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        let u16le = |v: &mut Vec<u8>, n: u16| v.extend_from_slice(&n.to_le_bytes());
        let u32le = |v: &mut Vec<u8>, n: u32| v.extend_from_slice(&n.to_le_bytes());
        for entry in &self.entries {
            let (offset, packed) = (out.len() as u32, entry.data.len() as u32);
            // Version 2.0, UTF-8 names (bit 11), dated 1980-01-01.
            u32le(&mut out, 0x0403_4b50);
            for n in [20, 0x0800, entry.method, 0, 0x21] { u16le(&mut out, n) }
            for n in [entry.crc, packed, entry.size] { u32le(&mut out, n) }
            u16le(&mut out, entry.name.len() as u16);
            u16le(&mut out, 0);
            out.extend_from_slice(entry.name.as_bytes());
            out.extend_from_slice(&entry.data);
            u32le(&mut central, 0x0201_4b50);
            for n in [20, 20, 0x0800, entry.method, 0, 0x21] { u16le(&mut central, n) }
            for n in [entry.crc, packed, entry.size] { u32le(&mut central, n) }
            for n in [entry.name.len() as u16, 0, 0, 0, 0] { u16le(&mut central, n) }
            u32le(&mut central, 0);
            u32le(&mut central, offset);
            central.extend_from_slice(entry.name.as_bytes());
        }
        let (start, length) = (out.len() as u32, central.len() as u32);
        out.extend_from_slice(&central);
        u32le(&mut out, 0x0605_4b50);
        for n in [0, 0, self.entries.len() as u16, self.entries.len() as u16] { u16le(&mut out, n) }
        u32le(&mut out, length);
        u32le(&mut out, start);
        u16le(&mut out, 0);
        out
    }

    /// The part that holds the document's text: named by the package, `word/document.xml` by custom.
    fn main(&self) -> Result<String> {
        let named = relationships(&self.text("_rels/.rels")?).into_iter().find(|(kind, _)| kind.ends_with("/officeDocument")).map(|(_, target)| target.trim_start_matches('/').to_string());
        let main = named.unwrap_or_else(|| "word/document.xml".into());
        if !self.has(&main) { bail!("这个文件里没有 Word 文档的正文。") }
        Ok(main)
    }
}

/// The relationships of a part: (type, target).
fn relationships(xml: &str) -> Vec<(String, String)> {
    let (mut out, mut at) = (vec![], 0);
    while let Some(found) = xml[at..].find("<Relationship ") {
        let Some((Node::Open(_, tag) | Node::Empty(_, tag), end)) = node(xml, at + found) else { break };
        out.push((attr(tag, "Type").unwrap_or_default(), attr(tag, "Target").unwrap_or_default()));
        at = end;
    }
    out
}

/// Where a part's relationships are kept.
fn rels_of(part: &str) -> String {
    let (folder, file) = part.rsplit_once('/').unwrap_or(("", part));
    if folder.is_empty() { format!("_rels/{file}.rels") } else { format!("{folder}/_rels/{file}.rels") }
}

/// The part a relationship of `part` leads to.
fn beside(part: &str, target: &str) -> String {
    if let Some(absolute) = target.strip_prefix('/') { return absolute.to_string() }
    let mut path: Vec<&str> = part.split('/').collect();
    path.pop();
    for piece in target.split('/') {
        match piece {
            ".." => drop(path.pop()),
            "." | "" => {}
            piece => path.push(piece),
        }
    }
    path.join("/")
}

// ---- XML, as it is written

#[derive(Clone, Copy, Debug, PartialEq)]
enum Node<'a> {
    /// A tag that opens an element: its name, and the tag as written.
    Open(&'a str, &'a str),
    /// An element without content.
    Empty(&'a str, &'a str),
    Close,
    /// Text between tags; a comment or an instruction, which are kept the same way.
    Text(&'a str),
}

/// The piece of XML that starts at `at`, and where the next one starts.
fn node(src: &str, at: usize) -> Option<(Node<'_>, usize)> {
    let rest = src.get(at..).filter(|rest| !rest.is_empty())?;
    if !rest.starts_with('<') {
        let end = rest.find('<').unwrap_or(rest.len());
        return Some((Node::Text(&rest[..end]), at + end));
    }
    for (open, close) in [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>"), ("<!", ">")] {
        if rest.starts_with(open) {
            let end = rest.find(close).map_or(rest.len(), |i| i + close.len());
            return Some((Node::Text(&rest[..end]), at + end));
        }
    }
    // The tag ends at the first `>` outside a quoted value.
    let (mut quote, mut end) = (0u8, rest.len());
    for (i, b) in rest.bytes().enumerate() {
        if quote != 0 {
            if b == quote { quote = 0 }
        } else if b == b'"' || b == b'\'' {
            quote = b
        } else if b == b'>' {
            end = i + 1;
            break;
        }
    }
    let tag = &rest[..end];
    if tag.starts_with("</") { return Some((Node::Close, at + end)) }
    let name = tag[1..].split(|c: char| c.is_whitespace() || c == '/' || c == '>').next().unwrap_or("");
    Some((if tag.ends_with("/>") { Node::Empty(name, tag) } else { Node::Open(name, tag) }, at + end))
}

/// Where the element (or other piece) that starts at `at` ends.
fn element_end(src: &str, at: usize) -> usize {
    let (mut depth, mut at) = (0usize, at);
    while let Some((piece, end)) = node(src, at) {
        match piece {
            Node::Open(..) => depth += 1,
            Node::Close => depth = depth.saturating_sub(1),
            _ => {}
        }
        at = end;
        if depth == 0 { break }
    }
    at
}

/// The pieces of an element's content: each element whole under its name, anything else without one.
fn children(inner: &str) -> Vec<(&str, &str)> {
    let (mut out, mut at) = (vec![], 0);
    while let Some((piece, end)) = node(inner, at) {
        let (name, end) = match piece {
            Node::Open(name, _) => (name, element_end(inner, at)),
            Node::Empty(name, _) => (name, end),
            Node::Close | Node::Text(_) => ("", end),
        };
        out.push((name, &inner[at..end]));
        at = end;
    }
    out
}

fn open_tag(xml: &str) -> &str {
    match node(xml, 0) {
        Some((Node::Open(_, tag) | Node::Empty(_, tag), _)) => tag,
        _ => "",
    }
}

/// What is between an element's opening and closing tags.
fn inside(xml: &str) -> &str {
    let open = open_tag(xml);
    if open.is_empty() || open.ends_with("/>") { return "" }
    let end = xml.rfind("</").unwrap_or(xml.len());
    xml.get(open.len()..end).unwrap_or("")
}

fn has_content(xml: &str) -> bool {
    let open = open_tag(xml);
    !open.is_empty() && !open.ends_with("/>")
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(found) = tag[from..].find(name) {
        let at = from + found;
        let after = &tag[at + name.len()..];
        if tag[..at].ends_with(char::is_whitespace) && after.starts_with('=') {
            let quote = after[1..].chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let value = &after[2..];
            return Some(unescape(&value[..value.find(quote)?]));
        }
        from = at + name.len();
    }
    None
}

/// The `w:val` of the first element of this name in `xml`.
fn val_of(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(found) = xml[from..].find(&open) {
        let at = from + found;
        if xml[at + open.len()..].starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
            return match node(xml, at) {
                Some((Node::Open(_, tag) | Node::Empty(_, tag), _)) => attr(tag, "w:val"),
                _ => None,
            };
        }
        from = at + open.len();
    }
    None
}

/// The switch of this name (`w:b`, `w:i`) is set in `xml`: there, and its `w:val` not one that turns it off.
fn switched_on(xml: &str, name: &str) -> bool {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(found) = xml[from..].find(&open) {
        let at = from + found;
        if xml[at + open.len()..].starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
            return !matches!(val_of(&xml[at..], name).as_deref(), Some("0" | "false" | "off"));
        }
        from = at + open.len();
    }
    false
}

/// `xml` without the elements of this name.
fn without(xml: &str, name: &str) -> String {
    let (mut out, mut at, open) = (String::new(), 0, format!("<{name}"));
    while let Some(found) = xml[at..].find(&open) {
        let start = at + found;
        let after = start + open.len();
        if xml[after..].starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
            out.push_str(&xml[at..start]);
            at = element_end(xml, start);
        } else {
            out.push_str(&xml[at..after]);
            at = after;
        }
    }
    out.push_str(&xml[at..]);
    out
}

fn unescape(text: &str) -> String {
    if !text.contains('&') { return text.to_string() }
    let (mut out, mut rest) = (String::with_capacity(text.len()), text);
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let named = rest.find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let code = if let Some(hex) = name.strip_prefix("#x").or(name.strip_prefix("#X")) { u32::from_str_radix(hex, 16).ok() } else { name.strip_prefix('#').and_then(|n| n.parse().ok()) };
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => code.and_then(char::from_u32),
            };
            c.map(|c| (c, end + 1))
        });
        match named {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn text_xml(tag: &str, text: &str) -> String {
    format!("<{tag} xml:space=\"preserve\">{}</{tag}>", escape(text))
}

// ---- a paragraph

/// A paragraph as far as its text goes: runs of text, what wraps runs (a
/// link, a field, another's insertion), and everything else as written.
#[derive(Clone, Debug)]
struct Para {
    /// The opening tag.
    open: String,
    /// `w:pPr` as written; empty without one.
    ppr: String,
    items: Vec<Item>,
}

#[derive(Clone, Debug)]
enum Item {
    Run(Run),
    /// `ins`: the runs inside are another's insertion that is not accepted yet.
    Wrap { open: String, close: String, items: Vec<Item>, ins: bool },
    /// Without text of its own: a bookmark, where a comment starts, text that was struck, an equation.
    Raw(String),
}

#[derive(Clone, Debug)]
struct Run {
    open: String,
    rpr: String,
    parts: Vec<Part>,
}

#[derive(Clone, Debug)]
enum Part {
    Text(String),
    /// A tab or a line break: written as it was, read as this one character.
    Shown(String, &'static str),
    /// A footnote's number, a picture, a field's code: no text.
    Raw(String),
}

impl Part {
    fn len(&self) -> usize {
        match self {
            Part::Text(text) => text.len(),
            Part::Shown(_, shown) => shown.len(),
            Part::Raw(_) => 0,
        }
    }
}

/// What goes with the words around it when they are struck: a picture or a note's number, not a mark that has its other half elsewhere.
fn is_content(xml: &str) -> bool {
    ["<w:drawing", "<w:pict", "<w:object", "<w:footnoteReference", "<w:endnoteReference", "<w:sym", "<mc:AlternateContent"].iter().any(|open| xml.starts_with(open))
}

fn parse_run(xml: &str) -> Run {
    let mut run = Run { open: open_tag(xml).to_string(), rpr: String::new(), parts: vec![] };
    for (name, piece) in children(inside(xml)) {
        match name {
            "w:rPr" if run.rpr.is_empty() && run.parts.is_empty() => run.rpr = piece.to_string(),
            "w:t" if has_content(piece) => run.parts.push(Part::Text(unescape(inside(piece)))),
            "w:tab" | "w:cr" => run.parts.push(Part::Shown(piece.to_string(), " ")),
            "w:br" if matches!(attr(open_tag(piece), "w:type").as_deref(), None | Some("textWrapping")) => run.parts.push(Part::Shown(piece.to_string(), " ")),
            "w:noBreakHyphen" => run.parts.push(Part::Shown(piece.to_string(), "-")),
            _ => run.parts.push(Part::Raw(piece.to_string())),
        }
    }
    run
}

fn parse_item(name: &str, xml: &str) -> Item {
    if !has_content(xml) { return Item::Raw(xml.to_string()) }
    match name {
        "w:r" => Item::Run(parse_run(xml)),
        "w:hyperlink" | "w:smartTag" | "w:fldSimple" | "w:ins" | "w:moveTo" | "w:customXml" | "w:dir" | "w:bdo" => {
            Item::Wrap { open: open_tag(xml).to_string(), close: format!("</{name}>"), items: children(inside(xml)).into_iter().map(|(name, piece)| parse_item(name, piece)).collect(), ins: name == "w:ins" || name == "w:moveTo" }
        }
        // A content control: its runs are in `w:sdtContent`, after its properties.
        "w:sdt" => {
            let Some((_, content)) = children(inside(xml)).into_iter().find(|(name, piece)| *name == "w:sdtContent" && has_content(piece)) else { return Item::Raw(xml.to_string()) };
            let start = content.as_ptr() as usize - xml.as_ptr() as usize;
            let (opened, closed) = (start + open_tag(content).len(), start + content.len() - "</w:sdtContent>".len());
            if !content.ends_with("</w:sdtContent>") || closed < opened { return Item::Raw(xml.to_string()) }
            Item::Wrap { open: xml[..opened].to_string(), close: xml[closed..].to_string(), items: children(&xml[opened..closed]).into_iter().map(|(name, piece)| parse_item(name, piece)).collect(), ins: false }
        }
        _ => Item::Raw(xml.to_string()),
    }
}

/// What a paragraph is made of, in order.
enum Leaf<'a> {
    Text(&'a str),
    Raw(&'a str),
}

fn leaves<'a>(items: &'a [Item], f: &mut dyn FnMut(Leaf<'a>)) {
    for item in items {
        match item {
            Item::Raw(xml) => f(Leaf::Raw(xml)),
            Item::Wrap { items, .. } => leaves(items, f),
            Item::Run(run) => {
                for part in &run.parts {
                    match part {
                        Part::Text(text) => f(Leaf::Text(text)),
                        Part::Shown(_, shown) => f(Leaf::Text(shown)),
                        Part::Raw(xml) => f(Leaf::Raw(xml)),
                    }
                }
            }
        }
    }
}

fn write_items(items: &[Item], out: &mut String) {
    for item in items {
        match item {
            Item::Raw(xml) => out.push_str(xml),
            Item::Wrap { open, close, items, .. } => {
                out.push_str(open);
                write_items(items, out);
                out.push_str(close);
            }
            Item::Run(run) => {
                out.push_str(&run.open);
                out.push_str(&run.rpr);
                for part in &run.parts {
                    match part {
                        Part::Text(text) => out.push_str(&text_xml("w:t", text)),
                        Part::Shown(xml, _) | Part::Raw(xml) => out.push_str(xml),
                    }
                }
                out.push_str("</w:r>");
            }
        }
    }
}

impl Run {
    fn len(&self) -> usize {
        self.parts.iter().map(Part::len).sum()
    }

    /// Cut the run in two at byte `cut` of its text: this one keeps what is before, the one returned has the rest.
    fn split(&mut self, cut: usize) -> Run {
        let mut right = Run { open: self.open.clone(), rpr: self.rpr.clone(), parts: vec![] };
        let mut pos = 0;
        for part in std::mem::take(&mut self.parts) {
            let len = part.len();
            if pos + len <= cut {
                self.parts.push(part);
            } else if pos >= cut {
                right.parts.push(part);
            } else if let Part::Text(text) = part {
                let at = text.floor_char_boundary(cut - pos);
                self.parts.push(Part::Text(text[..at].to_string()));
                right.parts.push(Part::Text(text[at..].to_string()));
            } else {
                right.parts.push(part);
            }
            pos += len;
        }
        right
    }
}

/// Put `xml` at byte `at` of the text these items hold, cutting a run where it falls inside one. Whether it found the place.
fn place(items: &mut Vec<Item>, pos: &mut usize, at: usize, xml: &str, end: bool) -> bool {
    let mut n = 0;
    while n < items.len() {
        let mut cut = None;
        match &mut items[n] {
            Item::Raw(_) => {}
            Item::Wrap { items: inner, .. } => {
                if place(inner, pos, at, xml, end) { return true }
            }
            Item::Run(run) => {
                let (from, to) = (*pos, *pos + run.len());
                *pos = to;
                // A range starts before the character at `at`, and ends after the one before it.
                if if end { from < at && at <= to } else { from <= at && at < to } {
                    cut = Some(if at == from { (n, None) } else if at == to { (n + 1, None) } else { (n + 1, Some(run.split(at - from))) });
                }
            }
        }
        if let Some((index, right)) = cut {
            items.insert(index, Item::Raw(xml.to_string()));
            if let Some(right) = right { items.insert(index + 1, Item::Run(right)) }
            return true;
        }
        n += 1;
    }
    false
}

impl Para {
    fn parse(xml: &str) -> Para {
        let open = open_tag(xml);
        let mut para = Para { open: open.strip_suffix("/>").map_or(open.to_string(), |tag| format!("{}>", tag.trim_end())), ppr: String::new(), items: vec![] };
        for (name, piece) in children(inside(xml)) {
            if name == "w:pPr" && para.ppr.is_empty() && para.items.is_empty() { para.ppr = piece.to_string() } else { para.items.push(parse_item(name, piece)) }
        }
        para
    }

    /// What the paragraph says: text that was struck does not count, text put in does.
    fn text(&self) -> String {
        let mut out = String::new();
        leaves(&self.items, &mut |leaf| {
            if let Leaf::Text(text) = leaf { out.push_str(text) }
        });
        out
    }

    /// Every run with words in it is set in bold: a heading made by hand, without a heading style.
    fn bold(&self) -> bool {
        fn runs(items: &[Item], seen: &mut usize, bold: &mut bool) {
            for item in items {
                match item {
                    Item::Run(run) => {
                        if run.parts.iter().any(|part| matches!(part, Part::Text(text) if !text.trim().is_empty())) {
                            *seen += 1;
                            *bold &= switched_on(&run.rpr, "w:b");
                        }
                    }
                    Item::Wrap { items, .. } => runs(items, seen, bold),
                    Item::Raw(_) => {}
                }
            }
        }
        let (mut seen, mut bold) = (0, true);
        runs(&self.items, &mut seen, &mut bold);
        seen > 0 && bold
    }

    fn xml(&self) -> String {
        let mut out = format!("{}{}", self.open, self.ppr);
        write_items(&self.items, &mut out);
        out.push_str("</w:p>");
        out
    }

    /// Put something without width at byte `at` of the text: where a comment's range starts, or (`end`) where it ends.
    fn insert(&mut self, at: usize, xml: &str, end: bool) {
        let mut pos = 0;
        if place(&mut self.items, &mut pos, at, xml, end) { return }
        // Before all text, or after all of it.
        if end && at == 0 { self.items.insert(0, Item::Raw(xml.to_string())) } else { self.items.push(Item::Raw(xml.to_string())) }
    }

    /// The heading's level as the paragraph itself says it; its style may say one too.
    fn outline(&self) -> Option<u8> {
        val_of(&self.ppr, "w:outlineLvl").and_then(|level| level.parse::<u8>().ok()).filter(|level| *level < 9).map(|level| level + 1)
    }

    fn style(&self) -> String {
        val_of(&self.ppr, "w:pStyle").unwrap_or_default()
    }

    /// A paragraph that cannot go when its text does: it ends a section of the document, or holds a picture.
    fn must_stay(&self) -> bool {
        let mut content = self.ppr.contains("<w:sectPr");
        leaves(&self.items, &mut |leaf| {
            if let Leaf::Raw(xml) = leaf { content |= is_content(xml) || xml.starts_with("<m:oMath") }
        });
        content
    }

    /// The run properties of the paragraph's mark, when they are plain formatting.
    fn mark_rpr(&self) -> String {
        let rpr = children(inside(&self.ppr)).into_iter().find(|(name, _)| *name == "w:rPr").map(|(_, piece)| piece.to_string()).unwrap_or_default();
        if ["<w:ins", "<w:del", "<w:moveFrom", "<w:moveTo", "<w:rPrChange"].iter().any(|mark| rpr.contains(mark)) { String::new() } else { rpr }
    }

    /// The properties a new paragraph after this one starts with, and those of its text: the run that says most.
    fn template(&self) -> (String, String) {
        let mut ppr = self.ppr.clone();
        for name in ["w:sectPr", "w:pPrChange", "w:ins", "w:del", "w:moveFrom", "w:moveTo"] { ppr = without(&ppr, name) }
        fn runs<'a>(items: &'a [Item], plain: bool, out: &mut Vec<(bool, &'a Run)>) {
            for item in items {
                match item {
                    Item::Run(run) => out.push((plain, run)),
                    Item::Wrap { items, ins, .. } => runs(items, plain && *ins, out),
                    Item::Raw(_) => {}
                }
            }
        }
        let mut found = vec![];
        runs(&self.items, true, &mut found);
        // A link's formatting is the link's, not the paragraph's.
        let rpr = found.iter().filter(|(_, run)| run.len() > 0).max_by_key(|(plain, run)| (*plain, run.len())).map(|(_, run)| without(&run.rpr, "w:rPrChange")).unwrap_or_else(|| self.mark_rpr());
        (ppr, rpr)
    }

    /// The paragraph saying `new`. What stayed keeps its runs; what went
    /// is left out, or struck when the changes are tracked; what came
    /// stands in the run of the text before it.
    fn edited(&self, new: &str, track: Option<&mut Track>) -> String {
        let old = self.text();
        // The spaces around the text are the file's own: the manuscript holds the text without them.
        let lead = old.len() - old.trim_start().len();
        let trail = old.trim_end().len().max(lead);
        let mut ops = ops(&old[lead..trail], new.trim());
        for op in &mut ops { op.at += lead }
        self.changed(&ops, track)
    }

    fn changed(&self, ops: &[Op], track: Option<&mut Track>) -> String {
        let mut editor = Editor { ops, used: vec![false; ops.len()], pos: 0, track };
        let mut body = String::new();
        editor.items(&self.items, false, &mut body);
        // Text for a paragraph that had none.
        for (n, op) in ops.iter().enumerate() {
            if editor.used[n] || op.ins.is_empty() { continue }
            let run = format!("<w:r>{}{}</w:r>", self.mark_rpr(), text_xml("w:t", &op.ins));
            body.push_str(&match editor.track.as_mut() {
                Some(track) => format!("<w:ins{}>{run}</w:ins>", track.attrs()),
                None => run,
            });
        }
        format!("{}{}{body}</w:p>", self.open, self.ppr)
    }

    /// The paragraph taken out, as a tracked change: its text struck, and its mark with it.
    fn struck(&self, track: &mut Track) -> String {
        let whole = [Op { at: 0, del: self.text().len(), ins: String::new() }];
        let para = Para { open: self.open.clone(), ppr: marked(&self.ppr, &format!("<w:del{}/>", track.attrs())), items: self.items.clone() };
        para.changed(&whole, Some(track))
    }
}

/// A paragraph's properties with its mark (the break after it) noted as put in or taken out.
fn marked(ppr: &str, mark: &str) -> String {
    if !has_content(ppr) { return format!("<w:pPr><w:rPr>{mark}</w:rPr></w:pPr>") }
    let kids = children(inside(ppr));
    let mut out = open_tag(ppr).to_string();
    let mut placed = false;
    for (name, piece) in &kids {
        if *name == "w:rPr" && !placed {
            // Put in comes before taken out, both before the formatting.
            let inner = inside(piece);
            let first = children(inner).first().map_or(0, |(name, piece)| if *name == "w:ins" && mark.starts_with("<w:del") { piece.len() } else { 0 });
            out.push_str(&format!("<w:rPr>{}{mark}{}</w:rPr>", &inner[..first], &inner[first..]));
            placed = true;
            continue;
        }
        if (*name == "w:sectPr" || *name == "w:pPrChange") && !placed {
            out.push_str(&format!("<w:rPr>{mark}</w:rPr>"));
            placed = true;
        }
        out.push_str(piece);
    }
    if !placed { out.push_str(&format!("<w:rPr>{mark}</w:rPr>")) }
    out.push_str("</w:pPr>");
    out
}

/// A paragraph that was not in the file.
fn new_para(ppr: &str, rpr: &str, text: &str, track: Option<&mut Track>) -> String {
    let run = format!("<w:r>{rpr}{}</w:r>", text_xml("w:t", text));
    match track {
        Some(track) => format!("<w:p>{}<w:ins{}>{run}</w:ins></w:p>", marked(ppr, &format!("<w:ins{}/>", track.attrs())), track.attrs()),
        None => format!("<w:p>{ppr}{run}</w:p>"),
    }
}

// ---- changing a paragraph's text

/// One change to a text: at byte `at`, `del` bytes go and `ins` comes.
#[derive(Clone, Debug, PartialEq)]
struct Op {
    at: usize,
    del: usize,
    ins: String,
}

/// The most words of a paragraph compared one by one; a longer one is replaced between what its two versions share at the start and the end.
const DIFF_TOKENS: usize = 2000;

/// A stretch that both versions share between two changes is not kept apart from them when it is this short (characters that are not spaces).
const TOO_SHORT: usize = 3;

/// What turns `old` into `new`, word by word.
fn ops(old: &str, new: &str) -> Vec<Op> {
    if old == new { return vec![] }
    let (a, b) = (diff_tokens(old), diff_tokens(new));
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (mid_a, mid_b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    // Stretches of the middle: shared or not, and how many words of each version.
    let mut runs: Vec<(bool, usize, usize)> = vec![];
    let push = |runs: &mut Vec<(bool, usize, usize)>, same: bool, x: usize, y: usize| match runs.last_mut() {
        Some((last, p, q)) if *last == same => {
            *p += x;
            *q += y;
        }
        _ => runs.push((same, x, y)),
    };
    if !mid_a.is_empty() && !mid_b.is_empty() && mid_a.len() <= DIFF_TOKENS && mid_b.len() <= DIFF_TOKENS {
        let width = mid_b.len() + 1;
        let mut table = vec![0u16; (mid_a.len() + 1) * width];
        for i in (0..mid_a.len()).rev() {
            for j in (0..mid_b.len()).rev() {
                table[i * width + j] = if mid_a[i] == mid_b[j] { table[(i + 1) * width + j + 1] + 1 } else { table[(i + 1) * width + j].max(table[i * width + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < mid_a.len() || j < mid_b.len() {
            if i < mid_a.len() && j < mid_b.len() && mid_a[i] == mid_b[j] {
                push(&mut runs, true, 1, 1);
                (i, j) = (i + 1, j + 1);
            } else if i < mid_a.len() && (j == mid_b.len() || table[(i + 1) * width + j] >= table[i * width + j + 1]) {
                push(&mut runs, false, 1, 0);
                i += 1;
            } else {
                push(&mut runs, false, 0, 1);
                j += 1;
            }
        }
    } else {
        push(&mut runs, false, mid_a.len(), mid_b.len());
    }
    // A word or two that both versions happen to share between two changes: one change reads better than three.
    let mut at = head;
    let mut tidy: Vec<(bool, usize, usize)> = vec![];
    for (n, &(same, x, y)) in runs.iter().enumerate() {
        let between = n > 0 && n + 1 < runs.len();
        let short = same && between && a[at..at + x].iter().flat_map(|word| word.chars()).filter(|c| !c.is_whitespace()).count() <= TOO_SHORT;
        push(&mut tidy, same && !short, x, y);
        at += x;
    }
    let bytes = |words: &[&str]| words.iter().map(|word| word.len()).sum::<usize>();
    let (mut out, mut i, mut j) = (vec![], head, head);
    let mut offset = bytes(&a[..head]);
    for (same, x, y) in tidy {
        let gone = bytes(&a[i..i + x]);
        if !same { out.push(Op { at: offset, del: gone, ins: b[j..j + y].concat() }) }
        offset += gone;
        (i, j) = (i + x, j + y);
    }
    out
}

/// Who makes the tracked changes, and the number of the next one.
struct Track {
    author: String,
    date: String,
    next: u32,
}

impl Track {
    fn attrs(&mut self) -> String {
        self.next += 1;
        format!(" w:id=\"{}\" w:author=\"{}\" w:date=\"{}\"", self.next - 1, escape(&self.author), escape(&self.date))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Keep,
    Del,
    Ins,
}

/// Writes a paragraph's items with changes made to its text.
struct Editor<'a, 'b> {
    ops: &'a [Op],
    /// Whether what each change puts in was written.
    used: Vec<bool>,
    /// How far into the old text the writing is, in bytes.
    pos: usize,
    track: Option<&'b mut Track>,
}

impl Editor<'_, '_> {
    /// The pieces a stretch of the old text becomes: what stays, what goes,
    /// and what a change that ends in it puts in.
    fn cut(&mut self, text: &str) -> Vec<(Kind, String)> {
        let (from, to) = (self.pos, self.pos + text.len());
        self.pos = to;
        let (mut out, mut at) = (vec![], from);
        for (n, op) in self.ops.iter().enumerate() {
            let (start, end) = (op.at, op.at + op.del);
            if start > to { break }
            if end < from { continue }
            let (a, b) = (start.clamp(from, to), end.clamp(from, to));
            if a > at { out.push((Kind::Keep, text[at - from..a - from].to_string())) }
            if b > a { out.push((Kind::Del, text[a - from..b - from].to_string())) }
            at = at.max(b);
            // What comes stands after the last character it replaces, or after the one before it; at the very start, before the first.
            if !op.ins.is_empty() && !self.used[n] && ((from < end && end <= to) || end == 0) {
                out.push((Kind::Ins, op.ins.clone()));
                self.used[n] = true;
            }
        }
        if to > at { out.push((Kind::Keep, text[at - from..].to_string())) }
        out
    }

    fn run(&mut self, run: &Run, in_ins: bool, out: &mut String) {
        let tracked = self.track.is_some();
        // What the run becomes, in order: stretches that stay, go or come, each made of text and of things written as they were.
        let mut pieces: Vec<(Kind, Vec<Result<String, String>>)> = vec![];
        let mut push = |kind: Kind, piece: Result<String, String>| {
            // Not tracked, what goes is gone and what comes is text like the rest; so is text put into another's insertion.
            let kind = match kind {
                Kind::Del if !tracked => return,
                Kind::Ins if !tracked || in_ins => Kind::Keep,
                kind => kind,
            };
            if pieces.last().is_none_or(|(last, _)| *last != kind) { pieces.push((kind, vec![])) }
            let Some((_, list)) = pieces.last_mut() else { return };
            match (list.last_mut(), piece) {
                (Some(Ok(text)), Ok(more)) => text.push_str(&more),
                (_, piece) => list.push(piece),
            }
        };
        for part in &run.parts {
            match part {
                Part::Text(text) => {
                    for (kind, piece) in self.cut(text) { push(kind, Ok(piece)) }
                }
                Part::Shown(xml, shown) => {
                    for (kind, piece) in self.cut(shown) { push(kind, if kind == Kind::Ins { Ok(piece) } else { Err(xml.clone()) }) }
                }
                Part::Raw(xml) => {
                    let gone = is_content(xml) && self.ops.iter().any(|op| op.at < self.pos && self.pos < op.at + op.del);
                    push(if gone { Kind::Del } else { Kind::Keep }, Err(xml.clone()));
                }
            }
        }
        for (kind, list) in pieces {
            let inner: String = list.into_iter().map(|piece| match piece {
                Ok(text) => text_xml(if kind == Kind::Del { "w:delText" } else { "w:t" }, &text),
                Err(xml) => xml,
            }).collect();
            let run = format!("{}{}{inner}</w:r>", run.open, run.rpr);
            match (kind, self.track.as_mut()) {
                (Kind::Del, Some(track)) => out.push_str(&format!("<w:del{}>{run}</w:del>", track.attrs())),
                (Kind::Ins, Some(track)) => out.push_str(&format!("<w:ins{}>{run}</w:ins>", track.attrs())),
                _ => out.push_str(&run),
            }
        }
    }

    fn items(&mut self, items: &[Item], in_ins: bool, out: &mut String) {
        for item in items {
            match item {
                Item::Raw(xml) => out.push_str(xml),
                Item::Run(run) => self.run(run, in_ins, out),
                Item::Wrap { open, close, items, ins } => {
                    let mut inner = String::new();
                    self.items(items, in_ins || *ins, &mut inner);
                    // A link whose words all went goes with them.
                    if !inner.is_empty() || items.is_empty() {
                        out.push_str(open);
                        out.push_str(&inner);
                        out.push_str(close);
                    }
                }
            }
        }
    }
}

// ---- matching the file's paragraphs with the manuscript's

/// What becomes of a paragraph of the file (the first number) or of the manuscript (the second, or the only one of `Added`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Same(usize, usize),
    Changed(usize, usize),
    Removed(usize),
    Added(usize),
}

/// Two paragraphs this much alike (`likeness`) are one paragraph that was changed.
const ALIKE: f32 = 0.45;

/// How much two texts share, from 0 to 1: by their pairs of neighbouring characters, which works for words and for Chinese alike.
fn likeness(a: &str, b: &str) -> f32 {
    let pairs = |text: &str| {
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        let mut pairs: Vec<(char, char)> = chars.windows(2).map(|pair| (pair[0], pair[1])).collect();
        pairs.sort_unstable();
        pairs
    };
    let (x, y) = (pairs(a), pairs(b));
    if x.is_empty() || y.is_empty() { return 0. }
    let (mut i, mut j, mut shared) = (0, 0, 0);
    while i < x.len() && j < y.len() {
        match x[i].cmp(&y[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                shared += 1;
                (i, j) = (i + 1, j + 1);
            }
        }
    }
    2. * shared as f32 / (x.len() + y.len()) as f32
}

/// The pairs that go together, in order, when each pair is worth `worth(i, j)` (nothing: not a pair) and the worth is to be the most.
fn best_pairs(rows: usize, columns: usize, worth: impl Fn(usize, usize) -> Option<f32>) -> Vec<(usize, usize)> {
    let width = columns + 1;
    let mut table = vec![0f32; (rows + 1) * width];
    for i in (0..rows).rev() {
        for j in (0..columns).rev() {
            let skip = table[(i + 1) * width + j].max(table[i * width + j + 1]);
            table[i * width + j] = worth(i, j).map_or(skip, |worth| skip.max(worth + table[(i + 1) * width + j + 1]));
        }
    }
    let (mut pairs, mut i, mut j) = (vec![], 0, 0);
    while i < rows && j < columns {
        let here = table[i * width + j];
        if worth(i, j).is_some_and(|worth| here == worth + table[(i + 1) * width + j + 1]) {
            pairs.push((i, j));
            (i, j) = (i + 1, j + 1);
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// The paragraphs between two that say the same on both sides: those alike
/// are one paragraph changed, and what is left over is changed in place
/// when as many came as went, else taken out and put in.
fn gap(old: &[&str], new: &[&str], o: usize, n: usize, steps: &mut Vec<Step>) {
    let pairs = if old.is_empty() || new.is_empty() || old.len() * new.len() > 40_000 { vec![] } else { best_pairs(old.len(), new.len(), |i, j| Some(likeness(old[i], new[j])).filter(|alike| *alike >= ALIKE)) };
    let (mut i, mut j) = (0, 0);
    for (x, y) in pairs.into_iter().chain([(old.len(), new.len())]) {
        if x - i == y - j {
            steps.extend((0..x - i).map(|k| Step::Changed(o + i + k, n + j + k)));
        } else {
            steps.extend((i..x).map(|k| Step::Removed(o + k)));
            steps.extend((j..y).map(|k| Step::Added(n + k)));
        }
        if x < old.len() { steps.push(Step::Changed(o + x, n + y)) }
        (i, j) = (x + 1, y + 1);
    }
}

fn align(old: &[&str], new: &[&str]) -> Vec<Step> {
    let head = old.iter().zip(new).take_while(|(x, y)| x == y).count();
    let tail = old[head..].iter().rev().zip(new[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a, b) = (&old[head..old.len() - tail], &new[head..new.len() - tail]);
    let mut steps: Vec<Step> = (0..head).map(|n| Step::Same(n, n)).collect();
    let same = if a.len() * b.len() > 4_000_000 { vec![] } else { best_pairs(a.len(), b.len(), |i, j| (a[i] == b[j]).then_some(1.)) };
    let (mut i, mut j) = (0, 0);
    for (x, y) in same.into_iter().chain([(a.len(), b.len())]) {
        gap(&a[i..x], &b[j..y], head + i, head + j, &mut steps);
        if x < a.len() { steps.push(Step::Same(head + x, head + y)) }
        (i, j) = (x + 1, y + 1);
    }
    steps.extend((0..tail).map(|n| Step::Same(old.len() - tail + n, new.len() - tail + n)));
    steps
}

// ---- the document

enum Block {
    /// A paragraph of the body: as written, and read.
    Para(String, Para),
    /// A table, a content control, the section's properties: kept as written.
    Other(String),
}

struct Document {
    /// Up to the body's first block, and from its closing tag on.
    head: String,
    blocks: Vec<Block>,
    tail: String,
}

impl Document {
    fn parse(xml: &str) -> Result<Document> {
        let unreadable = || anyhow!("读不懂这个 Word 文件的正文。");
        let body = xml.find("<w:body").ok_or_else(unreadable)?;
        let Some((Node::Open(..), start)) = node(xml, body) else { return Err(unreadable()) };
        let end = xml.rfind("</w:body>").filter(|end| *end >= start).ok_or_else(unreadable)?;
        let blocks = children(&xml[start..end]).into_iter().map(|(name, piece)| if name == "w:p" { Block::Para(piece.to_string(), Para::parse(piece)) } else { Block::Other(piece.to_string()) }).collect();
        Ok(Document { head: xml[..start].to_string(), blocks, tail: xml[end..].to_string() })
    }

    /// The paragraphs a manuscript is made of, by their place among the blocks: those that say something and are not a table of contents.
    fn readable(&self, styles: &Styles) -> Vec<usize> {
        self.blocks.iter().enumerate().filter(|(_, block)| matches!(block, Block::Para(_, para) if !para.text().trim().is_empty() && !styles.named(&para.style(), "toc"))).map(|(n, _)| n).collect()
    }

    fn para(&self, block: usize) -> &Para {
        match &self.blocks[block] {
            Block::Para(_, para) => para,
            Block::Other(_) => unreachable!("only paragraphs are asked for"),
        }
    }
}

/// The paragraph styles of a document, as far as they say what a paragraph is.
#[derive(Default)]
struct Styles {
    /// By id: the name in lower case, the style it is based on, its outline level.
    list: BTreeMap<String, (String, String, Option<u8>)>,
}

impl Styles {
    fn read(xml: &str) -> Styles {
        let (mut list, mut at) = (BTreeMap::new(), 0);
        while let Some(found) = xml[at..].find("<w:style ") {
            let start = at + found;
            at = element_end(xml, start).max(start + 1);
            let style = &xml[start..at];
            let Some(id) = attr(open_tag(style), "w:styleId") else { continue };
            let level = val_of(style, "w:outlineLvl").and_then(|level| level.parse::<u8>().ok()).filter(|level| *level < 9).map(|level| level + 1);
            list.insert(id, (val_of(style, "w:name").unwrap_or_default().to_lowercase(), val_of(style, "w:basedOn").unwrap_or_default(), level));
        }
        Styles { list }
    }

    /// The style and those it is based on, nearest first.
    fn chain<'a>(&'a self, id: &str) -> impl Iterator<Item = &'a (String, String, Option<u8>)> {
        let mut next = self.list.get(id);
        let mut left = 12;
        std::iter::from_fn(move || {
            let style = next.filter(|_| left > 0)?;
            left -= 1;
            next = self.list.get(&style.1);
            Some(style)
        })
    }

    /// The level of the heading a style makes: `heading 2` by name (Word keeps the English name in every language), or by its outline level.
    fn level(&self, id: &str) -> Option<u8> {
        self.chain(id).find_map(|(name, _, outline)| {
            let numbered = name.strip_prefix("heading ").or(name.strip_prefix("标题 ")).and_then(|n| n.trim().parse::<u8>().ok()).filter(|n| (1..=9).contains(n));
            numbered.or(*outline)
        })
    }

    /// Whether the style, or one it is based on, has a name that starts so.
    fn named(&self, id: &str, start: &str) -> bool {
        self.chain(id).any(|(name, _, _)| name == start || name.strip_prefix(start).is_some_and(|rest| rest.starts_with(' ')))
    }
}

// ---- reading

/// A paragraph of a Word file that says something.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadPara {
    pub text: String,
    /// The level of the heading it is, from 1.
    pub level: Option<u8>,
    /// It is the document's title.
    pub title: bool,
    /// All its words are set in bold.
    pub bold: bool,
}

/// A comment a Word file came with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadComment {
    pub author: String,
    /// As the file says it: `2026-11-12T09:00:00Z`.
    pub date: String,
    pub body: String,
    /// The paragraph it is about, by its place in `Read::paragraphs`; none for one in a table or on a picture.
    pub paragraph: Option<usize>,
    /// The words it is about, within that paragraph; empty for one about a place and not about words.
    pub quote: String,
}

/// What a Word file holds for a manuscript.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Read {
    pub paragraphs: Vec<ReadPara>,
    pub comments: Vec<ReadComment>,
    /// Tables and pictures: not in the manuscript, kept in the file.
    pub tables: usize,
    pub pictures: usize,
    /// The file has changes that were tracked and not accepted yet: it is read as if they were.
    pub tracked: bool,
}

fn id_of(xml: &str) -> Option<String> {
    attr(open_tag(xml), "w:id")
}

/// The part with the comments, when the document has one.
fn comments_part(zip: &Zip, main: &str) -> Result<Option<String>> {
    let rels = zip.text(&rels_of(main))?;
    Ok(relationships(&rels).into_iter().find(|(kind, _)| kind.ends_with("/comments")).map(|(_, target)| beside(main, &target)).filter(|part| zip.has(part)))
}

fn styles_of(zip: &Zip, main: &str) -> Result<Styles> {
    let rels = zip.text(&rels_of(main))?;
    let part = relationships(&rels).into_iter().find(|(kind, _)| kind.ends_with("/styles")).map(|(_, target)| beside(main, &target)).unwrap_or_else(|| beside(main, "styles.xml"));
    Ok(Styles::read(&zip.text(&part)?))
}

pub fn read(bytes: &[u8]) -> Result<Read> {
    let zip = Zip::read(bytes)?;
    let main = zip.main()?;
    let xml = zip.text(&main)?;
    let styles = styles_of(&zip, &main)?;
    let doc = Document::parse(&xml)?;
    let readable = doc.readable(&styles);
    let paragraphs: Vec<ReadPara> = readable
        .iter()
        .map(|&block| {
            let para = doc.para(block);
            let style = para.style();
            ReadPara { text: para.text().trim().to_string(), level: para.outline().or_else(|| styles.level(&style)), title: styles.named(&style, "title"), bold: para.bold() }
        })
        .collect();
    // Where each comment's range starts and ends: the words between, as far as they are in the paragraph it starts in.
    let mut anchors: BTreeMap<String, (Option<usize>, String)> = BTreeMap::new();
    let mut open: BTreeMap<String, (Option<usize>, String)> = BTreeMap::new();
    for (n, block) in doc.blocks.iter().enumerate() {
        let Block::Para(_, para) = block else { continue };
        let place = readable.iter().position(|block| *block == n);
        let mut started: Vec<(String, usize)> = vec![];
        let mut text = String::new();
        leaves(&para.items, &mut |leaf| match leaf {
            Leaf::Text(piece) => text.push_str(piece),
            Leaf::Raw(xml) if xml.starts_with("<w:commentRangeStart") => {
                if let Some(id) = id_of(xml) { started.push((id, text.len())) }
            }
            Leaf::Raw(xml) if xml.starts_with("<w:commentRangeEnd") => {
                let Some(id) = id_of(xml) else { return };
                if let Some(at) = started.iter().position(|(other, _)| *other == id) {
                    let (_, from) = started.remove(at);
                    anchors.insert(id, (place, text[from..].to_string()));
                } else if let Some(found) = open.remove(&id) {
                    anchors.insert(id, found);
                }
            }
            Leaf::Raw(xml) if xml.starts_with("<w:commentReference") => {
                if let Some(id) = id_of(xml) { anchors.entry(id).or_insert((place, String::new())); }
            }
            Leaf::Raw(_) => {}
        });
        // A range that goes on into the next paragraph is about what it covers of this one.
        for (id, from) in started { open.insert(id, (place, text[from..].to_string())); }
    }
    let mut comments = vec![];
    if let Some(part) = comments_part(&zip, &main)? {
        let xml = zip.text(&part)?;
        let mut at = 0;
        while let Some(found) = xml[at..].find("<w:comment ") {
            let start = at + found;
            at = element_end(&xml, start).max(start + 1);
            let comment = &xml[start..at];
            let tag = open_tag(comment);
            let body: Vec<String> = children(inside(comment)).into_iter().filter(|(name, _)| *name == "w:p").map(|(_, piece)| Para::parse(piece).text().trim().to_string()).filter(|line| !line.is_empty()).collect();
            let (paragraph, quote) = attr(tag, "w:id").and_then(|id| anchors.get(&id).cloned()).unwrap_or_default();
            let quote = quote.trim().to_string();
            comments.push(ReadComment { author: attr(tag, "w:author").unwrap_or_default(), date: attr(tag, "w:date").unwrap_or_default(), body: body.join("\n"), paragraph, quote });
        }
    }
    let body: String = doc.blocks.iter().map(|block| match block {
        Block::Para(xml, _) | Block::Other(xml) => xml.as_str(),
    }).collect();
    Ok(Read { paragraphs, comments, tables: doc.blocks.iter().filter(|block| matches!(block, Block::Other(xml) if xml.starts_with("<w:tbl"))).count(), pictures: body.matches("<w:drawing").count() + body.matches("<w:pict>").count() + body.matches("<w:pict ").count(), tracked: body.contains("<w:ins ") || body.contains("<w:del ") })
}

// ---- writing again

/// What a paragraph of the manuscript is in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Title,
    Heading,
    Body,
}

/// A comment to put on a paragraph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Note {
    /// The words it is about, in bytes of the paragraph's text; empty for the whole paragraph.
    pub range: Range<usize>,
    pub author: String,
    /// `2026-11-12T09:00:00Z`.
    pub date: String,
    pub body: String,
}

/// A paragraph of the manuscript as it stands now.
#[derive(Clone, Debug)]
pub struct Wanted {
    pub text: String,
    pub role: Role,
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Write the changes as Word's revisions, to be accepted or rejected there.
    pub track: bool,
    /// Whose changes they are, and when they were made.
    pub author: String,
    pub date: String,
}

/// A Word file written again, and what was done to it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rewritten {
    pub bytes: Vec<u8>,
    /// Paragraphs left as they were, changed, put in and taken out.
    pub kept: usize,
    pub changed: usize,
    pub added: usize,
    pub removed: usize,
    /// Comments put in.
    pub comments: usize,
}

/// The comments to add to a document, and the number of the next one.
struct Comments {
    next: u32,
    added: String,
    count: usize,
}

impl Comments {
    fn add(&mut self, note: &Note) -> u32 {
        let id = self.next;
        self.next += 1;
        self.count += 1;
        let initials: String = note.author.chars().filter(|c| c.is_alphanumeric()).take(1).flat_map(char::to_uppercase).collect();
        let lines: String = note.body.lines().map(str::trim).filter(|line| !line.is_empty()).map(|line| format!("<w:p><w:r>{}</w:r></w:p>", text_xml("w:t", line))).collect();
        let lines = if lines.is_empty() { "<w:p/>".to_string() } else { lines };
        self.added.push_str(&format!("<w:comment w:id=\"{id}\" w:author=\"{}\" w:date=\"{}\" w:initials=\"{}\">{lines}</w:comment>", escape(&note.author), escape(&note.date), escape(&initials)));
        id
    }
}

/// The largest number given as `w:id` in `xml` to an element whose tag starts with one of `opens`; every `w:id` when there are none.
fn largest_id(xml: &str, opens: &[&str]) -> u32 {
    let (mut largest, mut at) = (0, 0);
    while let Some(found) = xml[at..].find(" w:id=\"") {
        let start = at + found + 7;
        at = start;
        let tag_start = xml[..start].rfind('<').unwrap_or(0);
        if !opens.is_empty() && !opens.iter().any(|open| xml[tag_start..].starts_with(open)) { continue }
        let digits: String = xml[start..].chars().take_while(char::is_ascii_digit).collect();
        largest = largest.max(digits.parse::<u32>().unwrap_or(0));
    }
    largest
}

/// A paragraph with its comments marked in it. `xml` says the paragraph's text with the file's own spaces around it.
fn noted(xml: String, notes: &[Note], comments: &mut Comments) -> String {
    if notes.is_empty() { return xml }
    let mut para = Para::parse(&xml);
    let text = para.text();
    let lead = text.len() - text.trim_start().len();
    let length = text.trim().len();
    for note in notes {
        let (start, end) = (text.floor_char_boundary(lead + note.range.start.min(length)), text.floor_char_boundary(lead + note.range.end.min(length)));
        let (start, end) = if end > start { (start, end) } else { (lead, lead + length) };
        let id = comments.add(note);
        para.insert(start, &format!("<w:commentRangeStart w:id=\"{id}\"/>"), false);
        para.insert(end, &format!("<w:commentRangeEnd w:id=\"{id}\"/><w:r><w:commentReference w:id=\"{id}\"/></w:r>"), true);
    }
    para.xml()
}

/// The Word file `source` saying what `wanted` says, paragraph by
/// paragraph, with the comments of `wanted`. See the top of this file for
/// what is kept and how.
pub fn rewrite(source: &[u8], wanted: &[Wanted], options: &Options) -> Result<Rewritten> {
    let mut zip = Zip::read(source)?;
    let main = zip.main()?;
    let xml = zip.text(&main)?;
    let styles = styles_of(&zip, &main)?;
    let doc = Document::parse(&xml)?;
    let readable = doc.readable(&styles);
    let old: Vec<String> = readable.iter().map(|&block| doc.para(block).text().trim().to_string()).collect();
    let new: Vec<&str> = wanted.iter().map(|wanted| wanted.text.trim()).collect();
    let steps = align(&old.iter().map(String::as_str).collect::<Vec<_>>(), &new);

    let comments_part = comments_part(&zip, &main)?;
    let comments_xml = match &comments_part {
        Some(part) => zip.text(part)?,
        None => String::new(),
    };
    let mut comments = Comments { next: largest_id(&comments_xml, &["<w:comment "]).max(largest_id(&xml, &["<w:commentRangeStart", "<w:commentReference"])) + 1, added: String::new(), count: 0 };
    let mut track = options.track.then(|| Track { author: options.author.clone(), date: options.date.clone(), next: largest_id(&xml, &[]) + 1 });

    // What each paragraph of the file is, a heading or text: by its style, and by what the manuscript takes it for. A new paragraph is set like the nearest of its kind.
    let mut heading: Vec<bool> = readable.iter().map(|&block| doc.para(block).outline().is_some() || styles.level(&doc.para(block).style()).is_some() || styles.named(&doc.para(block).style(), "title")).collect();
    for step in &steps {
        if let Step::Same(o, w) | Step::Changed(o, w) = *step { heading[o] = wanted[w].role != Role::Body }
    }
    let template = |w: usize, after: Option<usize>| -> (String, String) {
        let wants = wanted[w].role != Role::Body;
        let before = after.map_or(0, |o| o + 1);
        match (0..before).rev().chain(before..heading.len()).find(|&o| heading[o] == wants) {
            Some(o) => doc.para(readable[o]).template(),
            None if wants => ("<w:pPr><w:keepNext/></w:pPr>".into(), "<w:rPr><w:b/></w:rPr>".into()),
            None => Default::default(),
        }
    };

    let mut out: Vec<Vec<String>> = doc.blocks.iter().map(|block| match block {
        Block::Para(xml, _) | Block::Other(xml) => vec![xml.clone()],
    }).collect();
    let mut front: Vec<String> = vec![];
    // The paragraph of the file last gone past: among the blocks, and among the paragraphs.
    let (mut last, mut last_old): (Option<usize>, Option<usize>) = (None, None);
    let mut done = Rewritten::default();
    for step in &steps {
        match *step {
            Step::Same(o, w) => {
                let block = readable[o];
                let written = std::mem::take(&mut out[block][0]);
                out[block] = vec![noted(written, &wanted[w].notes, &mut comments)];
                (last, last_old, done.kept) = (Some(block), Some(o), done.kept + 1);
            }
            Step::Changed(o, w) => {
                let block = readable[o];
                out[block] = vec![noted(doc.para(block).edited(&wanted[w].text, track.as_mut()), &wanted[w].notes, &mut comments)];
                (last, last_old, done.changed) = (Some(block), Some(o), done.changed + 1);
            }
            Step::Removed(o) => {
                let block = readable[o];
                let para = doc.para(block);
                out[block] = match track.as_mut() {
                    Some(track) => vec![para.struck(track)],
                    None if para.must_stay() => vec![para.edited("", None)],
                    None => vec![],
                };
                (last, last_old, done.removed) = (Some(block), Some(o), done.removed + 1);
            }
            Step::Added(w) => {
                let (ppr, rpr) = template(w, last_old);
                let para = noted(new_para(&ppr, &rpr, wanted[w].text.trim(), track.as_mut()), &wanted[w].notes, &mut comments);
                match last {
                    Some(block) => out[block].push(para),
                    None => front.push(para),
                }
                done.added += 1;
            }
        }
    }
    // What comes before every paragraph of the file stands before the first of them; in a file without any, before the page's properties at the end.
    let front_at = readable.first().copied().or_else(|| doc.blocks.iter().rposition(|block| matches!(block, Block::Other(xml) if xml.starts_with("<w:sectPr")))).unwrap_or(doc.blocks.len());
    let mut body = String::with_capacity(xml.len() + 1024);
    for (n, pieces) in out.iter().enumerate() {
        if n == front_at { front.iter().for_each(|para| body.push_str(para)) }
        pieces.iter().for_each(|piece| body.push_str(piece));
    }
    if front_at >= out.len() { front.iter().for_each(|para| body.push_str(para)) }
    zip.set(&main, format!("{}{body}{}", doc.head, doc.tail).as_bytes());

    if comments.count > 0 {
        // The names the document itself uses: a file in the strict form of the format has others.
        let strict = xml.contains("http://purl.oclc.org/ooxml/wordprocessingml/main");
        match comments_part {
            Some(part) => {
                let whole = match comments_xml.rfind("</w:comments>") {
                    Some(end) => format!("{}{}{}", &comments_xml[..end], comments.added, &comments_xml[end..]),
                    // A part with no comment yet is one empty element.
                    None => match comments_xml.rfind("/>") {
                        Some(end) => format!("{}>{}</w:comments>", &comments_xml[..end], comments.added),
                        None => bail!("读不懂这个 Word 文件的批注。"),
                    },
                };
                zip.set(&part, whole.as_bytes());
            }
            None => {
                let part = beside(&main, "comments.xml");
                let space = if strict { "http://purl.oclc.org/ooxml/wordprocessingml/main" } else { "http://schemas.openxmlformats.org/wordprocessingml/2006/main" };
                zip.set(&part, format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:comments xmlns:w=\"{space}\">{}</w:comments>", comments.added).as_bytes());
                let rels_part = rels_of(&main);
                let rels = zip.text(&rels_part)?;
                let kind = if strict { "http://purl.oclc.org/ooxml/officeDocument/relationships/comments" } else { "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" };
                let mut id = 1;
                while rels.contains(&format!("\"rIdNote{id}\"")) { id += 1 }
                let relationship = format!("<Relationship Id=\"rIdNote{id}\" Type=\"{kind}\" Target=\"comments.xml\"/>");
                let rels = match rels.rfind("</Relationships>") {
                    Some(end) => format!("{}{relationship}{}", &rels[..end], &rels[end..]),
                    None => format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{relationship}</Relationships>"),
                };
                zip.set(&rels_part, rels.as_bytes());
                let types = zip.text("[Content_Types].xml")?;
                if !types.contains(&format!("\"/{part}\"")) {
                    let end = types.rfind("</Types>").ok_or_else(|| anyhow!(NOT_DOCX))?;
                    let kind = "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml";
                    zip.set("[Content_Types].xml", format!("{}<Override PartName=\"/{part}\" ContentType=\"{kind}\"/>{}", &types[..end], &types[end..]).as_bytes());
                }
            }
        }
    }
    done.comments = comments.count;
    done.bytes = zip.write();
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

    /// A Word file with this body, and these comments when there are any.
    fn file(body: &str, comments: &str) -> Vec<u8> {
        let mut zip = Zip { entries: vec![] };
        let comment_type = if comments.is_empty() { "" } else { "<Override PartName=\"/word/comments.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml\"/>" };
        zip.set("[Content_Types].xml", format!("<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>{comment_type}</Types>").as_bytes());
        zip.set("_rels/.rels", b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>");
        let comment_rel = if comments.is_empty() { "" } else { "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments\" Target=\"comments.xml\"/>" };
        zip.set("word/_rels/document.xml.rels", format!("<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>{comment_rel}</Relationships>").as_bytes());
        zip.set("word/styles.xml", format!("<?xml version=\"1.0\"?><w:styles {W}><w:style w:type=\"paragraph\" w:styleId=\"1\"><w:name w:val=\"heading 1\"/></w:style><w:style w:type=\"paragraph\" w:styleId=\"Sub\"><w:name w:val=\"Sub\"/><w:basedOn w:val=\"1\"/><w:pPr><w:outlineLvl w:val=\"1\"/></w:pPr></w:style><w:style w:type=\"paragraph\" w:styleId=\"a3\"><w:name w:val=\"Title\"/></w:style><w:style w:type=\"paragraph\" w:styleId=\"TOC1\"><w:name w:val=\"toc 1\"/></w:style></w:styles>").as_bytes());
        zip.set("word/document.xml", format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<w:document {W}><w:body>{body}<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr></w:body></w:document>").as_bytes());
        if !comments.is_empty() { zip.set("word/comments.xml", format!("<?xml version=\"1.0\"?><w:comments {W}>{comments}</w:comments>").as_bytes()) }
        zip.write()
    }

    fn body_of(bytes: &[u8]) -> String {
        let xml = Zip::read(bytes).unwrap().text("word/document.xml").unwrap();
        let start = xml.find("<w:body>").unwrap() + 8;
        xml[start..xml.rfind("<w:sectPr>").unwrap()].to_string()
    }

    fn body(text: &str) -> Wanted {
        Wanted { text: text.into(), role: Role::Body, notes: vec![] }
    }

    fn texts(bytes: &[u8]) -> Vec<String> {
        read(bytes).unwrap().paragraphs.into_iter().map(|para| para.text).collect()
    }

    const BODY: &str = "<w:p><w:pPr><w:pStyle w:val=\"a3\"/></w:pPr><w:r><w:t>Roots &amp; shoots</w:t></w:r></w:p>\
<w:p><w:pPr><w:pStyle w:val=\"TOC1\"/></w:pPr><w:r><w:t>Introduction 1</w:t></w:r></w:p>\
<w:p><w:pPr><w:pStyle w:val=\"1\"/></w:pPr><w:r><w:t>Introduction</w:t></w:r></w:p>\
<w:p><w:pPr><w:ind w:firstLine=\"420\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Roots grow </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>down</w:t></w:r><w:r><w:t xml:space=\"preserve\"> into the </w:t></w:r><w:commentRangeStart w:id=\"4\"/><w:r><w:t>soil</w:t></w:r><w:commentRangeEnd w:id=\"4\"/><w:r><w:commentReference w:id=\"4\"/></w:r><w:r><w:t>.</w:t></w:r></w:p>\
<w:p/>\
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
<w:p><w:pPr><w:pStyle w:val=\"Sub\"/></w:pPr><w:r><w:t>Hairs</w:t></w:r></w:p>\
<w:p><w:r><w:t>Root hairs take up </w:t></w:r><w:ins w:id=\"9\" w:author=\"Li\" w:date=\"2026-01-01T00:00:00Z\"><w:r><w:t>water</w:t></w:r></w:ins><w:del w:id=\"10\" w:author=\"Li\" w:date=\"2026-01-01T00:00:00Z\"><w:r><w:delText>nothing</w:delText></w:r></w:del><w:r><w:tab/><w:t>(Fig. 1)</w:t><w:footnoteReference w:id=\"2\"/><w:t>.</w:t></w:r></w:p>";

    const COMMENT: &str = "<w:comment w:id=\"4\" w:author=\"Li Hua\" w:date=\"2026-01-02T03:04:05Z\" w:initials=\"L\"><w:p><w:r><w:annotationRef/></w:r><w:r><w:t>Which soil?</w:t></w:r></w:p><w:p><w:r><w:t>Say more.</w:t></w:r></w:p></w:comment>";

    #[test]
    fn an_archive_is_read_and_written_with_its_entries_as_they_were() {
        let mut zip = Zip { entries: vec![] };
        zip.set("a.xml", "<a>啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊</a>".as_bytes());
        zip.set("b", b"x");
        let bytes = zip.write();
        let read = Zip::read(&bytes).unwrap();
        assert_eq!(read.names().collect::<Vec<_>>(), ["a.xml", "b"]);
        assert_eq!(read.entries.iter().map(|entry| entry.method).collect::<Vec<_>>(), [8, 0], "what packing does not make smaller is stored");
        assert_eq!(read.text("a.xml").unwrap(), "<a>啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊啊</a>");
        assert_eq!(read.get("b").unwrap().unwrap(), b"x");
        assert_eq!(read.write(), bytes);
        assert!(Zip::read(b"not a zip at all, only words").is_err());
        assert!(Zip::read(&bytes[..bytes.len() - 30]).is_err());
        assert_eq!((beside("word/document.xml", "comments.xml"), beside("word/document.xml", "../custom/a.xml"), beside("word/document.xml", "/word/x.xml")), ("word/comments.xml".into(), "custom/a.xml".into(), "word/x.xml".into()));
        assert_eq!(rels_of("word/document.xml"), "word/_rels/document.xml.rels");
    }

    #[test]
    fn xml_is_taken_apart_as_written() {
        let xml = "<a x=\"1>2\" y='q'><b/>text &amp; more<c><c>in</c></c><!-- <d> --></a>";
        assert_eq!(children(inside(xml)), [("b", "<b/>"), ("", "text &amp; more"), ("c", "<c><c>in</c></c>"), ("", "<!-- <d> -->")]);
        assert_eq!((attr(open_tag(xml), "x").unwrap(), attr(open_tag(xml), "y").unwrap(), attr(open_tag(xml), "z")), ("1>2".into(), "q".into(), None));
        assert_eq!(unescape("a &lt; b &amp;&amp; &#x4e2d;&#25991; &unknown; &"), "a < b && 中文 &unknown; &");
        assert_eq!(without("<p><x a=\"1\"><y/></x><xy/><x/>t</p>", "x"), "<p><xy/>t</p>");
        assert_eq!(val_of("<w:pPr><w:pStyleX w:val=\"no\"/><w:pStyle w:val=\"Heading1\"/></w:pPr>", "w:pStyle").unwrap(), "Heading1");
    }

    #[test]
    fn a_file_is_read_into_paragraphs_headings_and_comments() {
        let read = read(&file(BODY, COMMENT)).unwrap();
        let shown: Vec<(&str, Option<u8>, bool)> = read.paragraphs.iter().map(|para| (para.text.as_str(), para.level, para.title)).collect();
        assert_eq!(shown, [("Roots & shoots", None, true), ("Introduction", Some(1), false), ("Roots grow down into the soil.", None, false), ("Hairs", Some(2), false), ("Root hairs take up water (Fig. 1).", None, false)]);
        assert_eq!(read.comments, [ReadComment { author: "Li Hua".into(), date: "2026-01-02T03:04:05Z".into(), body: "Which soil?\nSay more.".into(), paragraph: Some(2), quote: "soil".into() }]);
        assert_eq!((read.tables, read.pictures, read.tracked), (1, 0, true));
    }

    #[test]
    fn a_comment_over_several_paragraphs_is_about_the_first() {
        let body = "<w:p><w:r><w:t>One. </w:t></w:r><w:commentRangeStart w:id=\"0\"/><w:r><w:t>Two.</w:t></w:r></w:p><w:p><w:r><w:t>Three.</w:t></w:r><w:commentRangeEnd w:id=\"0\"/><w:r><w:commentReference w:id=\"0\"/></w:r></w:p><w:p><w:r><w:t>Four.</w:t></w:r><w:r><w:commentReference w:id=\"1\"/></w:r></w:p>";
        let comments = "<w:comment w:id=\"0\" w:author=\"A\"><w:p><w:r><w:t>both</w:t></w:r></w:p></w:comment><w:comment w:id=\"1\" w:author=\"A\"><w:p><w:r><w:t>here</w:t></w:r></w:p></w:comment><w:comment w:id=\"7\" w:author=\"A\"><w:p><w:r><w:t>nowhere</w:t></w:r></w:p></w:comment>";
        let read = read(&file(body, comments)).unwrap();
        assert_eq!(read.comments.iter().map(|c| (c.body.as_str(), c.paragraph, c.quote.as_str())).collect::<Vec<_>>(), [("both", Some(0), "Two."), ("here", Some(2), ""), ("nowhere", None, "")]);
    }

    #[test]
    fn nothing_changed_writes_the_document_as_it_was() {
        let source = file(BODY, COMMENT);
        let wanted: Vec<Wanted> = texts(&source).iter().map(|text| body(text)).collect();
        let done = rewrite(&source, &wanted, &Options::default()).unwrap();
        assert_eq!(body_of(&done.bytes), body_of(&source));
        assert_eq!((done.kept, done.changed, done.added, done.removed, done.comments), (5, 0, 0, 0, 0));
        assert_eq!(Zip::read(&done.bytes).unwrap().text("word/comments.xml").unwrap(), Zip::read(&source).unwrap().text("word/comments.xml").unwrap());
    }

    #[test]
    fn a_changed_paragraph_keeps_the_runs_of_what_stayed() {
        let source = file(BODY, COMMENT);
        let mut wanted: Vec<Wanted> = texts(&source).iter().map(|text| body(text)).collect();
        wanted[2].text = "Roots grow steadily down into the wet soil.".into();
        let done = rewrite(&source, &wanted, &Options::default()).unwrap();
        let out = body_of(&done.bytes);
        // The bold word is still bold and alone in its run; what came stands with the words before it; the comment's range is where it was.
        assert!(out.contains("<w:pPr><w:ind w:firstLine=\"420\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Roots grow steadily </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t xml:space=\"preserve\">down</w:t></w:r><w:r><w:t xml:space=\"preserve\"> into the wet </w:t></w:r><w:commentRangeStart w:id=\"4\"/><w:r><w:t xml:space=\"preserve\">soil</w:t></w:r><w:commentRangeEnd w:id=\"4\"/>"), "{out}");
        assert_eq!((done.kept, done.changed), (4, 1));
        assert_eq!(texts(&done.bytes)[2], "Roots grow steadily down into the wet soil.");
        // The table and the empty paragraph are where they were, and the other paragraphs as written.
        assert!(out.contains("<w:p/><w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:pPr><w:pStyle w:val=\"Sub\"/></w:pPr><w:r><w:t>Hairs</w:t></w:r></w:p>"));
    }

    #[test]
    fn a_change_around_a_footnote_a_tab_and_anothers_revisions_keeps_them() {
        let source = file(BODY, COMMENT);
        let mut wanted: Vec<Wanted> = texts(&source).iter().map(|text| body(text)).collect();
        wanted[4].text = "Fine root hairs take up water and salts (Fig. 1).".into();
        let out = body_of(&rewrite(&source, &wanted, &Options::default()).unwrap().bytes);
        assert!(out.contains("<w:r><w:t xml:space=\"preserve\">Fine root hairs take up </w:t></w:r><w:ins w:id=\"9\" w:author=\"Li\" w:date=\"2026-01-01T00:00:00Z\"><w:r><w:t xml:space=\"preserve\">water and salts</w:t></w:r></w:ins><w:del w:id=\"10\" w:author=\"Li\" w:date=\"2026-01-01T00:00:00Z\"><w:r><w:delText>nothing</w:delText></w:r></w:del><w:r><w:tab/><w:t xml:space=\"preserve\">(Fig. 1)</w:t><w:footnoteReference w:id=\"2\"/><w:t xml:space=\"preserve\">.</w:t></w:r></w:p>"), "{out}");
    }

    #[test]
    fn tracked_changes_strike_what_went_and_mark_what_came() {
        let source = file(BODY, COMMENT);
        let mut wanted: Vec<Wanted> = texts(&source).iter().map(|text| body(text)).collect();
        wanted[2].text = "Roots grow down into the wet ground.".into();
        wanted.remove(3);
        wanted.push(body("A new paragraph."));
        let options = Options { track: true, author: "Xu & Co".into(), date: "2026-11-12T00:00:00Z".into() };
        let done = rewrite(&source, &wanted, &options).unwrap();
        let out = body_of(&done.bytes);
        let who = "w:author=\"Xu &amp; Co\" w:date=\"2026-11-12T00:00:00Z\"";
        // Numbers go on from the largest the file has (10).
        assert!(out.contains(&format!("<w:r><w:t xml:space=\"preserve\"> into the </w:t></w:r><w:commentRangeStart w:id=\"4\"/><w:del w:id=\"11\" {who}><w:r><w:delText xml:space=\"preserve\">soil</w:delText></w:r></w:del><w:ins w:id=\"12\" {who}><w:r><w:t xml:space=\"preserve\">wet ground</w:t></w:r></w:ins><w:commentRangeEnd w:id=\"4\"/>")), "{out}");
        // The heading taken out is struck with its mark; the new paragraph is marked as put in, set like the body text before it.
        assert!(out.contains(&format!("<w:p><w:pPr><w:pStyle w:val=\"Sub\"/><w:rPr><w:del w:id=\"13\" {who}/></w:rPr></w:pPr><w:del w:id=\"14\" {who}><w:r><w:delText xml:space=\"preserve\">Hairs</w:delText></w:r></w:del></w:p>")), "{out}");
        assert!(out.ends_with(&format!("<w:p><w:pPr><w:rPr><w:ins w:id=\"15\" {who}/></w:rPr></w:pPr><w:ins w:id=\"16\" {who}><w:r><w:t xml:space=\"preserve\">A new paragraph.</w:t></w:r></w:ins></w:p>")), "{out}");
        assert_eq!((done.kept, done.changed, done.added, done.removed), (3, 1, 1, 1));
        // Read as if the changes were accepted, the file says what was wanted.
        assert_eq!(texts(&done.bytes), wanted.iter().map(|w| w.text.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn paragraphs_are_put_in_and_taken_out_where_they_belong() {
        let source = file(BODY, "");
        let mut wanted: Vec<Wanted> = texts(&source).iter().map(|text| body(text)).collect();
        wanted[0].role = Role::Title;
        wanted[1].role = Role::Heading;
        wanted[3].role = Role::Heading;
        wanted.remove(2);
        wanted.insert(2, body("Shoots grow up."));
        wanted.insert(3, body("Leaves follow the light."));
        wanted.insert(0, body("A line before all."));
        wanted.push(Wanted { text: "Outlook".into(), role: Role::Heading, notes: vec![] });
        let done = rewrite(&source, &wanted, &Options::default()).unwrap();
        assert_eq!(texts(&done.bytes), wanted.iter().map(|w| w.text.clone()).collect::<Vec<_>>());
        let out = body_of(&done.bytes);
        assert!(out.starts_with("<w:p><w:pPr><w:ind w:firstLine=\"420\"/></w:pPr><w:r><w:t xml:space=\"preserve\">A line before all.</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val=\"a3\"/>"), "{out}");
        // One paragraph went and two came: the old one is gone, the new ones are set like body text, before the table that followed.
        assert!(out.contains("<w:p><w:pPr><w:ind w:firstLine=\"420\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Shoots grow up.</w:t></w:r></w:p><w:p><w:pPr><w:ind w:firstLine=\"420\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Leaves follow the light.</w:t></w:r></w:p><w:p/><w:tbl>"), "{out}");
        // A new heading is set like the heading before it.
        assert!(out.ends_with("<w:p><w:pPr><w:pStyle w:val=\"Sub\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Outlook</w:t></w:r></w:p>"), "{out}");
        assert_eq!((done.kept, done.changed, done.added, done.removed), (4, 0, 4, 1));
    }

    #[test]
    fn comments_are_put_on_their_words_beside_those_of_the_file() {
        let source = file(BODY, COMMENT);
        let mut wanted: Vec<Wanted> = texts(&source).iter().map(|text| body(text)).collect();
        let note = |range: Range<usize>, body: &str| Note { range, author: "助手".into(), date: "2026-11-12T00:00:00Z".into(), body: body.into() };
        wanted[2].notes = vec![note(6..15, "为什么是 down？\n第二行 <b>")];
        wanted[1].notes = vec![note(0..0, "whole")];
        let done = rewrite(&source, &wanted, &Options::default()).unwrap();
        let read = read(&done.bytes).unwrap();
        let shown: Vec<(&str, &str, Option<usize>, &str)> = read.comments.iter().map(|c| (c.author.as_str(), c.body.as_str(), c.paragraph, c.quote.as_str())).collect();
        assert_eq!(shown, [("Li Hua", "Which soil?\nSay more.", Some(2), "soil"), ("助手", "whole", Some(1), "Introduction"), ("助手", "为什么是 down？\n第二行 <b>", Some(2), "grow down")]);
        let out = body_of(&done.bytes);
        // The range cuts the runs it starts and ends in; each half keeps its formatting.
        assert!(out.contains("<w:r><w:t xml:space=\"preserve\">Roots </w:t></w:r><w:commentRangeStart w:id=\"6\"/><w:r><w:t xml:space=\"preserve\">grow </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t xml:space=\"preserve\">down</w:t></w:r><w:commentRangeEnd w:id=\"6\"/><w:r><w:commentReference w:id=\"6\"/></w:r><w:r><w:t xml:space=\"preserve\"> into the </w:t></w:r>"), "{out}");
        assert_eq!(done.comments, 2);
        // A file without comments gets the part, its relationship and its type.
        let plain = file(BODY, "");
        let done = rewrite(&plain, &wanted, &Options::default()).unwrap();
        let zip = Zip::read(&done.bytes).unwrap();
        assert!(zip.text("word/comments.xml").unwrap().contains("<w:comment w:id=\"5\" w:author=\"助手\" w:date=\"2026-11-12T00:00:00Z\" w:initials=\"助\"><w:p><w:r><w:t xml:space=\"preserve\">whole</w:t></w:r></w:p></w:comment>"));
        assert!(zip.text("word/_rels/document.xml.rels").unwrap().contains("Id=\"rIdNote1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments\" Target=\"comments.xml\""));
        assert!(zip.text("[Content_Types].xml").unwrap().contains("<Override PartName=\"/word/comments.xml\""));
        assert_eq!(super::read(&done.bytes).unwrap().comments.len(), 2);
    }

    #[test]
    fn changes_are_found_word_by_word() {
        let op = |at: usize, del: usize, ins: &str| Op { at, del, ins: ins.into() };
        assert_eq!(ops("same", "same"), []);
        assert_eq!(ops("Roots grow down.", "Roots grow steadily down."), [op(11, 0, "steadily ")]);
        assert_eq!(ops("细胞图谱数据", "单细胞图谱的数据"), [op(0, 0, "单"), op(12, 0, "的")]);
        assert_eq!(ops("a cell wall", "a wall"), [op(2, 5, "")]);
        // A short word that both happen to have between two changes is not kept apart.
        assert_eq!(ops("We saw big roots of old plants here.", "We saw small leaves of young trees here."), [op(7, 23, "small leaves of young trees")]);
        assert_eq!(ops("", "new"), [op(0, 0, "new")]);
        assert_eq!(ops("old", ""), [op(0, 3, "")]);
    }

    #[test]
    fn paragraphs_are_matched_by_what_they_say() {
        use Step::*;
        assert_eq!(align(&["a", "b", "c"], &["a", "b", "c"]), [Same(0, 0), Same(1, 1), Same(2, 2)]);
        // One changed in place, one taken out, one put in elsewhere.
        assert_eq!(align(&["title", "roots grow down into the soil", "gone", "end"], &["title", "roots grow deep down into the soil", "end", "new"]), [Same(0, 0), Changed(1, 1), Removed(2), Same(3, 2), Added(3)]);
        // As many came as went: each stands for the one in its place, alike or not.
        assert_eq!(align(&["a", "x", "y", "b"], &["a", "完全不同", "another", "b"]), [Same(0, 0), Changed(1, 1), Changed(2, 2), Same(3, 3)]);
        // A paragraph cut in two: the half more like it is the paragraph changed, the other is new.
        assert_eq!(align(&["a", "roots grow down into the soil and take up water from it", "b"], &["a", "roots grow down into the soil", "and take up water", "b"]), [Same(0, 0), Changed(1, 1), Added(2), Same(2, 3)]);
        assert_eq!(align(&[], &["a"]), [Added(0)]);
        assert_eq!(align(&["a"], &[]), [Removed(0)]);
        assert!(likeness("单细胞时空组学", "单细胞时空组学如何") > 0.8 && likeness("abc", "xyz") == 0.);
    }

    #[test]
    fn a_paragraph_that_holds_more_than_text_stays_when_its_text_goes() {
        let body = "<w:p><w:r><w:t>Keep.</w:t></w:r></w:p><w:p><w:pPr><w:sectPr><w:type w:val=\"continuous\"/></w:sectPr></w:pPr><w:r><w:t>Goes, its section break stays.</w:t></w:r></w:p><w:p><w:r><w:t>Plain, goes.</w:t></w:r></w:p>";
        let done = rewrite(&file(body, ""), &[super::tests::body("Keep.")], &Options::default()).unwrap();
        assert_eq!(body_of(&done.bytes), "<w:p><w:r><w:t>Keep.</w:t></w:r></w:p><w:p><w:pPr><w:sectPr><w:type w:val=\"continuous\"/></w:sectPr></w:pPr></w:p>");
        // An empty file takes the text before its page properties.
        let done = rewrite(&file("", ""), &[super::tests::body("First words.")], &Options::default()).unwrap();
        assert_eq!(body_of(&done.bytes), "<w:p><w:r><w:t xml:space=\"preserve\">First words.</w:t></w:r></w:p>");
    }
}
