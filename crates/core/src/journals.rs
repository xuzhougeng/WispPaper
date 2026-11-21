//! 期刊分区与影响因子: the journal metrics the app carries (`data/journals.tsv`, built by `data/journals.py`
//! from ShowJCR's tables), and finding a paper's journal among them by its name or ISSN.
//!
//! A paper keeps only the journal's name, as its source wrote it: in full from Crossref ("Nature
//! Communications"), abbreviated from PubMed ("Nat Commun", "J. Biol. Chem."). A name is found as it is
//! first, then as an abbreviation of exactly one journal's name. A name that fits several is not guessed.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::OnceLock;

/// The editions in `journals.tsv`, as a reader names them.
pub const EDITIONS: &str = "JCR 2025 · 中科院分区表升级版 2025 · 新锐期刊分区表 2026 · 国际期刊预警名单 2025";
pub const JCR_YEAR: i32 = 2025;
pub const CAS_YEAR: i32 = 2025;
pub const XR_YEAR: i32 = 2026;

/// Which partition the table's 分区 column shows (`Settings::journal_zone`), with its label.
pub const ZONES: [(&str, &str); 3] = [("cas", "中科院"), ("xr", "新锐"), ("jcr", "JCR")];

/// One journal of the table. Empty or `None` where its edition does not list it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    /// The first is the one to show; the others are how the editions write it (one may be Chinese).
    pub names: Vec<String>,
    pub issn: String,
    pub eissn: String,
    /// JCR's journal impact factor.
    pub impact: Option<f32>,
    /// JCR's best quartile among the journal's categories (`Q1`…`Q4`), and that category.
    pub quartile: String,
    pub quartile_category: String,
    /// 中科院分区表升级版: the 大类 zone (1–4), whether it is a Top journal, the 大类.
    pub cas: Option<u8>,
    pub cas_top: bool,
    pub cas_category: String,
    /// 新锐期刊分区表: the same three.
    pub xr: Option<u8>,
    pub xr_top: bool,
    pub xr_category: String,
    /// Why the journal is warned against, when a list does: 国际期刊预警名单 or 新锐's Under Review.
    pub warning: String,
}

impl Journal {
    pub fn name(&self) -> &str {
        self.names.first().map_or("", String::as_str)
    }

    /// The impact factor as JCR writes it: one decimal.
    pub fn impact_text(&self) -> Option<String> {
        self.impact.map(|f| format!("{f:.1}"))
    }

    /// The zone of one partition (`ZONES`), as the 分区 column writes it: `1区 Top`, `Q2`.
    pub fn zone(&self, which: &str) -> Option<String> {
        let zone = |z: Option<u8>, top: bool| z.map(|z| if top { format!("{z}区 Top") } else { format!("{z}区") });
        match which {
            "jcr" => Some(self.quartile.clone()).filter(|q| !q.is_empty()),
            "xr" => zone(self.xr, self.xr_top),
            _ => zone(self.cas, self.cas_top),
        }
    }

    /// The order of the 分区 column: a lower zone first, Top before the rest of its zone.
    pub fn zone_rank(&self, which: &str) -> Option<u8> {
        match which {
            "jcr" => self.quartile.strip_prefix('Q').and_then(|q| q.parse::<u8>().ok()).map(|q| q * 2),
            "xr" => self.xr.map(|z| z * 2 - u8::from(self.xr_top)),
            _ => self.cas.map(|z| z * 2 - u8::from(self.cas_top)),
        }
    }

    /// Every metric in one line, for a reader: `IF 18.1 · JCR Q1 · 中科院 1区 Top · 新锐 1区 Top`.
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        if let Some(impact) = self.impact_text() { parts.push(format!("IF {impact}")) }
        if !self.quartile.is_empty() { parts.push(format!("JCR {}", self.quartile)) }
        for (which, label) in [("cas", "中科院"), ("xr", "新锐")] {
            if let Some(zone) = self.zone(which) { parts.push(format!("{label} {zone}")) }
        }
        parts.join(" · ")
    }
}

/// The table and the ways into it.
pub struct Table {
    pub journals: Vec<Journal>,
    by_key: HashMap<String, usize>,
    by_issn: HashMap<String, usize>,
    /// Each name's words without the small ones, under their initials, for abbreviations.
    by_initials: HashMap<String, Vec<(usize, Vec<String>)>>,
}

static TABLE: OnceLock<Table> = OnceLock::new();

/// The table the app carries, read on first use.
pub fn table() -> &'static Table {
    TABLE.get_or_init(|| Table::read(include_str!("../data/journals.tsv")))
}

/// The journal a paper's journal name is, if the table knows it.
pub fn find(name: &str) -> Option<&'static Journal> {
    table().find(name)
}

/// Lower case, `&` as and, words of letters and digits, no leading "the": `The Plant Cell` and `PLANT CELL` are one.
pub fn key(name: &str) -> String {
    let lower = crate::model::strip_markup(name).to_lowercase().replace('&', " and ");
    let mut words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.first() == Some(&"the") { words.remove(0); }
    words.join(" ")
}

/// Words an abbreviation leaves out: `J Biol Chem` is the Journal *of* Biological Chemistry.
const SMALL: [&str; 17] = ["of", "the", "and", "in", "for", "on", "de", "des", "du", "la", "le", "et", "der", "und", "fur", "für", "y"];

fn long_words(key: &str) -> Vec<String> {
    key.split(' ').filter(|w| !SMALL.contains(w)).map(str::to_string).collect()
}

fn initials(words: &[String]) -> String {
    words.iter().filter_map(|w| w.chars().next()).collect()
}

