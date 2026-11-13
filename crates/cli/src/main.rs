//! `wisp`: the libraries without the window. Every tool in
//! `wisp_core::tools` is a subcommand that prints JSON, and `wisp mcp` serves
//! the same tools to an AI client over stdin and stdout. It reads the same
//! data folder as the app and can run while the app is open.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use wisp_core::settings::{Settings, default_root, settings_file};
use wisp_core::{mcp, tools};

fn overview() -> String {
    let mut out = String::from(
        "wisp：在命令行里使用 Wisp Paper 的文献库，输出为 JSON。\n\n\
用法：wisp [--data-dir 文件夹] <命令> [参数]\n\n\
命令：\n  tools                列出全部工具及其参数\n  mcp                  在标准输入输出上提供 MCP 服务，供 AI 客户端连接\n  <工具> --help        查看一个工具的参数\n\n工具：\n",
    );
    for tool in tools::catalog() {
        out.push_str(&format!("  {:<20} {}\n", tool.name, tool.description.split('。').next().unwrap_or("")));
    }
    out.push_str("\n不指定 --data-dir 时使用应用设置中的数据文件夹。\n");
    out
}

/// The data folder: the one given, else the one the app uses.
fn data_root(given: Option<PathBuf>) -> Result<PathBuf, String> {
    match given {
        Some(dir) => Ok(dir),
        None => Ok(Settings::load(&settings_file()).map_err(|e| format!("{e:#}"))?.data_root(&default_root())),
    }
}

/// Take `--data-dir DIR` (or `--data-dir=DIR`) out of the words, wherever it stands.
fn take_data_dir(words: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let Some(at) = words.iter().position(|w| w == "--data-dir" || w.starts_with("--data-dir=")) else { return Ok(None) };
    let word = words.remove(at);
    if let Some((_, dir)) = word.split_once('=') { return Ok(Some(PathBuf::from(dir))) }
    if at >= words.len() { return Err("--data-dir 后面需要一个文件夹。".into()) }
    Ok(Some(PathBuf::from(words.remove(at))))
}

/// What one invocation prints. `mcp` is not handled here: it does not end.
fn run(root: &Path, words: &[String]) -> Result<String, String> {
    let Some(command) = words.first() else { return Ok(overview()) };
    let catalog = tools::catalog();
    match command.as_str() {
        "help" | "--help" | "-h" => Ok(overview()),
        "tools" => Ok(pretty(&serde_json::json!(catalog.iter().map(|t| serde_json::json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema})).collect::<Vec<_>>()))),
        name => {
            // `list-papers` is `list_papers`.
            let name = name.replace('-', "_");
            let Some(tool) = catalog.iter().find(|t| t.name == name) else { return Err(format!("没有这个命令：{command}。运行 wisp help 查看可用的命令。")) };
            if words[1..].iter().any(|w| w == "--help" || w == "-h") { return Ok(tools::usage(tool)) }
            let args = tools::cli_args(&tool.input_schema, &words[1..])?;
            tools::call(root, tool.name, &args, &wisp_net::get_text, &wisp_net::download).map(|value| pretty(&value))
        }
    }
}

