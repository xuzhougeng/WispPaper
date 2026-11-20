//! Citation styles as CSL, the definition files Zotero, Mendeley and
//! Pandoc share (`.csl`, from the Zotero style repository): how a
//! manuscript exported to Word cites in a journal's own form.
//!
//! A style is one of three: built in (`csl:<name>`, the set `hayagriva`
//! carries), a file the person added to `styles/` in the data folder
//! (`file:<file name>`), or one Zotero itself has installed
//! (`zotero:<file name>`, read where it is, never written). A dependent
//! style, which only names its parent, is that parent when it can be found
//! among the three. Styles that cite in footnotes are refused: a manuscript
//! here has no footnotes.
//!
//! The records keep no volume, issue or pages, so an entry has none in any
//! style. `render` gives the text of each citation and of the list with
//! what is set in italics or raised, and which part of a citation names
//! which paper (`cite::Rich`), so the Word file can lead from one to the
//! other.

use crate::cite::Rich;
use crate::docx::Look;
use crate::model::Paper;
use anyhow::{Context as _, Result, anyhow, bail};
use hayagriva::archive::ArchivedStyle;
use hayagriva::citationberg::{self, FontStyle, FontVariant, FontWeight, IndependentStyle, Locale, LocaleCode, StyleClass, TextDecoration, VerticalAlign};
use hayagriva::types::{Date, EntryType, FormatString, MaybeTyped, Numeric, PageRanges, Person};
use hayagriva::{BibliographyDriver, BibliographyRequest, CitationItem, CitationRequest, ElemChild, ElemChildren, ElemMeta, Entry, Formatting};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::OnceLock;

/// The folder of the data root that holds the styles the person added.
pub const FOLDER: &str = "styles";

/// The built-in styles offered first: the ones most manuscripts are asked for.
pub const COMMON: [&str; 8] = ["apa", "nature", "cell", "vancouver", "ieee", "gb-7714-2015-numeric", "gb-7714-2015-author-date", "chicago-author-date"];

/// A style that can be chosen: the key the settings keep, and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub key: String,
    pub name: String,
}

/// A style read and ready to cite with.
pub struct Loaded {
    pub key: String,
    pub name: String,
    style: IndependentStyle,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Loaded({})", self.key)
    }
}

impl PartialEq for Loaded {
    fn eq(&self, other: &Loaded) -> bool {
        self.key == other.key
    }
}

fn listed(style: ArchivedStyle) -> Listed {
    Listed { key: format!("csl:{}", style.names()[0]), name: style.display_name().to_string() }
}

/// `COMMON`, as they are listed.
pub fn common() -> Vec<Listed> {
    COMMON.iter().filter_map(|name| ArchivedStyle::by_name(name)).map(listed).collect()
}

/// Every built-in style, by name.
pub fn bundled() -> Vec<Listed> {
    let mut all: Vec<Listed> = ArchivedStyle::all().iter().copied().map(listed).collect();
    all.sort_by_key(|style| style.name.to_lowercase());
    all
}

/// Where Zotero keeps the styles it has installed, unless the person moved its data.
pub fn zotero_folder() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join("Zotero").join("styles")).filter(|folder| folder.is_dir())
}

/// What stands between `<name>` and `</name>` the first time, as a style's own file says its title and id.
fn tagged(xml: &str, name: &str) -> Option<String> {
    let start = xml.find(&format!("<{name}>"))? + name.len() + 2;
    let end = start + xml[start..].find(&format!("</{name}>"))?;
    Some(xml[start..end].trim().replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&apos;", "'").replace("&quot;", "\"")).filter(|text| !text.is_empty())
}

fn files(folder: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(folder) else { return vec![] };
    let mut found: Vec<(String, String)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("csl")))
        .filter_map(|entry| Some((entry.file_name().to_str()?.to_string(), std::fs::read_to_string(entry.path()).ok()?)))
        .collect();
    found.sort();
    found
}

/// The styles beside the built-in ones: those added to the data folder,
/// then those Zotero has that are neither built in nor added.
pub fn added(root: &Path) -> Vec<Listed> {
    added_from(root, zotero_folder().as_deref())
}

