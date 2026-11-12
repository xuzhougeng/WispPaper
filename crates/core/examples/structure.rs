//! What the structuring makes of a file, for looking at a PDF that comes out wrong:
//!
//!     cargo run -p wisp_core --example structure -- a.pdf [blocks|sentences|pages|outline|apply <reply.txt>]
//!
//! Prints the text's language, how many sentences each part got and how
//! each part opens; with `blocks`, every block and its label; with
//! `sentences`, every sentence kept; with `pages`, the page text; with
//! `outline`, what a model is asked when it sorts the parts; with `apply`,
//! the parts a model's answer (in a file) gives.

use wisp_core::structure;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = std::path::PathBuf::from(args.next().expect("usage: structure <a.pdf> [blocks|pages]"));
    let mode = args.next().unwrap_or_default();
    let pages = match wisp_core::fulltext::file_pages(&path) {
        Ok(pages) => pages,
        Err(err) => return println!("{err}"),
    };
    if mode == "pages" {
        for (index, page) in pages.iter().enumerate() {
            println!("=================== page {}\n{page}", index + 1);
        }
        return;
    }
    let blocks = structure::blocks(&pages);
    if mode == "outline" { return print!("{}", structure::outline_prompt("", &blocks)) }
    let labels = match mode.as_str() {
        "apply" => structure::parse_outline(&std::fs::read_to_string(args.next().expect("apply <reply.txt>")).expect("reply"), &blocks, "").expect("outline"),
        _ => structure::assign(&blocks, ""),
    };
    if mode == "blocks" {
        for (index, (block, label)) in blocks.iter().zip(&labels).enumerate() {
            println!("{index:4} p{:<3} {:12} {} {}", block.page, label, if block.heading { "H" } else { "P" }, block.text.chars().take(100).collect::<String>());
        }
        return;
    }
    let sentences = structure::sentences(&blocks, &labels);
    if mode == "sentences" {
        for sentence in &sentences {
            println!("{:12} p{:<3} {}", sentence.section, sentence.page, sentence.text);
        }
        return;
    }
    let (_, sure) = structure::assign_sure(&blocks, "");
    let (open, paragraphs) = (structure::open_paragraphs(&blocks, &sure), structure::prose_paragraphs(&blocks));
    println!("{} pages, {} blocks, {} sentences", pages.len(), blocks.len(), sentences.len());
    println!("language {:?}, {open} of {paragraphs} paragraphs guessed, worth asking a model: {}", structure::body_language(&blocks, &labels).unwrap_or_else(|| structure::language(&pages)), structure::worth_asking(open, paragraphs));
    for (key, name) in structure::SECTIONS {
        let part: Vec<_> = sentences.iter().filter(|s| s.section == key).collect();
        if part.is_empty() { continue }
        println!("\n{name} ({key}): {} sentences", part.len());
        for sentence in part.iter().take(3) {
            println!("  p{} [{}] {}", sentence.page, sentence.heading, sentence.text);
        }
    }
}
