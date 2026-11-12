//! How long one 写作 search takes over a library with a few thousand
//! sentences in a part, built from made-up text:
//!
//!     cargo run --release -p wisp_core --example retrieval_bench -- [papers] [sentences per part]
//!
//! Each search goes through `tools::call`, as the window's does (a new
//! connection every time); the first one also reads and sorts the texts.

use serde_json::json;
use std::time::Instant;
use wisp_core::libraries;
use wisp_core::model::Paper;
use wisp_core::store::Store;

const WORDS: [&str; 48] = [
    "root", "soil", "compaction", "cell", "wall", "gene", "expression", "auxin", "growth", "water", "uptake", "stress", "signal", "pathway", "tissue", "epidermis", "cortex", "meristem", "nitrogen", "phosphate", "hormone", "response", "mutant", "seedling",
    "transcript", "protein", "receptor", "kinase", "drought", "salinity", "elongation", "division", "atlas", "single", "spatial", "lineage", "development", "regulation", "network", "variation", "field", "yield", "rice", "maize", "wheat", "barley", "model", "data",
];

/// A sentence made of the words, the same for the same `seed`.
fn sentence(seed: usize) -> String {
    let mut state = seed as u64 * 6364136223846793005 + 1442695040888963407;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let length = 14 + next() % 14;
    let words: Vec<&str> = (0..length).map(|_| WORDS[next() % WORDS.len()]).collect();
    format!("However, the {} remains unclear in {}.", words.join(" "), WORDS[next() % WORDS.len()])
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1).map(|a| a.parse::<usize>().unwrap_or(0));
    let papers = args.next().filter(|n| *n > 0).unwrap_or(80);
    let per_part = args.next().filter(|n| *n > 0).unwrap_or(4000);
    let each = per_part.div_ceil(papers);
    let root = std::env::temp_dir().join(format!("wisp-bench-{}", std::process::id()));
    let info = libraries::create(&root, "bench", "", "green")?;
    let mut store = Store::open(&info.dir)?;
    for n in 0..papers {
        let paper = store.add_paper(Paper { title: format!("Paper {n}"), authors: "Doe, Jane".into(), year: Some(2020), ..Default::default() })?;
        let part = |offset: usize| (0..each).map(|i| sentence(n * 100_000 + offset + i)).collect::<Vec<_>>().join(" ");
        let body = format!("Introduction\n\n{}\n\nResults\n\n{}\n\nDiscussion\n\n{}", part(0), part(30_000), part(60_000));
        let source = root.join(format!("p{n}.txt"));
        std::fs::write(&source, body)?;
        store.import_file(&paper.id, &source, "fulltext")?;
    }
    drop(store);
    let time = |args: serde_json::Value| -> anyhow::Result<(f64, usize)> {
        let start = Instant::now();
        let found = wisp_core::tools::call(&root, "find_sentences", &args, &|_: &str| Err("offline".to_string()), &|_: &str| Err("offline".to_string())).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok((start.elapsed().as_secs_f64() * 1000., found["sentencesInSection"].as_u64().unwrap_or(0) as usize))
    };
    let (first, count) = time(json!({"section": "introduction"}))?;
    println!("{papers} papers, {count} sentences in the introduction; reading and sorting them: {first:.0} ms");
    for (name, args) in [
        ("one query", json!({"section": "introduction", "query": "root soil compaction remains unclear"})),
        ("one query again", json!({"section": "introduction", "query": "auxin signal pathway in the epidermis"})),
        ("four groups", json!({"section": "introduction", "queries": ["root soil compaction", "remains unclear little is known", "mechanical impedance hard soil", "cell wall epidermis cortex"]})),
        ("four groups, context", json!({"section": "introduction", "context": true, "queries": ["root soil compaction", "remains unclear little is known", "mechanical impedance hard soil", "cell wall epidermis cortex"]})),
    ] {
        let mut times: Vec<f64> = (0..5).map(|_| time(args.clone()).map(|t| t.0)).collect::<anyhow::Result<_>>()?;
        times.sort_by(f64::total_cmp);
        println!("{name}: median {:.1} ms (fastest {:.1}, slowest {:.1})", times[2], times[0], times[4]);
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}