fn added_from(root: &Path, zotero: Option<&Path>) -> Vec<Listed> {
    let name = |file: &str, xml: &str| tagged(xml, "title").unwrap_or_else(|| file.trim_end_matches(".csl").to_string());
    let own = files(&root.join(FOLDER));
    let mut out: Vec<Listed> = own.iter().map(|(file, xml)| Listed { key: format!("file:{file}"), name: name(file, xml) }).collect();
    let ids: Vec<String> = own.iter().filter_map(|(_, xml)| tagged(xml, "id")).collect();
    for (file, xml) in zotero.map(files).unwrap_or_default() {
        let id = tagged(&xml, "id").unwrap_or_default();
        if ArchivedStyle::by_id(&id).is_some() || ids.contains(&id) || own.iter().any(|(mine, _)| *mine == file) { continue }
        out.push(Listed { key: format!("zotero:{file}"), name: name(&file, &xml) });
    }
    out
}

/// The name a key is shown by; none for a key that names no style any more.
pub fn name_of(key: &str, root: &Path) -> Option<String> {
    match key.strip_prefix("csl:") {
        Some(name) => ArchivedStyle::by_name(name).map(|style| style.display_name().to_string()),
        None => added(root).into_iter().find(|style| style.key == key).map(|style| style.name),
    }
}

/// The file a key of a style outside the built-in ones names.
fn file_of(key: &str, root: &Path, zotero: Option<&Path>) -> Option<PathBuf> {
    let (folder, file) = match (key.strip_prefix("file:"), key.strip_prefix("zotero:")) {
        (Some(file), _) => (root.join(FOLDER), file),
        (_, Some(file)) => (zotero?.to_path_buf(), file),
        _ => return None,
    };
    // A key is a file's name, never a way out of the folder.
    if file.is_empty() || file.contains(['/', '\\']) || file.starts_with('.') { return None }
    Some(folder.join(file))
}

/// A style's file as the style to cite with: itself, or the parent a dependent one names.
fn independent(xml: &str, root: &Path, zotero: Option<&Path>) -> Result<IndependentStyle> {
    let style = citationberg::Style::from_xml(xml).map_err(|err| anyhow!("这不是能读的 CSL 样式文件：{err}"))?;
    let style = match style {
        citationberg::Style::Independent(style) => style,
        citationberg::Style::Dependent(dependent) => {
            let parent = dependent.parent_link.href.clone();
            let file = format!("{}.csl", parent.trim_end_matches('/').rsplit('/').next().unwrap_or_default());
            let beside = [Some(root.join(FOLDER)), zotero.map(Path::to_path_buf)].into_iter().flatten().find_map(|folder| std::fs::read_to_string(folder.join(&file)).ok());
            let found = match (ArchivedStyle::by_id(&parent), beside) {
                (Some(archived), _) => Some(archived.get()),
                (None, Some(xml)) => citationberg::Style::from_xml(&xml).ok(),
                (None, None) => None,
            };
            match found {
                Some(citationberg::Style::Independent(style)) => style,
                _ => bail!("这个样式只是「{}」的别名，本身没有格式定义。请一并添加 {file}（在 Zotero 样式库里搜同名样式）。", tagged(xml, "title").unwrap_or_default()),
            }
        }
    };
    if style.settings.class == StyleClass::Note { bail!("这是脚注式的引用样式，稿件里没有脚注，暂不支持。") }
    Ok(style)
}

/// The style a key names.
pub fn load(key: &str, root: &Path) -> Result<Loaded> {
    load_from(key, root, zotero_folder().as_deref())
}

fn load_from(key: &str, root: &Path, zotero: Option<&Path>) -> Result<Loaded> {
    if let Some(name) = key.strip_prefix("csl:") {
        let archived = ArchivedStyle::by_name(name).ok_or_else(|| anyhow!("没有这个内置引用样式：{name}"))?;
        let citationberg::Style::Independent(style) = archived.get() else { bail!("内置样式 {name} 读不了") };
        if style.settings.class == StyleClass::Note { bail!("这是脚注式的引用样式，稿件里没有脚注，暂不支持。") }
        return Ok(Loaded { key: key.to_string(), name: archived.display_name().to_string(), style });
    }
    let path = file_of(key, root, zotero).ok_or_else(|| anyhow!("不认识这个引用样式：{key}"))?;
    let xml = std::fs::read_to_string(&path).with_context(|| format!("引用样式文件不在了：{}", path.display()))?;
    let name = tagged(&xml, "title").unwrap_or_else(|| key.to_string());
    Ok(Loaded { key: key.to_string(), name, style: independent(&xml, root, zotero)? })
}

