//! Papers, custom field definitions and attachments. Plain data plus the
//! normalisation every write goes through.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One paper. `fields` holds the values of the library's custom fields
/// (自定义词条) by field id; an absent or empty list means "not filled in".
/// `fulltext` is computed from the attachments: missing, linked, stored or textless.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Paper {
    pub id: String,
    pub title: String,
    pub doi: String,
    pub pmid: String,
    /// Names separated by "; ".
    pub authors: String,
    pub year: Option<i32>,
    pub journal: String,
    /// unknown, preprint, published or unpublished.
    pub publication_status: String,
    pub url: String,
    #[serde(rename = "abstract")]
    pub abstract_text: String,
    pub notes: String,
    pub tags: Vec<String>,
    /// pending, included, excluded, uncertain or irrelevant.
    pub screening: String,
    pub screening_reason: String,
    /// When the screening decision was made; empty while pending.
    pub reviewed_at: String,
    /// unread, skimmed or read.
    pub reading: String,
    pub starred: bool,
    pub source: String,
    pub fields: BTreeMap<String, Vec<String>>,
    pub fulltext: String,
    pub added_at: String,
    pub updated_at: String,
}

pub const SCREENINGS: [(&str, &str); 5] = [("pending", "待判断"), ("included", "已纳入"), ("excluded", "相关"), ("uncertain", "证据不足"), ("irrelevant", "不相关")];
pub const READINGS: [(&str, &str); 3] = [("unread", "未读"), ("skimmed", "已浏览"), ("read", "已精读")];
/// `textless`: a full text is stored, and no text could be had from it.
pub const FULLTEXTS: [(&str, &str); 4] = [("missing", "无全文"), ("linked", "有链接"), ("stored", "已存 PDF"), ("textless", "PDF 无文字")];
pub const PUBLICATION_STATUSES: [(&str, &str); 4] = [("unknown", "尚未确认"), ("preprint", "预印本"), ("published", "正式发表"), ("unpublished", "未发表")];

/// The display label of a stored value; the value itself when unknown.
pub fn label<'a>(table: &'a [(&'a str, &'a str)], value: &'a str) -> &'a str {
    table.iter().find(|(v, _)| *v == value).map_or(value, |(_, l)| l)
}

pub fn one_of(table: &[(&str, &str)], value: &str) -> bool {
    table.iter().any(|(v, _)| *v == value)
}

/// What kind of values a custom field takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    /// One free line of text.
    #[default]
    Text,
    /// Any number of values, comma separated when typed.
    Tags,
    /// One value out of `options`.
    Select,
}

impl FieldKind {
    pub const ALL: [FieldKind; 3] = [FieldKind::Text, FieldKind::Tags, FieldKind::Select];

    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Tags => "tags",
            FieldKind::Select => "select",
        }
    }

    pub fn parse(value: &str) -> FieldKind {
        match value {
            "tags" => FieldKind::Tags,
            "select" => FieldKind::Select,
            _ => FieldKind::Text,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FieldKind::Text => "文本",
            FieldKind::Tags => "多值",
            FieldKind::Select => "单选",
        }
    }
}

/// A custom field (自定义词条) of the library: 物种, 研究方法, 课题…
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FieldDef {
    pub id: String,
    pub name: String,
    pub kind: FieldKind,
    /// Choices of a Select field; suggestions for a Tags field.
    pub options: Vec<String>,
    /// Shown as a column of the paper table.
    pub in_table: bool,
    /// What the field is for, in the person's words: what the assistant is told to look for (提取).
    pub hint: String,
}

/// A file stored under the library's `papers/` folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PaperFile {
    pub id: String,
    pub paper_id: String,
    /// fulltext, supplement or other.
    pub kind: String,
    /// The original file name.
    pub title: String,
    /// Lowercase extension without the dot.
    pub format: String,
    /// Relative to the library folder, `/` separated.
    pub local_path: String,
    pub size_bytes: u64,
    pub added_at: String,
}

pub fn file_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 { format!("{} KB", (bytes as f64 / 1024.).round().max(1.) as u64) } else { format!("{:.1} MB", bytes as f64 / 1024. / 1024.) }
}

// ---- normalisation

