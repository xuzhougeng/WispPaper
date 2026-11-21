//! Fill a data folder with two sample libraries, for trying the app out:
//!
//!     cargo run -p wisp_core --example seed -- <data folder> [a.pdf] [more.pdf …] [--supplement s1.pdf …]
//!
//! The first PDF is attached to the first paper of the first library; any
//! further ones become papers of the second library, each with its PDF as
//! full text, so 写作 has sentences to show. PDFs after `--supplement` are
//! the first paper's attachments S1, S2, …

use wisp_core::libraries;
use wisp_core::model::{FieldDef, FieldKind, Paper};
use wisp_core::store::Store;

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let supplements: Vec<std::path::PathBuf> = match args.iter().position(|a| a == "--supplement") {
        Some(at) => args.split_off(at).into_iter().skip(1).map(std::path::PathBuf::from).collect(),
        None => vec![],
    };
    let mut args = args.into_iter();
    let root = std::path::PathBuf::from(args.next().expect("usage: seed <data folder> [a.pdf]"));
    let pdf = args.next().map(std::path::PathBuf::from);
    let more: Vec<std::path::PathBuf> = args.map(std::path::PathBuf::from).collect();
    std::fs::create_dir_all(&root)?;

    let llm = libraries::create(&root, "大语言模型", "Transformer 架构、预训练语言模型及其评测方法；关注注意力机制的改进与长上下文建模。", "violet")?;
    let mut store = Store::open(&llm.dir)?;
    let mut method = FieldDef { name: "方法".into(), kind: FieldKind::Tags, options: vec!["Transformer".into(), "RNN".into(), "预训练".into()], in_table: true, ..Default::default() };
    store.save_field(&mut method)?;
    let mut level = FieldDef { name: "重要程度".into(), kind: FieldKind::Select, options: vec!["必读".into(), "参考".into(), "了解".into()], in_table: true, ..Default::default() };
    store.save_field(&mut level)?;
    let papers = [
        ("Attention Is All You Need", "Vaswani, Ashish; Shazeer, Noam; Parmar, Niki; Uszkoreit, Jakob; Jones, Llion; Gomez, Aidan N.; Kaiser, Lukasz; Polosukhin, Illia", 2017, "Advances in Neural Information Processing Systems", "10.48550/arxiv.1706.03762", "The dominant sequence transduction models are based on complex recurrent or convolutional neural networks that include an encoder and a decoder. We propose a new simple network architecture, the Transformer, based solely on attention mechanisms, dispensing with recurrence and convolutions entirely.", "preprint", vec!["注意力", "架构"], "Transformer", "必读", "read"),
        ("BERT: Pre-training of Deep Bidirectional Transformers for Language Understanding", "Devlin, Jacob; Chang, Ming-Wei; Lee, Kenton; Toutanova, Kristina", 2019, "NAACL-HLT", "10.18653/v1/n19-1423", "We introduce a new language representation model called BERT, which stands for Bidirectional Encoder Representations from Transformers.", "published", vec!["预训练"], "Transformer, 预训练", "必读", "skimmed"),
        ("Long Short-Term Memory", "Hochreiter, Sepp; Schmidhuber, Jürgen", 1997, "Neural Computation", "10.1162/neco.1997.9.8.1735", "Learning to store information over extended time intervals by recurrent backpropagation takes a very long time, mostly because of insufficient, decaying error backflow.", "published", vec!["架构"], "RNN", "参考", "unread"),
        ("Language Models are Few-Shot Learners", "Brown, Tom B.; Mann, Benjamin; Ryder, Nick; Subbiah, Melanie", 2020, "Advances in Neural Information Processing Systems", "", "We show that scaling up language models greatly improves task-agnostic, few-shot performance.", "published", vec!["预训练", "评测"], "Transformer, 预训练", "", "unread"),
        ("一篇只有题目的条目：等待补全信息", "", 0, "", "", "", "", vec![], "", "", "unread"),
    ];
    let mut first = None;
    for (i, (title, authors, year, journal, doi, abstract_text, status, tags, methods, importance, reading)) in papers.into_iter().enumerate() {
        let mut paper = Paper { title: title.into(), authors: authors.into(), year: (year > 0).then_some(year), journal: journal.into(), doi: doi.into(), abstract_text: abstract_text.into(), publication_status: status.into(), tags: tags.into_iter().map(String::from).collect(), reading: reading.into(), starred: i == 0, source: "示例数据".into(), ..Default::default() };
        if !methods.is_empty() { paper.fields.insert(method.id.clone(), methods.split(", ").map(String::from).collect()); }
        if !importance.is_empty() { paper.fields.insert(level.id.clone(), vec![importance.into()]); }
        if i == 0 {
            paper.notes = "编码器和解码器都只用注意力，训练可以并行。\n位置编码用的是正弦函数。".into();
            paper.decide("included", "奠基性工作");
        }
        // Spread the added dates out so "newest first" has an order.
        paper.added_at = format!("2026-11-{:02}T08:00:00.000Z", 10 - i);
        let added = store.add_paper(paper)?;
        if i == 0 { first = Some(added.id) }
    }
    if let (Some(id), Some(pdf)) = (first.as_ref(), pdf) {
        let file = store.import_file(id, &pdf, "fulltext")?;
        store.save_fulltext_pages(&file.id, &wisp_core::fulltext::file_pages(&pdf))?;
    }
    if let Some(id) = &first {
        for path in &supplements { store.import_file(id, path, "supplement")?; }
    }

    let roots = libraries::create(&root, "植物根尖单细胞", "根尖分生组织的单细胞转录组研究：细胞类型图谱、发育轨迹和干细胞微环境。", "green")?;
    let mut store = Store::open(&roots.dir)?;
    for (index, path) in more.iter().enumerate() {
        let pages = wisp_core::fulltext::file_pages(path);
        // The title is the first heading of some length; a file that gives no text keeps its name.
        let stem = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let title = pages.as_ref().ok().and_then(|pages| wisp_core::structure::blocks(pages).into_iter().find(|b| b.heading && b.text.split_whitespace().count() >= 6 && !b.text.to_lowercase().contains("open access"))).map_or(stem.clone(), |b| b.text);
        let record = |title: String| Paper { title, year: Some(2020 + (index % 6) as i32), source: "示例数据".into(), ..Default::default() };
        // Two files with the same first heading are still two papers.
        let paper = store.add_paper(record(title)).or_else(|_| store.add_paper(record(stem)))?;
        let file = store.import_file(&paper.id, path, "fulltext")?;
        store.save_fulltext_pages(&file.id, &pages)?;
    }
    // With PDFs of its own the library needs no sample record (one of them may be this very paper).
    if more.is_empty() { store.add_paper(Paper { title: "A single-cell RNA sequencing profiles the developmental landscape of Arabidopsis root".into(), authors: "Zhang, Tian-Qi; Xu, Zhou-Geng; Shang, Guan-Dong; Wang, Jia-Wei".into(), year: Some(2019), journal: "Molecular Plant".into(), doi: "10.1016/j.molp.2019.04.004".into(), ..Default::default() })?; }
    println!("seeded {}", root.display());
    Ok(())
}