/// Take a `.csl` file into the data folder's styles, once it is found to be one that can be cited with.
pub fn add(root: &Path, file: &Path) -> Result<Listed> {
    let xml = std::fs::read_to_string(file).with_context(|| format!("读不了 {}", file.display()))?;
    let name = file.file_name().and_then(|name| name.to_str()).ok_or_else(|| anyhow!("文件名读不了"))?;
    save(root, name, &xml)
}

/// Keep a style's XML under a file name in the data folder's styles, once
/// it is found to be one that can be cited with. A file of that name that
/// says something else is kept first, in `styles/removed/`.
pub fn save(root: &Path, name: &str, xml: &str) -> Result<Listed> {
    independent(xml, root, zotero_folder().as_deref())?;
    let name = name.trim();
    let name = if name.to_lowercase().ends_with(".csl") { name.to_string() } else { format!("{name}.csl") };
    if name.len() <= 4 || name.starts_with('.') || name.contains(['/', '\\', ':']) { bail!("样式的文件名不能用：{name}") }
    let folder = root.join(FOLDER);
    std::fs::create_dir_all(&folder).with_context(|| format!("无法创建样式文件夹 {}", folder.display()))?;
    let file = folder.join(&name);
    if let Ok(before) = std::fs::read_to_string(&file) && before != xml {
        let kept = folder.join("removed");
        std::fs::create_dir_all(&kept)?;
        std::fs::write(kept.join(format!("{}-{name}", crate::time::now_iso().replace(':', "-"))), before)?;
    }
    std::fs::write(&file, xml).with_context(|| format!("无法保存 {name}"))?;
    Ok(Listed { key: format!("file:{name}"), name: tagged(xml, "title").unwrap_or_else(|| name.trim_end_matches(".csl").to_string()) })
}

/// A style's definition as its file says it, a built-in one written out: what a changed style starts from.
pub fn xml_of(key: &str, root: &Path) -> Result<String> {
    if let Some(name) = key.strip_prefix("csl:") {
        let archived = ArchivedStyle::by_name(name).ok_or_else(|| anyhow!("没有这个内置引用样式：{name}"))?;
        return archived.get().to_xml().map_err(|err| anyhow!("内置样式 {name} 写不出来：{err}"));
    }
    let path = file_of(key, root, zotero_folder().as_deref()).ok_or_else(|| anyhow!("不认识这个引用样式：{key}"))?;
    std::fs::read_to_string(&path).with_context(|| format!("引用样式文件不在了：{}", path.display()))
}

/// A style from its XML alone, kept nowhere: for trying one out.
pub fn parse(xml: &str, root: &Path) -> Result<Loaded> {
    Ok(Loaded { key: String::new(), name: tagged(xml, "title").unwrap_or_default(), style: independent(xml, root, zotero_folder().as_deref())? })
}

/// The style the settings name: the two simple ones of `cite::Style`, or a CSL style.
pub fn style(key: &str, root: &Path) -> Result<crate::cite::Style> {
    if key.contains(':') { Ok(crate::cite::Style::Csl(std::sync::Arc::new(load(key, root)?))) } else { Ok(crate::cite::Style::from_key(key)) }
}

// ---- citing

fn locales() -> &'static [Locale] {
    static LOCALES: OnceLock<Vec<Locale>> = OnceLock::new();
    LOCALES.get_or_init(hayagriva::archive::locales)
}

/// The locale a manuscript's language asks a style's terms in (`et al.`, `and`); the style's own for English.
fn locale(language: &str) -> Option<LocaleCode> {
    let code = match language {
        "zh" => "zh-CN",
        "de" => "de-DE",
        "fr" => "fr-FR",
        "es" => "es-ES",
        "ja" => "ja-JP",
        _ => return None,
    };
    Some(LocaleCode(code.into()))
}