/// The bare lowercase DOI, without a link or label around it or the
/// punctuation a citation leaves behind.
pub fn normalize_doi(value: &str) -> String {
    let lower = value.trim().to_lowercase();
    let mut rest = lower.as_str();
    let after_scheme = rest.strip_prefix("https://").or_else(|| rest.strip_prefix("http://")).unwrap_or(rest);
    let after_host = after_scheme.strip_prefix("dx.").or_else(|| after_scheme.strip_prefix("www.")).unwrap_or(after_scheme);
    if let Some(bare) = after_host.strip_prefix("doi.org/") {
        rest = bare;
    } else if let Some(labelled) = rest.strip_prefix("doi") && labelled.starts_with(label_break) {
        rest = labelled.trim_start_matches(label_break);
    }
    rest.trim().trim_end_matches(['.', ',', ';']).to_string()
}

/// What stands between a label (`doi`, `pmid`) and its number: a colon of
/// either width or any space, a no-break one included.
fn label_break(c: char) -> bool {
    c == ':' || c == '：' || c.is_whitespace()
}

/// `10.<4–9 digits>/<anything without spaces>`; an empty DOI is fine.
pub fn valid_doi(value: &str) -> bool {
    if value.is_empty() { return true }
    let Some(rest) = value.strip_prefix("10.") else { return false };
    let Some((registrant, suffix)) = rest.split_once('/') else { return false };
    (4..=9).contains(&registrant.len()) && registrant.bytes().all(|b| b.is_ascii_digit()) && !suffix.is_empty() && !suffix.contains(char::is_whitespace)
}

/// The bare PMID from a label or a PubMed link.
pub fn normalize_pmid(value: &str) -> String {
    let lower = value.trim().to_lowercase();
    let mut rest = lower.as_str();
    let after_scheme = rest.strip_prefix("https://").or_else(|| rest.strip_prefix("http://")).unwrap_or(rest);
    let after_www = after_scheme.strip_prefix("www.").unwrap_or(after_scheme);
    if let Some(bare) = after_www.strip_prefix("pubmed.ncbi.nlm.nih.gov/").or_else(|| after_www.strip_prefix("ncbi.nlm.nih.gov/pubmed/")) {
        rest = bare;
    } else if let Some(labelled) = rest.strip_prefix("pmid") && labelled.starts_with(label_break) {
        rest = labelled.trim_start_matches(label_break);
    }
    rest.trim().trim_end_matches(['/', '.', ',', ';']).to_string()
}

