//! What the Word reader makes of a file, and what writing it again does:
//!
//!     cargo run -p wisp_core --example docx -- a.docx                  its paragraphs, headings and comments
//!     cargo run -p wisp_core --example docx -- a.docx same out.docx    written again with nothing changed
//!     cargo run -p wisp_core --example docx -- a.docx edit out.docx    every third paragraph changed, one taken out, one put in, comments added
//!     cargo run -p wisp_core --example docx -- a.docx track out.docx   the same, as tracked changes

use wisp_core::docx::{self, Note, Options, Role, Wanted};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else { anyhow::bail!("which file?") };
    let bytes = std::fs::read(path)?;
    let read = docx::read(&bytes)?;
    let Some(mode) = args.get(1) else {
        for (n, para) in read.paragraphs.iter().enumerate() {
            let what = match (para.title, para.level) {
                (true, _) => "title".to_string(),
                (_, Some(level)) => format!("h{level}"),
                _ => String::new(),
            };
            println!("{n:>4} {what:<6}{}", para.text);
        }
        for comment in &read.comments {
            println!("comment by {} ({}) on paragraph {:?} 「{}」: {}", comment.author, comment.date, comment.paragraph, comment.quote, comment.body.replace('\n', " / "));
        }
        println!("{} paragraphs, {} comments, {} tables, {} pictures{}", read.paragraphs.len(), read.comments.len(), read.tables, read.pictures, if read.tracked { ", tracked changes" } else { "" });
        return Ok(());
    };
    let out = args.get(2).map(String::as_str).unwrap_or("out.docx");
    let mut wanted: Vec<Wanted> = read.paragraphs.iter().map(|para| Wanted { text: para.text.clone(), role: if para.title { Role::Title } else if para.level.is_some() { Role::Heading } else { Role::Body }, notes: vec![] }).collect();
    if mode != "same" {
        let author = std::env::var("DOCX_AUTHOR").unwrap_or_else(|_| "Wisp Paper 助手".into());
        let note = |range: std::ops::Range<usize>, body: &str| Note { range, author: author.clone(), date: "2026-11-12T08:00:00Z".into(), body: body.into() };
        let bodies: Vec<usize> = (0..wanted.len()).filter(|n| wanted[*n].role == Role::Body && wanted[*n].text.chars().count() > 30).collect();
        for (k, &n) in bodies.iter().enumerate().filter(|(k, _)| k % 3 == 0) {
            let text = wanted[n].text.clone();
            let cut = text.char_indices().nth(text.chars().count() / 2).map_or(0, |(at, _)| at);
            let end = text.char_indices().nth(text.chars().count() / 2 + 8).map_or(text.len(), |(at, _)| at);
            let put = if k % 2 == 0 { "（这里补充了一句说明）" } else { " [an added remark] " };
            wanted[n].text = format!("{}{put}{}", &text[..cut], &text[end..]);
            wanted[n].notes.push(note(cut..cut + put.len(), "补充"));
        }
        if let Some(&n) = bodies.get(1) {
            let end = wanted[n].text.char_indices().nth(12).map_or(0, |(at, _)| at);
            wanted[n].notes.push(note(0..end, "这一句没有改，只加了批注。\n第二行。"));
        }
        if let Some(&n) = bodies.get(2) { wanted.remove(n); }
        let at = bodies.first().map_or(wanted.len(), |n| n + 1).min(wanted.len());
        wanted.insert(at, Wanted { text: "这是新加的一段。A paragraph that was not in the file.".into(), role: Role::Body, notes: vec![note(0..0, "新增的一段")] });
        wanted.push(Wanted { text: "新的一节".into(), role: Role::Heading, notes: vec![] });
    }
    let done = docx::rewrite(&bytes, &wanted, &Options { track: mode == "track", author: "审阅者".into(), date: "2026-11-12T08:00:00Z".into() })?;
    std::fs::write(out, &done.bytes)?;
    println!("{out}: kept {}, changed {}, added {}, removed {}, comments {}", done.kept, done.changed, done.added, done.removed, done.comments);
    // Read as if every change was accepted, the file says what was wanted.
    let again = docx::read(&done.bytes)?;
    let (got, want): (Vec<&str>, Vec<&str>) = (again.paragraphs.iter().map(|p| p.text.as_str()).collect(), wanted.iter().map(|w| w.text.trim()).collect());
    if got == want { println!("read again: the {} paragraphs wanted, {} comments", got.len(), again.comments.len()) } else {
        println!("READ AGAIN DIFFERS: {} paragraphs, wanted {}", got.len(), want.len());
        for (n, (a, b)) in got.iter().zip(&want).enumerate().filter(|(_, (a, b))| a != b).take(5) { println!("  {n}: got  {a}\n     want {b}") }
    }
    Ok(())
}