/// A paper as a CSL processor reads one: an article, in its journal when it has one.
/// The volume and issue are the journal's, the pages the article's.
fn entry(paper: &Paper) -> Entry {
    let mut entry = Entry::new(&paper.id, EntryType::Article);
    entry.set_title(FormatString::with_value(paper.title.trim()));
    let authors: Vec<Person> = crate::model::author_names(&paper.authors).into_iter().map(|(family, given)| Person { name: family, given_name: Some(given).filter(|given| !given.is_empty()), prefix: None, suffix: None, comma_suffix: false, alias: None }).collect();
    if !authors.is_empty() { entry.set_authors(authors) }
    if let Some(year) = paper.year { entry.set_date(Date::from_year(year)) }
    if !paper.doi.trim().is_empty() { entry.set_doi(paper.doi.trim().to_string()) }
    if !paper.pmid.trim().is_empty() { entry.set_pmid(paper.pmid.trim().to_string()) }
    if !paper.journal.trim().is_empty() {
        let mut journal = Entry::new("journal", EntryType::Periodical);
        journal.set_title(FormatString::with_value(paper.journal.trim()));
        if let Some(volume) = numbered(&paper.volume) { journal.set_volume(volume) }
        if let Some(issue) = numbered(&paper.issue) { journal.set_issue(issue) }
        entry.set_parents(vec![journal]);
    }
    // A range a style can shorten or give an en dash; an article number as it is written.
    if !paper.pages.trim().is_empty() { entry.set_page_range(MaybeTyped::<PageRanges>::from_str(paper.pages.trim()).unwrap_or_else(|never| match never {})) }
    entry
}

/// A volume or issue: a number where it is one (`585`, `12-13`), else the words as written (`S1`).
fn numbered(value: &str) -> Option<MaybeTyped<Numeric>> {
    let value = value.trim();
    (!value.is_empty()).then(|| MaybeTyped::from_str(value).unwrap_or_else(|never| match never {}))
}

fn look(formatting: &Formatting) -> Look {
    Look {
        italic: matches!(formatting.font_style, FontStyle::Italic),
        bold: matches!(formatting.font_weight, FontWeight::Bold),
        sup: matches!(formatting.vertical_align, VerticalAlign::Sup),
        sub: matches!(formatting.vertical_align, VerticalAlign::Sub),
        small_caps: matches!(formatting.font_variant, FontVariant::SmallCaps),
        underline: matches!(formatting.text_decoration, TextDecoration::Underline),
    }
}

/// What a processor rendered, as one line of text with its looks; the parts that are one cited item are linked to `ids[n]`.
fn flatten(children: &ElemChildren, ids: &[&str], out: &mut Rich) {
    let push = |out: &mut Rich, text: &str, formatting: Option<&Formatting>| {
        let start = out.text.len();
        // One paragraph of a Word file: a style's line breaks are spaces.
        out.text.extend(text.chars().map(|c| if c == '\n' || c == '\t' { ' ' } else { c }));
        let look = formatting.map(look).unwrap_or_default();
        if look == Look::default() || out.text.len() == start { return }
        match out.looks.last_mut() {
            Some((range, last)) if *last == look && range.end == start => range.end = out.text.len(),
            _ => out.looks.push((start..out.text.len(), look)),
        }
    };
    for child in &children.0 {
        match child {
            ElemChild::Text(text) => push(out, &text.text, Some(&text.formatting)),
            ElemChild::Link { text, .. } => push(out, &text.text, Some(&text.formatting)),
            ElemChild::Markup(text) => push(out, text, None),
            ElemChild::Transparent { .. } => {}
            ElemChild::Elem(elem) => {
                let start = out.text.len();
                flatten(&elem.children, ids, out);
                let Some(ElemMeta::Entry(n)) = elem.meta else { continue };
                let said = &out.text[start..];
                let (lead, kept) = (said.len() - said.trim_start().len(), said.trim().len());
                if let Some(id) = ids.get(n).filter(|_| kept > 0) { out.links.push((start + lead..start + lead + kept, id.to_string())) }
            }
        }
    }
}

/// The text without the spaces around it, and its ranges kept inside it.
fn trimmed(mut rich: Rich) -> Rich {
    while rich.text.contains("  ") { rich = squeezed(rich) }
    let end = rich.text.trim_end().len();
    rich.text.truncate(end);
    let lead = rich.text.len() - rich.text.trim_start().len();
    rich.text.drain(..lead);
    let fit = |range: &std::ops::Range<usize>| range.start.saturating_sub(lead).min(end - lead)..range.end.saturating_sub(lead).min(end - lead);
    rich.looks = rich.looks.iter().map(|(range, look)| (fit(range), *look)).filter(|(range, _)| !range.is_empty()).collect();
    rich.links = rich.links.iter().map(|(range, to)| (fit(range), to.clone())).filter(|(range, _)| !range.is_empty()).collect();
    rich
}