/// Whether `short` can be an abbreviation of `long`: the same first letter, then its letters in order (`natl` of `national`).
fn shortens(short: &str, long: &str) -> bool {
    let (mut s, mut l) = (short.chars(), long.chars());
    if s.next() != l.next() { return false }
    s.all(|c| l.any(|d| d == c))
}

pub fn issn_key(issn: &str) -> String {
    issn.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_uppercase).collect()
}

impl Table {
    pub fn read(tsv: &str) -> Table {
        let mut table = Table { journals: vec![], by_key: HashMap::new(), by_issn: HashMap::new(), by_initials: HashMap::new() };
        for line in tsv.lines() {
            let cells: Vec<&str> = line.split('\t').collect();
            if cells.len() < 13 { continue }
            let zone = |cell: &str| cell.parse::<u8>().ok().filter(|z| (1..=4).contains(z));
            let journal = Journal {
                names: cells[0].split('|').filter(|n| !n.is_empty()).map(str::to_string).collect(),
                issn: cells[1].into(),
                eissn: cells[2].into(),
                impact: cells[3].parse().ok(),
                quartile: cells[4].into(),
                quartile_category: cells[5].into(),
                cas: zone(cells[6]),
                cas_top: cells[7] == "1",
                cas_category: cells[8].into(),
                xr: zone(cells[9]),
                xr_top: cells[10] == "1",
                xr_category: cells[11].into(),
                warning: cells[12].into(),
            };
            let at = table.journals.len();
            for name in &journal.names {
                let key = key(name);
                if key.is_empty() { continue }
                table.by_key.entry(key.clone()).or_insert(at);
                let words = long_words(&key);
                if words.len() >= 2 { table.by_initials.entry(initials(&words)).or_default().push((at, words)) }
            }
            for issn in [&journal.issn, &journal.eissn] {
                if !issn.is_empty() { table.by_issn.entry(issn_key(issn)).or_insert(at); }
            }
            table.journals.push(journal);
        }
        table
    }

    pub fn by_issn(&self, issn: &str) -> Option<&Journal> {
        self.by_issn.get(&issn_key(issn)).map(|&i| &self.journals[i])
    }

    /// The journal of that name, or the one journal it abbreviates.
    pub fn find(&self, name: &str) -> Option<&Journal> {
        let key = key(name);
        if key.is_empty() { return None }
        if let Some(&i) = self.by_key.get(&key) { return Some(&self.journals[i]) }
        // An abbreviation has two words at least: a word alone fits too many journals.
        let words = long_words(&key);
        if words.len() < 2 { return None }
        let fits: Vec<(usize, bool)> = self.by_initials.get(&initials(&words))?.iter().filter(|(_, long)| long.len() == words.len() && words.iter().zip(long).all(|(s, l)| shortens(s, l))).map(|(i, long)| (*i, words.iter().zip(long).all(|(s, l)| l.starts_with(s.as_str())))).collect();
        let mut fits: Vec<(usize, bool)> = fits.into_iter().fold(vec![], |mut kept, fit| {
            if !kept.iter().any(|(i, _)| *i == fit.0) { kept.push(fit) }
            kept
        });
        // `Nat Commun` cuts each word short, which a name with a letter taken from the middle does not; that one is the likelier.
        if fits.len() > 1 && fits.iter().filter(|(_, prefix)| *prefix).count() == 1 { fits.retain(|(_, prefix)| *prefix) }
        match fits.as_slice() {
            [(i, _)] => Some(&self.journals[*i]),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_reads() {
        let t = table();
        assert!(t.journals.len() > 20_000);
        let nature = find("Nature").unwrap();
        assert!(nature.impact.unwrap() > 10.);
        assert_eq!((nature.quartile.as_str(), nature.cas, nature.cas_top), ("Q1", Some(1), true));
        assert_eq!(t.by_issn("0028-0836").map(Journal::name), Some(nature.name()));
    }

    #[test]
    fn names_as_sources_write_them() {
        let plant = find("PLANT CELL").unwrap();
        assert_eq!(find("The Plant Cell"), Some(plant));
        assert_eq!(find("Plant cell"), Some(plant));
        let ncomms = find("Nature Communications").unwrap();
        assert_eq!(find("Nat Commun"), Some(ncomms));
        assert_eq!(find("Nat. Commun."), Some(ncomms));
        assert_eq!(find("J Biol Chem"), find("Journal of Biological Chemistry"));
        assert!(find("J Biol Chem").is_some());
        let pnas = find("Proc Natl Acad Sci U S A").unwrap();
        assert_eq!(pnas.issn, "0027-8424");
        assert_eq!(find("Protein and Cell"), find("Protein & Cell"));
        // Not a journal, and a word alone is not guessed at.
        assert_eq!(find("bioRxiv preprint server of nothing"), None);
        assert_eq!(find(""), None);
    }

    #[test]
    fn rows_and_zones() {
        let t = Table::read("A Journal|甲刊\t1234-5678\t\t3.25\tQ2\tX\t1\t1\t生物学\t2\t0\t生物学\t国际期刊预警名单 2025\nshort line\n");
        assert_eq!(t.journals.len(), 1);
        let j = t.find("甲刊").unwrap();
        assert_eq!(j.impact_text().as_deref(), Some("3.2"));
        assert_eq!((j.zone("cas").as_deref(), j.zone("xr").as_deref(), j.zone("jcr").as_deref()), (Some("1区 Top"), Some("2区"), Some("Q2")));
        assert_eq!(j.summary(), "IF 3.2 · JCR Q2 · 中科院 1区 Top · 新锐 2区");
        assert!(j.zone_rank("cas") < Journal { cas: Some(1), ..Default::default() }.zone_rank("cas"));
        assert_eq!(t.by_issn("12345678").map(Journal::name), Some("A Journal"));
    }
}