/// A title reduced to its lowercase letters and digits, so markup,
/// punctuation and hyphenation cannot hide a duplicate.
pub fn title_key(title: &str) -> String {
    strip_markup(title).chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Text without `<tags>`, with whitespace collapsed. A tag opens with a
/// letter or a slash and closes: a `<` on its own ("IL-6 < 10 pg/mL",
/// "p<0.05") is a character of the text.
pub fn strip_markup(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        match after.find('>').filter(|_| after.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/')) {
            Some(end) => {
                out.push(' ');
                rest = &after[end + 1..];
            }
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    decode_entities(&out).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `&amp;`, `&#x2009;`, `&#8201;` and the like as the characters they stand
/// for, each read once; anything unknown stays as written.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest[1..].find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = &rest[1..end + 1];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name.strip_prefix("#x").or(name.strip_prefix("#X")).map(|hex| u32::from_str_radix(hex, 16)).or(name.strip_prefix('#').map(str::parse::<u32>)).and_then(Result::ok).and_then(char::from_u32),
            };
            c.map(|c| (c, end + 2))
        });
        match decoded {
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

/// Trim every entry, drop blanks and duplicates, keep the order.
pub fn clean_strings<I: IntoIterator<Item = S>, S: AsRef<str>>(values: I) -> Vec<String> {
    let mut clean: Vec<String> = vec![];
    for value in values {
        let value = value.as_ref().trim();
        if !value.is_empty() && !clean.iter().any(|kept| kept == value) {
            clean.push(value.to_string());
        }
    }
    clean
}

/// Split what a person typed into a list: commas, semicolons (either width) and newlines separate.
pub fn split_list(value: &str) -> Vec<String> {
    clean_strings(value.split([',', '，', ';', '；', '\n']))
}

pub fn clean_authors(value: &str) -> String {
    clean_strings(value.split([';', '；'])).join("; ")
}

/// Unique enough ids without a random number crate: time plus a hashed counter.
pub fn new_id(prefix: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    format!("{prefix}-{}-{:08x}", crate::time::now_millis(), hasher.finish() as u32)
}

impl Paper {
    /// Bring every field into its stored shape and fill the defaults.
    pub fn normalize(&mut self) {
        self.id = self.id.trim().to_string();
        self.title = strip_markup(&self.title);
        self.doi = normalize_doi(&self.doi);
        self.pmid = normalize_pmid(&self.pmid);
        self.authors = clean_authors(&self.authors);
        self.journal = self.journal.trim().to_string();
        self.url = self.url.trim().to_string();
        self.abstract_text = self.abstract_text.trim().to_string();
        self.tags = clean_strings(&self.tags);
        self.screening_reason = self.screening_reason.trim().to_string();
        self.source = self.source.trim().to_string();
        for values in self.fields.values_mut() {
            *values = clean_strings(values.iter());
        }
        self.fields.retain(|_, values| !values.is_empty());
        if self.screening.is_empty() { self.screening = "pending".into() }
        if self.reading.is_empty() { self.reading = "unread".into() }
        if self.publication_status.is_empty() { self.publication_status = "unknown".into() }
        if self.source.is_empty() { self.source = "手动添加".into() }
        if self.fulltext.is_empty() { self.fulltext = "missing".into() }
        let now = crate::time::now_iso();
        if self.added_at.is_empty() { self.added_at = now.clone() }
        self.updated_at = now;
    }

    /// What is wrong with this paper, in words a person can act on.
    pub fn validate(&self) -> Result<(), String> {
        if self.title.is_empty() { return Err("请填写文献题目。".into()) }
        if !valid_doi(&self.doi) { return Err(format!("DOI 格式不正确：{}", self.doi)) }
        if !self.pmid.is_empty() && !self.pmid.bytes().all(|b| b.is_ascii_digit()) { return Err(format!("PMID 应为数字：{}", self.pmid)) }
        if let Some(year) = self.year && !(1800..=2200).contains(&year) { return Err("年份应在 1800–2200 之间，或留空。".into()) }
        if !one_of(&SCREENINGS, &self.screening) { return Err(format!("不支持的收录状态：{}", self.screening)) }
        if !one_of(&READINGS, &self.reading) { return Err(format!("不支持的阅读状态：{}", self.reading)) }
        if !one_of(&PUBLICATION_STATUSES, &self.publication_status) { return Err(format!("不支持的发表状态：{}", self.publication_status)) }
        Ok(())
    }

    /// Record a screening decision; pending means nobody has decided.
    pub fn decide(&mut self, status: &str, reason: &str) {
        self.screening = status.to_string();
        self.screening_reason = reason.trim().to_string();
        self.reviewed_at = if status == "pending" { String::new() } else { crate::time::now_iso() };
    }

    /// Where the paper lives on the web: its own URL, else the DOI or PubMed page.
    pub fn link(&self) -> String {
        if self.url.starts_with("http://") || self.url.starts_with("https://") {
            self.url.clone()
        } else if !self.doi.is_empty() {
            format!("https://doi.org/{}", self.doi)
        } else if !self.pmid.is_empty() {
            format!("https://pubmed.ncbi.nlm.nih.gov/{}/", self.pmid)
        } else {
            String::new()
        }
    }

    /// The paper as a sentence cites it: `Zhang et al., 2019`.
    pub fn citation(&self) -> String {
        // `Zhang, Tian-Qi`, `Tian-Qi Zhang` or `Zhang TQ`: the family name either way.
        let names: Vec<String> = author_names(&self.authors).into_iter().map(|(family, _)| family).collect();
        let who = match names.as_slice() {
            [] => self.title.split_whitespace().take(3).collect::<Vec<_>>().join(" "),
            [one] => one.clone(),
            [one, two] => format!("{one} and {two}"),
            [first, ..] => format!("{first} et al."),
        };
        match self.year {
            Some(year) => format!("{who}, {year}"),
            None => who,
        }
    }

    /// "Zhang et al." style short author line.
    pub fn short_authors(&self) -> String {
        let names: Vec<&str> = self.authors.split(';').map(str::trim).filter(|n| !n.is_empty()).collect();
        match names.len() {
            0 => String::new(),
            1 | 2 => names.join("; "),
            _ => format!("{} 等", names[0]),
        }
    }
}

/// Whether a word is a person's initials, as a database writes them after
/// the family name: `M`, `JJ`, `T-Q`. Up to three capitals.
fn is_initials(word: &str) -> bool {
    let letters = word.chars().filter(|c| !matches!(c, '-' | '.')).count();
    (1..=3).contains(&letters) && word.chars().all(|c| c.is_uppercase() || matches!(c, '-' | '.'))
}

/// A name without a comma that ends in initials after other words: `Bono M`, `de Melo AV`.
fn ends_in_initials(name: &str) -> bool {
    let words: Vec<&str> = name.split_whitespace().collect();
    words.len() >= 2 && is_initials(words[words.len() - 1]) && !words[..words.len() - 1].iter().all(|word| is_initials(word))
}

/// The names of `A; B; C`, each as its family name and the rest of it.
/// `Zhang, Tian-Qi` is cut at the comma. Without one the family name is
/// the last word (`Tian-Qi Zhang`), unless the name is written the way
/// Europe PMC writes authors, family name first and initials after it
/// (`Zhang TQ`, `de Melo AV`): a name that ends in one capital is, and one
/// that ends in two or three when most of the list without commas does
/// (so `Wei LI` beside `Tian-Qi Zhang` keeps its family name).
pub fn author_names(authors: &str) -> Vec<(String, String)> {
    let names: Vec<&str> = authors.split(';').map(str::trim).filter(|name| !name.is_empty()).collect();
    let bare = names.iter().filter(|name| !name.contains(',')).count();
    let listed_so = names.iter().filter(|name| !name.contains(',') && ends_in_initials(name)).count() * 2 > bare;
    names
        .into_iter()
        .map(|name| {
            if let Some((family, given)) = name.split_once(',') { return (family.trim().to_string(), given.trim().to_string()) }
            let words: Vec<&str> = name.split_whitespace().collect();
            let Some((last, rest)) = words.split_last() else { return (String::new(), String::new()) };
            if ends_in_initials(name) && (listed_so || last.chars().count() == 1) { (rest.join(" "), last.to_string()) } else { (last.to_string(), rest.join(" ")) }
        })
        .filter(|(family, _)| !family.is_empty())
        .collect()
}

/// `A; B; C` with the names written family name first and initials after
/// it (`Bono M`) given their comma, `Bono, M`: the form every reader of
/// names agrees on. The others stay as they are written.
pub fn family_first(authors: &str) -> String {
    let names: Vec<&str> = authors.split(';').map(str::trim).filter(|name| !name.is_empty()).collect();
    let read = author_names(authors);
    if read.len() != names.len() { return names.join("; ") }
    names.iter().zip(read).map(|(name, (family, given))| if !name.contains(',') && !given.is_empty() && name.starts_with(family.as_str()) && ends_in_initials(name) { format!("{family}, {given}") } else { name.to_string() }).collect::<Vec<_>>().join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names_however_a_name_is_written() {
        let read = |authors: &str| author_names(authors).into_iter().map(|(family, given)| format!("{family}|{given}")).collect::<Vec<_>>();
        assert_eq!(read("Zhang, Tian-Qi; Tian-Qi Zhang; 张伟; Jürgen Müller"), ["Zhang|Tian-Qi", "Zhang|Tian-Qi", "张伟|", "Müller|Jürgen"]);
        // As Europe PMC writes them: the family name first, initials after it.
        assert_eq!(read("Bono M; Marín-Sanz M; Gallo-Franco JJ; de Melo AV"), ["Bono|M", "Marín-Sanz|M", "Gallo-Franco|JJ", "de Melo|AV"]);
        assert_eq!(read("Harris CR; Millman KJ; The NumPy Team"), ["Harris|CR", "Millman|KJ", "Team|The NumPy"]);
        // A family name in capitals among names written given name first stays the family name; one capital never is one.
        assert_eq!(read("Tian-Qi Zhang; Noam Shazeer; Wei LI"), ["Zhang|Tian-Qi", "Shazeer|Noam", "LI|Wei"]);
        assert_eq!(read("Tian-Qi Zhang; Noam Shazeer; Bono M"), ["Zhang|Tian-Qi", "Shazeer|Noam", "Bono|M"]);
        assert_eq!(read("J K Rowling; A B;  ; "), ["Rowling|J K", "B|A"]);
        let paper = |authors: &str| Paper { authors: authors.into(), year: Some(2026), ..Default::default() };
        assert_eq!(paper("Bono M; Marín-Sanz M; Uberegui E").citation(), "Bono et al., 2026");
        assert_eq!(paper("de Melo AV; Costa LC").citation(), "de Melo and Costa, 2026");
        assert_eq!(paper("Wei Zhang").citation(), "Zhang, 2026");
        // Given their comma, so every reader agrees; the rest as written.
        assert_eq!(family_first("Bono M; de Melo AV; Zhang, Wei; NumPy Team"), "Bono, M; de Melo, AV; Zhang, Wei; NumPy Team");
        assert_eq!(family_first("Tian-Qi Zhang; Wei LI"), "Tian-Qi Zhang; Wei LI");
    }

    #[test]
    fn doi_forms() {
        for raw in ["10.1038/S41586-020-2649-2", "https://doi.org/10.1038/s41586-020-2649-2", "http://dx.doi.org/10.1038/s41586-020-2649-2.", "doi: 10.1038/s41586-020-2649-2;", "DOI:10.1038/s41586-020-2649-2"] {
            assert_eq!(normalize_doi(raw), "10.1038/s41586-020-2649-2", "{raw}");
        }
        assert!(valid_doi("10.1038/s41586-020-2649-2"));
        assert!(valid_doi(""));
        assert!(!valid_doi("10.1/x"));
        assert!(!valid_doi("11.1038/x"));
        assert!(!valid_doi("10.1038/a b"));
    }

    #[test]
    fn pmid_forms() {
        assert_eq!(normalize_pmid("PMID: 32939066"), "32939066");
        // A label followed by a space or colon of more than one byte.
        assert_eq!((normalize_doi("DOI\u{a0}10.1038/x"), normalize_doi("doi：10.1038/x"), normalize_pmid("PMID\u{3000}123")), ("10.1038/x".to_string(), "10.1038/x".to_string(), "123".to_string()));
        // A `<` that opens no tag stays, however often the title is normalized.
        assert_eq!((strip_markup("IL-6 < 10 pg/mL in <i>Mus</i>"), strip_markup("Tumours <2 cm: a cohort"), strip_markup("a <b")), ("IL-6 < 10 pg/mL in Mus".to_string(), "Tumours <2 cm: a cohort".to_string(), "a <b".to_string()));
        assert_eq!(normalize_pmid("https://pubmed.ncbi.nlm.nih.gov/32939066/"), "32939066");
    }

    #[test]
    fn title_keys_ignore_markup() {
        assert_eq!(title_key("Single-cell <i>RNA</i> atlas."), title_key("single cell rna atlas"));
    }

    #[test]
    fn entities() {
        assert_eq!(strip_markup("188&#x2009;270 cells &amp; 5&#160;&#956;m, p&lt;0.05 &quot;x&quot; it&#39;s"), "188 270 cells & 5 μm, p<0.05 \"x\" it's");
        // Read once: an escaped entity stays an entity. What is not one stays as written.
        assert_eq!(strip_markup("&amp;lt; R&D; A & B &#xZZ; &unknown; &"), "&lt; R&D; A & B &#xZZ; &unknown; &");
    }

    #[test]
    fn normalize_fills_defaults() {
        let mut p = Paper { title: "  A <b>title</b> ".into(), authors: "A B ;C D;; ".into(), tags: vec!["x".into(), " x ".into(), "".into()], ..Default::default() };
        p.normalize();
        assert_eq!(p.title, "A title");
        assert_eq!(p.authors, "A B; C D");
        assert_eq!(p.tags, vec!["x"]);
        assert_eq!((p.screening.as_str(), p.reading.as_str(), p.fulltext.as_str()), ("pending", "unread", "missing"));
        assert!(p.validate().is_ok());
    }

    #[test]
    fn ids_differ() {
        assert_ne!(new_id("P"), new_id("P"));
    }
}