/// Two spaces in a row, which a line break beside a space leaves, as one.
fn squeezed(rich: Rich) -> Rich {
    let Some(at) = rich.text.find("  ") else { return rich };
    let shift = |n: usize| if n > at { n - 1 } else { n };
    let mut text = rich.text;
    text.remove(at);
    Rich { text, looks: rich.looks.into_iter().map(|(range, look)| (shift(range.start)..shift(range.end), look)).collect(), links: rich.links.into_iter().map(|(range, to)| (shift(range.start)..shift(range.end), to)).collect() }
}

/// The citations of a manuscript in a style: each group of papers as it is
/// written in the text, in the order given, and the reference list with the
/// paper each entry is (empty for a style that has none).
pub fn render(style: &Loaded, groups: &[Vec<&Paper>], language: &str) -> (Vec<Rich>, Vec<(String, Rich)>) {
    let mut entries: HashMap<&str, Entry> = HashMap::new();
    for paper in groups.iter().flatten() { entries.entry(paper.id.as_str()).or_insert_with(|| entry(paper)); }
    let locale = locale(language);
    let mut driver = BibliographyDriver::new();
    for group in groups {
        let items = group.iter().map(|paper| CitationItem::with_entry(&entries[paper.id.as_str()])).collect();
        driver.citation(CitationRequest::new(items, &style.style, locale.clone(), locales(), None));
    }
    let rendered = driver.finish(BibliographyRequest::new(&style.style, locale, locales()));
    let cited = rendered
        .citations
        .iter()
        .zip(groups)
        .map(|(citation, group)| {
            let ids: Vec<&str> = group.iter().map(|paper| paper.id.as_str()).collect();
            let mut rich = Rich::default();
            flatten(&citation.citation, &ids, &mut rich);
            trimmed(rich)
        })
        .collect();
    let list = rendered
        .bibliography
        .map(|bibliography| {
            bibliography
                .items
                .iter()
                .map(|item| {
                    let mut rich = Rich::default();
                    // The number a numbered list sets apart before each entry: `1.`, `[1]`.
                    if let Some(first) = &item.first_field {
                        flatten(&ElemChildren(vec![first.clone()]), &[], &mut rich);
                        rich.text.push(' ');
                    }
                    flatten(&item.content, &[], &mut rich);
                    (item.key.clone(), trimmed(rich))
                })
                .collect()
        })
        .unwrap_or_default();
    (cited, list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(id: &str, authors: &str, year: i32, title: &str, journal: &str) -> Paper {
        Paper { id: id.into(), authors: authors.into(), year: Some(year), title: title.into(), journal: journal.into(), ..Default::default() }
    }

    fn temp(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("wisp-csl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn the_common_styles_are_all_built_in_and_cite_in_the_text() {
        let root = temp("common");
        assert_eq!(common().len(), COMMON.len());
        for one in common() { assert!(load_from(&one.key, &root, None).is_ok(), "{}", one.key) }
        assert!(bundled().len() > 50);
        // A style that cites in footnotes is said to be one.
        assert!(load_from("csl:chicago-notes", &root, None).unwrap_err().to_string().contains("脚注"));
        assert!(load_from("csl:nothing", &root, None).is_err());
        assert!(load_from("file:../x.csl", &root, None).is_err());
    }

    #[test]
    fn a_numbered_style_numbers_raises_and_links_each_paper() {
        let root = temp("nature");
        let style = load_from("csl:nature", &root, None).unwrap();
        let (a, b, c) = (paper("a", "Zhang, Wei; Li, Qiang; Wang, Fang", 2024, "Single-cell atlas of roots", "Nature Plants"), paper("b", "Smith, John", 2019, "Second", "Cell"), paper("c", "Li, Qiang", 2020, "Third", "Science"));
        let (cited, list) = render(&style, &[vec![&a], vec![&a, &b, &c], vec![&c, &a]], "en");
        assert_eq!(cited.iter().map(|rich| rich.text.as_str()).collect::<Vec<_>>(), ["1", "1–3", "1,3"]);
        assert_eq!(cited[0].links, [(0..1, "a".to_string())]);
        // A range names its two ends; a list every number, whatever order the papers were cited in.
        assert_eq!(cited[1].links, [(0..1, "a".to_string()), (4..5, "c".to_string())]);
        assert_eq!(cited[2].links, [(0..1, "a".to_string()), (2..3, "c".to_string())]);
        assert!(cited[1].looks.iter().all(|(_, look)| look.sup) && cited[1].looks.iter().map(|(range, _)| range.len()).sum::<usize>() == cited[1].text.len());
        assert_eq!(list.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert!(list[0].1.text.starts_with("1. Zhang, W., Li, Q. & Wang, F. Single-cell atlas of roots."), "{}", list[0].1.text);
        // The journal is in italics.
        let (range, look) = list[0].1.looks.iter().find(|(_, look)| look.italic).unwrap();
        assert_eq!((&list[0].1.text[range.clone()], look.bold), ("Nature Plants", false));
    }

    #[test]
    fn an_entry_says_where_in_the_journal_the_paper_is() {
        let root = temp("placed");
        let mut numpy = paper("a", "Harris, Charles R.; Millman, K. Jarrod", 2020, "Array programming with NumPy", "Nature");
        (numpy.volume, numpy.issue, numpy.pages) = ("585".into(), "7825".into(), "357-362".into());
        let entry = |style: &str, paper: &Paper| render(&load_from(style, &root, None).unwrap(), &[vec![paper]], "en").1[0].1.clone();
        let apa = entry("csl:apa", &numpy);
        assert_eq!(apa.text, "Harris, C. R., & Millman, K. J. (2020). Array programming with NumPy. Nature, 585(7825), 357–362.");
        // APA: the journal and the volume in italics, the issue not.
        let italic: Vec<&str> = apa.looks.iter().filter(|(_, look)| look.italic).map(|(range, _)| &apa.text[range.clone()]).collect();
        assert_eq!(italic, ["Nature", "585"]);
        // Nature: the volume in bold (with its comma, as the style has it), no issue.
        let nature = entry("csl:nature", &numpy);
        assert_eq!(nature.text, "1. Harris, C. R. & Millman, K. J. Array programming with NumPy. Nature 585, 357–362 (2020).");
        let bold: Vec<&str> = nature.looks.iter().filter(|(_, look)| look.bold).map(|(range, _)| &nature.text[range.clone()]).collect();
        assert_eq!(bold, ["585,"]);
        // An article number and a supplement's issue are written as they are; what a record lacks is left out.
        (numpy.volume, numpy.issue, numpy.pages) = ("12".into(), "S1".into(), "e1234".into());
        assert_eq!(entry("csl:apa", &numpy).text, "Harris, C. R., & Millman, K. J. (2020). Array programming with NumPy. Nature, 12(S1), e1234.");
        (numpy.issue, numpy.pages) = (String::new(), String::new());
        assert_eq!(entry("csl:apa", &numpy).text, "Harris, C. R., & Millman, K. J. (2020). Array programming with NumPy. Nature, 12.");
    }

    #[test]
    fn an_author_date_style_tells_alike_papers_apart_and_sorts_its_list() {
        let root = temp("apa");
        let style = load_from("csl:apa", &root, None).unwrap();
        let (a, b, c) = (paper("a", "Zhang, Wei; Li, Qiang; Wang, Fang", 2024, "Single-cell atlas of roots", "Nature Plants"), paper("b", "Zhang, Wei; Li, Qiang; Wang, Fang", 2024, "Another atlas", "Cell"), paper("c", "Smith, John", 2019, "Third", "Science"));
        let (cited, list) = render(&style, &[vec![&a], vec![&b, &c]], "en");
        assert_eq!(cited[0].text, "(Zhang et al., 2024a)");
        assert_eq!(cited[1].text, "(Smith, 2019; Zhang et al., 2024b)");
        assert_eq!(cited[1].links.iter().map(|(range, to)| (&cited[1].text[range.clone()], to.as_str())).collect::<Vec<_>>(), [("Smith, 2019", "c"), ("Zhang et al., 2024b", "b")]);
        assert_eq!(list.len(), 3);
        assert_eq!((list[0].0.as_str(), list[0].1.text.as_str()), ("c", "Smith, J. (2019). Third. Science."));
        // The letter a citation carries is the one its entry has.
        assert!(list.iter().any(|(id, entry)| id == "b" && entry.text.contains("(2024b). Another atlas.")), "{list:?}");
    }

    const OWN: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<style xmlns="http://purl.org/net/xbiblio/csl" class="in-text" version="1.0">
  <info><title>Journal of Roots &amp; Shoots</title><id>http://www.zotero.org/styles/journal-of-roots</id><updated>2024-01-01T00:00:00+00:00</updated></info>
  <citation collapse="citation-number"><sort><key variable="citation-number"/></sort><layout prefix="{" suffix="}" delimiter=";"><text variable="citation-number"/></layout></citation>
  <bibliography><layout suffix="."><text variable="citation-number" suffix=") "/><names variable="author"><name initialize-with="" name-as-sort-order="all" sort-separator=" "/></names><text variable="title" prefix=": " font-weight="bold"/></layout></bibliography>
</style>"#;

    const ALIAS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<style xmlns="http://purl.org/net/xbiblio/csl" version="1.0" default-locale="en-US">
  <info><title>Plant Letters</title><id>http://www.zotero.org/styles/plant-letters</id><link href="http://www.zotero.org/styles/PARENT" rel="independent-parent"/><updated>2024-01-01T00:00:00+00:00</updated></info>
</style>"#;

    #[test]
    fn a_file_of_the_persons_or_of_zotero_is_a_style_and_an_alias_is_its_parent() {
        let (root, zotero, elsewhere) = (temp("own"), temp("zotero"), temp("elsewhere"));
        std::fs::write(elsewhere.join("journal-of-roots.csl"), OWN).unwrap();
        let listed = add(&root, &elsewhere.join("journal-of-roots.csl")).unwrap();
        assert_eq!(listed, Listed { key: "file:journal-of-roots.csl".into(), name: "Journal of Roots & Shoots".into() });
        // Zotero's: the one built in and the one added are not listed twice.
        std::fs::write(zotero.join("apa.csl"), OWN.replace("journal-of-roots", "apa")).unwrap();
        std::fs::write(zotero.join("journal-of-roots.csl"), OWN).unwrap();
        std::fs::write(zotero.join("plant-letters.csl"), ALIAS.replace("PARENT", "journal-of-roots")).unwrap();
        std::fs::write(zotero.join("lost.csl"), ALIAS.replace("PARENT", "nowhere").replace("plant-letters", "lost")).unwrap();
        std::fs::write(zotero.join("of-nature.csl"), ALIAS.replace("PARENT", "nature").replace("plant-letters", "of-nature")).unwrap();
        let keys: Vec<String> = added_from(&root, Some(&zotero)).into_iter().map(|style| style.key).collect();
        assert_eq!(keys, ["file:journal-of-roots.csl", "zotero:lost.csl", "zotero:of-nature.csl", "zotero:plant-letters.csl"]);

        let a = paper("a", "Zhang, Wei; Li, Qiang", 2024, "Atlas", "Cell");
        let b = paper("b", "Smith, John", 2019, "Second", "Cell");
        for key in ["file:journal-of-roots.csl", "zotero:plant-letters.csl"] {
            let style = load_from(key, &root, Some(&zotero)).unwrap();
            let (cited, list) = render(&style, &[vec![&a, &b]], "en");
            assert_eq!(cited[0].text, "{1;2}", "{key}");
            assert_eq!(list[0].1.text, "1) Zhang W, Li Q: Atlas.");
            assert_eq!(list[0].1.looks.iter().map(|(range, look)| (&list[0].1.text[range.clone()], look.bold)).collect::<Vec<_>>(), [(": Atlas", true)]);
        }
        assert_eq!(load_from("zotero:of-nature.csl", &root, Some(&zotero)).unwrap().name, "Plant Letters");
        let lost = load_from("zotero:lost.csl", &root, Some(&zotero)).unwrap_err().to_string();
        assert!(lost.contains("nowhere.csl"), "{lost}");
        assert!(add(&root, &elsewhere.join("missing.csl")).is_err());
        std::fs::write(elsewhere.join("not.csl"), "<html/>").unwrap();
        assert!(add(&root, &elsewhere.join("not.csl")).is_err());
    }
}
