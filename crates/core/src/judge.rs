//! 判断: a model that writes nothing and decides. TypeSafe calls the kind
//! System One, and its first is Jev: it is given a state (here, a paper and
//! what the library is about) and questions whose possible answers are
//! fixed beforehand, and answers each with one of them and a probability
//! for every one. It is a kind of model of its own (`provider::Model::kind`,
//! `KIND`), beside those that answer in words and those that draw, and it
//! does one thing here: 筛选, where each paper wants one of a few verdicts
//! many times over. It gives no reason and no quotation; what it gives in
//! their place is how sure it is, and a paper it is not sure of is left to
//! the person (`SURE_ENOUGH`). `request` builds the request,
//! `screen_request` the one about a paper, `read_screening` the judgement
//! in the answer. Sending is `wisp_net::judge`.

use crate::ai::{HttpRequest, Judgement, Profile};
use crate::model::Paper;
use serde_json::{Value, json};

/// `provider::Model::kind` of a model that judges.
pub const KIND: &str = "judge";

/// Whether a model's name says it judges: what a model just added is taken for.
pub fn looks_like(model: &str) -> bool {
    let name = model.trim().to_lowercase();
    name == "jev" || name.starts_with("jev-")
}

/// How sure the model has to be of its verdict for it to stand as the
/// suggestion; under it the paper is left as 证据不足, for the person.
pub const SURE_ENOUGH: f64 = 0.6;

/// The most of a paper's text a judgement is asked with, in bytes: these
/// models answer in a fraction of a second because what they read is short.
const BUDGET: usize = 24_000;

/// A request with a state and questions, as the System One interface takes them.
pub fn request(profile: &Profile, state: Value, questions: Value) -> Result<HttpRequest, String> {
    let model = profile.model.trim();
    if model.is_empty() { return Err("判断模型还没有名称。请在「设置 › AI › 模型」里填写。".into()) }
    let key = profile.key();
    if key.is_empty() { return Err("判断模型的服务还没有 API Key。请在「设置 › AI › 模型」里填写，或设置对应的环境变量。".into()) }
    let base = profile.base_url.trim().trim_end_matches('/');
    if !base.starts_with("https://") && !base.starts_with("http://") { return Err("模型服务地址应以 https:// 或 http:// 开头。".into()) }
    Ok(HttpRequest { url: format!("{base}/systemone"), headers: vec![("authorization".to_string(), format!("Bearer {key}")), ("content-type".to_string(), "application/json".to_string())], body: json!({"model": model, "state": state, "questions": questions}), proxy: profile.proxy.trim().to_string() })
}

/// Whether a request could be made at all: the key, the name and the address, checked once before a run.
pub fn ready(profile: &Profile) -> Result<(), String> {
    request(profile, json!(""), json!({})).map(|_| ())
}

/// The names the verdicts go by in the question, with the key of `model::SCREENINGS` each stands for and what it means.
const VERDICTS: [(&str, &str, &str); 3] = [
    ("include", "included", "The paper is about the topic and meets the screening goal: it should be read."),
    ("related", "excluded", "The paper touches the topic or is useful background, but does not meet the screening goal itself."),
    ("irrelevant", "irrelevant", "The paper has nothing to do with the topic."),
];

/// The request that asks which verdict a paper gets, and what it was read
/// from: "abstract", "fulltext" (the first pages, when there is no
/// abstract), or "none" when there is nothing to read and nothing to ask.
pub fn screen_request(profile: &Profile, topic: &str, criteria: &str, paper: &Paper, pages: Option<&[String]>) -> Result<(HttpRequest, &'static str), String> {
    let mut record = json!({"title": paper.title});
    for (name, value) in [("authors", paper.authors.clone()), ("journal", paper.journal.clone()), ("year", paper.year.map(|y| y.to_string()).unwrap_or_default())] {
        if !value.trim().is_empty() { record[name] = json!(value.trim()) }
    }
    let basis = if !paper.abstract_text.trim().is_empty() {
        record["abstract"] = json!(paper.abstract_text.trim());
        "abstract"
    } else {
        // No abstract: the paper's first pages, as far as the budget goes.
        let mut text = String::new();
        for page in pages.unwrap_or(&[]).iter().map(|page| page.trim()).filter(|page| !page.is_empty()) {
            // The room left counts the break between two pages; a page that no whole letter of fits ends it.
            let room = BUDGET.saturating_sub(text.len() + if text.is_empty() { 0 } else { 2 });
            let mut end = page.len().min(room);
            while !page.is_char_boundary(end) { end -= 1 }
            if end == 0 { break }
            if !text.is_empty() { text.push_str("\n\n") }
            text.push_str(&page[..end]);
        }
        if text.is_empty() {
            "none"
        } else {
            record["text"] = json!(text);
            "fulltext"
        }
    };
    let mut state = json!({"paper": record});
    if !topic.trim().is_empty() { state["library_topic"] = json!(topic.trim()) }
    if !criteria.trim().is_empty() { state["screening_goal"] = json!(criteria.trim()) }
    let against = if criteria.trim().is_empty() { "the library's topic" } else { "the screening goal (and the library's topic)" };
    let options: serde_json::Map<String, Value> = VERDICTS.iter().map(|(name, _, meaning)| (name.to_string(), json!(meaning))).collect();
    let questions = json!({"verdict": {"type": "choice", "instructions": format!("A researcher is screening papers for a literature library. Judge the paper against {against}."), "criteria": options}});
    Ok((request(profile, state, questions)?, basis))
}