fn pretty(value: &serde_json::Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// MCP over a pair of streams: one JSON-RPC message per line, each way.
fn serve(root: &Path, server: &mcp::Server, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() { continue }
        let reply = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(message) => mcp::handle(root, &message, server),
            Err(err) => Some(mcp::parse_error(&err.to_string())),
        };
        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

fn main() {
    let mut words: Vec<String> = std::env::args().skip(1).collect();
    // The proxy chosen in the app's settings holds here too; a settings file that cannot be read is reported by `data_root`.
    if let Ok(settings) = Settings::load(&settings_file()) { wisp_net::set_proxy(&settings.proxy) }
    let result = take_data_dir(&mut words).and_then(data_root).and_then(|root| {
        if words.first().map(String::as_str) == Some("mcp") {
            // `wisp mcp --read-only` leaves out the tools that change a library.
            let server = mcp::Server { fetch: &wisp_net::get_text, download: &wisp_net::download, read_only: words.iter().any(|w| w == "--read-only") };
            serve(&root, &server, std::io::stdin().lock(), std::io::stdout().lock()).map(|()| String::new()).map_err(|e| format!("MCP 连接中断：{e}"))
        } else {
            run(&root, &words)
        }
    });
    match result {
        Ok(text) => print!("{text}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use wisp_core::libraries;
    use wisp_core::model::{Paper, new_id};
    use wisp_core::store::Store;

    fn sample() -> (PathBuf, String) {
        let root = std::env::temp_dir().join(format!("wisp-cli-{}", new_id("T")));
        let info = libraries::create(&root, "根", "", "green").unwrap();
        let mut store = Store::open(&info.dir).unwrap();
        let paper = store.add_paper(Paper { title: "Root stem cell niche".into(), ..Default::default() }).unwrap();
        let text = root.join("root.txt");
        std::fs::write(&text, "WOX5 marks the quiescent centre.").unwrap();
        store.import_file(&paper.id, &text, "fulltext").unwrap();
        (root, paper.id)
    }

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(String::from).collect()
    }

    #[test]
    fn commands() {
        let (root, id) = sample();
        let json = |line: &str| serde_json::from_str::<Value>(&run(&root, &words(line)).unwrap()).unwrap();
        assert_eq!(json("list_libraries")["libraries"][0]["name"], "根");
        assert_eq!(json("list-papers --query niche -l 根")["papers"][0]["id"], id.as_str());
        assert_eq!(json(&format!("get_paper {id}"))["title"], "Root stem cell niche");
        assert_eq!(json("search_fulltext wox5")["hits"][0]["page"], 1);
        assert!(json("tools").as_array().unwrap().iter().any(|t| t["name"] == "read_fulltext"));

        assert!(run(&root, &[]).unwrap().contains("search_fulltext"));
        assert!(run(&root, &words("get_paper --help")).unwrap().starts_with("wisp get_paper <id>"));
        assert!(run(&root, &words("get_paper P-none")).unwrap_err().contains("文献不存在"));
        assert!(run(&root, &words("frobnicate")).unwrap_err().contains("wisp help"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn creates_and_imports_from_paths_with_spaces() {
        let root = std::env::temp_dir().join(format!("wisp-cli-local-{}", new_id("T")));
        let invoke = |args: &[&str]| serde_json::from_str::<Value>(&run(&root, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()).unwrap();
        let created = invoke(&["create-library", "--name", "根 单细胞", "--topic", "Root cell atlas", "--color", "green"]);
        let id = created["id"].as_str().unwrap();
        assert_eq!(invoke(&["library_status", "-l", id])["topic"], "Root cell atlas");
        let source = root.join("引用 文献.bib");
        std::fs::write(&source, "@article{root, title={Root cell atlas}, doi={10.1234/root}}").unwrap();
        let args = ["import-references", "-l", id, "--path", source.to_str().unwrap()];
        assert_eq!(invoke(&args)["added"], 1);
        assert_eq!(invoke(&args)["skipped"], 1);
        assert_eq!(invoke(&["list_papers", "-l", id])["total"], 1);
        for name in ["create_library", "import_pdf", "import_references", "import_zotero"] {
            assert!(run(&root, &words(&format!("{name} --help"))).unwrap().starts_with(&format!("wisp {name}")));
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn data_dir_flag() {
        let mut line = words("list_papers --data-dir /x --limit 1");
        assert_eq!((take_data_dir(&mut line).unwrap(), line), (Some(PathBuf::from("/x")), words("list_papers --limit 1")));
        let mut line = words("--data-dir=/y tools");
        assert_eq!((take_data_dir(&mut line).unwrap(), line), (Some(PathBuf::from("/y")), words("tools")));
        assert!(take_data_dir(&mut words("tools --data-dir")).is_err());
        assert_eq!(take_data_dir(&mut words("tools")).unwrap(), None);
        assert_eq!(data_root(Some(PathBuf::from("/z"))).unwrap(), PathBuf::from("/z"));
    }

    #[test]
    fn mcp_over_streams() {
        let (root, id) = sample();
        let input = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}).to_string(),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
            String::new(),
            "{not json".to_string(),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "read_fulltext", "arguments": {"paperId": id}}}).to_string(),
        ]
        .join("\n");
        let mut output = vec![];
        serve(&root, &mcp::Server { fetch: &wisp_net::get_text, download: &wisp_net::download, read_only: false }, input.as_bytes(), &mut output).unwrap();
        let replies: Vec<Value> = String::from_utf8(output).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        // One line per reply; the notification and the blank line get none.
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "wisp-paper");
        assert_eq!(replies[1]["error"]["code"], -32700);
        assert_eq!(replies[2]["result"]["structuredContent"]["pages"][0]["text"], "WOX5 marks the quiescent centre.");
        std::fs::remove_dir_all(root).ok();
    }
}
