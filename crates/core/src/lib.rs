//! Wisp Paper's library engine. No GPUI in here: everything can be compiled
//! and tested on its own (`cargo test -p wisp_core`).
//!
//! - `model`    papers, custom field definitions, attachments, normalisation
//! - `libraries` the independent libraries under the data folder
//! - `store`    one library on disk: a SQLite file plus `papers/`
//! - `filter`   search, filters, sorting and the sidebar counts
//! - `import`   BibTeX and RIS readers
//! - `zotero`   read-only local Zotero import, with notes, tags and PDF copies
//! - `export`   BibTeX and CSV writers
//! - `lookup`   Crossref / Europe PMC records → `Paper`
//! - `fulltext` page text of a stored PDF
//! - `pdftext`  that text read off a PDF's glyphs, with the breaks the page shows
//! - `structure` that text sorted into the parts of a paper and cut into sentences, for 写作
//! - `markdown` the Markdown of an answer → blocks to draw
//! - `ai`       context building, request bodies and stream parsing for 文献库提问
//! - `sources`  literature database searches: URLs to ask, records read from the replies
//! - `openaccess` where a paper's full text can be had for free: addresses to ask, candidates read from the replies
//! - `settings` settings.json, and where it and the libraries live
//! - `tools`    what an agent can do with a library: the one table behind the command line, MCP and the assistant
//! - `mcp`      the Model Context Protocol replies for those tools
//! - `update`   which release is the newest, and whether it is newer than this build

pub mod ai;
pub mod complete;
pub mod export;
pub mod filter;
pub mod fulltext;
pub mod import;
pub mod libraries;
pub mod lookup;
pub mod markdown;
pub mod mcp;
pub mod model;
pub mod openaccess;
pub mod pdftext;
pub mod retrieval;
pub mod settings;
pub mod sources;
pub mod store;
pub mod structure;
pub mod time;
pub mod tools;
pub mod update;
pub mod zotero;