/// The label a verdict is shown under, for the sentence that stands in place of a reason.
fn label(key: &str) -> &'static str {
    crate::model::SCREENINGS.iter().find(|(known, _)| *known == key).map_or("", |(_, label)| label)
}

/// Read the answer to `screen_request`: the verdict with the highest
/// probability, or 证据不足 when the model is not sure enough of it. The
/// reason is the probabilities themselves, since the model gives none.
pub fn read_screening(reply: &Value) -> Result<Judgement, String> {
    let answer = &reply["answers"]["verdict"];
    let chosen = answer["choice"].as_str().unwrap_or("");
    let Some((_, verdict, _)) = VERDICTS.iter().find(|(name, _, _)| *name == chosen) else { return Err(if chosen.is_empty() { "判断模型的回答里没有结论。".to_string() } else { format!("判断模型给出了不认识的结论：{chosen}") }) };
    let mut shares: Vec<(&str, f64)> = VERDICTS.iter().filter_map(|(name, key, _)| answer["probabilities"][name].as_f64().map(|p| (*key, p.clamp(0., 1.)))).collect();
    shares.sort_by(|a, b| b.1.total_cmp(&a.1));
    // How sure it is: what it says, else the share of the verdict it chose.
    let confidence = answer["confidence"].as_f64().or_else(|| shares.first().map(|(_, share)| *share)).map(|c| c.clamp(0., 1.));
    let spread = shares.iter().map(|(key, share)| format!("{} {:.0}%", label(key), share * 100.)).collect::<Vec<_>>().join("，");
    let said = if spread.is_empty() { format!("判断模型的结论：{}。", label(verdict)) } else { format!("判断模型给出的概率：{spread}。") };
    let sure = confidence.is_none_or(|c| c >= SURE_ENOUGH);
    let reason = if sure { format!("{said}它只作判断，不写理由。") } else { format!("{said}它倾向于「{}」，但把握只有 {:.0}%，留给你判断。", label(verdict), confidence.unwrap_or(0.) * 100.) };
    Ok(Judgement { verdict: if sure { verdict.to_string() } else { "uncertain".into() }, reason, excerpt: String::new(), page: None, confidence })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Protocol;

    fn jev() -> Profile {
        Profile { protocol: Protocol::OpenAI, base_url: "https://api.typesafe.ai/v1/".into(), api_key: "k".into(), model: "jev-latest".into(), kind: KIND.into(), ..Default::default() }
    }

    #[test]
    fn what_is_asked_about_a_paper() {
        let paper = Paper { title: "A single-cell atlas of the root".into(), authors: "Zhang, Wei".into(), year: Some(2021), abstract_text: " We profile 100,000 cells. ".into(), ..Default::default() };
        let pages = vec!["Introduction. Roots grow.".to_string()];
        let (asked, basis) = screen_request(&jev(), "root development", "只要单细胞测序的研究", &paper, Some(&pages)).unwrap();
        assert_eq!((asked.url.as_str(), basis), ("https://api.typesafe.ai/v1/systemone", "abstract"));
        assert_eq!(asked.headers[0], ("authorization".to_string(), "Bearer k".to_string()));
        assert_eq!(asked.body["model"], "jev-latest");
        // The paper's record and abstract, the topic and the goal; the full text stays out when there is an abstract.
        assert_eq!(asked.body["state"], json!({"paper": {"title": "A single-cell atlas of the root", "authors": "Zhang, Wei", "year": "2021", "abstract": "We profile 100,000 cells."}, "library_topic": "root development", "screening_goal": "只要单细胞测序的研究"}));
        let question = &asked.body["questions"]["verdict"];
        assert_eq!((question["type"].as_str(), question["criteria"].as_object().map(|options| { let mut names: Vec<_> = options.keys().cloned().collect(); names.sort(); names })), (Some("choice"), Some(vec!["include".to_string(), "irrelevant".into(), "related".into()])));
        assert!(question["instructions"].as_str().unwrap().contains("screening goal"));
        // Without an abstract the first pages are read, as far as the budget goes; without either, nothing is.
        let bare = Paper { title: "Untitled scan".into(), ..Default::default() };
        let long = vec!["页".repeat(BUDGET), "second page".to_string()];
        let (asked, basis) = screen_request(&jev(), "root development", "", &bare, Some(&long)).unwrap();
        let text = asked.body["state"]["paper"]["text"].as_str().unwrap();
        assert_eq!((basis, text.len() <= BUDGET, text.contains("second page"), asked.body["state"].get("screening_goal")), ("fulltext", true, false, None));
        assert!(asked.body["questions"]["verdict"]["instructions"].as_str().unwrap().contains("the library's topic."));
        assert_eq!(screen_request(&jev(), "t", "", &bare, None).unwrap().1, "none");
        // What cannot be asked says why.
        assert!(ready(&Profile { api_key: String::new(), key_env: "WISP_TEST_NO_SUCH_KEY".into(), ..jev() }).unwrap_err().contains("API Key"));
        assert!(ready(&Profile { model: " ".into(), ..jev() }).unwrap_err().contains("名称"));
        assert_eq!(ready(&jev()), Ok(()));
        assert!(looks_like("jev-latest") && looks_like("Jev") && !looks_like("jevons-1") && !looks_like("gpt-6.1"));
    }

    #[test]
    fn what_is_made_of_the_answer() {
        let sure = json!({"model": "jev-1.13.0", "answers": {"verdict": {"type": "choice", "choice": "include", "confidence": 0.86, "probabilities": {"include": 0.91, "related": 0.07, "irrelevant": 0.02}}}, "usage": {"input_tokens": 300, "output_tokens": 1}});
        let judged = read_screening(&sure).unwrap();
        assert_eq!((judged.verdict.as_str(), judged.confidence, judged.excerpt.as_str(), judged.page), ("included", Some(0.86), "", None));
        assert_eq!(judged.reason, "判断模型给出的概率：已纳入 91%，相关 7%，不相关 2%。它只作判断，不写理由。");
        // "related" is the verdict kept as `excluded`; not sure enough, the paper is left to the person.
        let related = json!({"answers": {"verdict": {"choice": "related", "confidence": 0.7, "probabilities": {"include": 0.2, "related": 0.75, "irrelevant": 0.05}}}});
        assert_eq!(read_screening(&related).unwrap().verdict, "excluded");
        let torn = json!({"answers": {"verdict": {"choice": "irrelevant", "confidence": 0.31, "probabilities": {"include": 0.1, "related": 0.42, "irrelevant": 0.48}}}});
        let judged = read_screening(&torn).unwrap();
        assert_eq!((judged.verdict.as_str(), judged.confidence), ("uncertain", Some(0.31)));
        assert_eq!(judged.reason, "判断模型给出的概率：不相关 48%，相关 42%，已纳入 10%。它倾向于「不相关」，但把握只有 31%，留给你判断。");
        // Without a confidence, the share of what it chose stands for it; without probabilities, the verdict alone.
        let plain = json!({"answers": {"verdict": {"choice": "include", "probabilities": {"include": 0.55, "related": 0.45}}}});
        assert_eq!(read_screening(&plain).map(|j| (j.verdict, j.confidence)), Ok(("uncertain".to_string(), Some(0.55))));
        let bare = json!({"answers": {"verdict": {"choice": "include"}}});
        assert_eq!(read_screening(&bare).map(|j| (j.verdict, j.reason)), Ok(("included".to_string(), "判断模型的结论：已纳入。它只作判断，不写理由。".to_string())));
        assert!(read_screening(&json!({"answers": {}})).unwrap_err().contains("没有结论"));
        assert!(read_screening(&json!({"answers": {"verdict": {"choice": "maybe"}}})).unwrap_err().contains("maybe"));
    }
}
