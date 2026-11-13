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
//! - `extract`  提取: a model's proposals for the custom fields of a paper, each with a sentence of the source
//! - `figure`   图片库: pieces of PDF pages kept as pictures, and the caption a page gives each
//! - `lookup`   Crossref / Europe PMC records → `Paper`
//! - `fulltext` page text of a stored PDF
//! - `pdftext`  that text read off a PDF's glyphs, with the breaks the page shows
//! - `structure` that text sorted into the parts of a paper and cut into sentences, for 写作
//! - `cite`     citations in a manuscript: keys, finding them, the reference list and BibTeX
//! - `docx`     Word files for 写作: read into paragraphs and comments, written again with the text as it stands and its comments
//! - `comment`  批注: remarks on words of a manuscript, the person's, the assistant's and those a Word file came with
//! - `markdown` the Markdown of an answer → blocks to draw
//! - `ai`       context building, request bodies and stream parsing for 文献库提问
//! - `screening` the person's side of 筛选: the keys, a round of papers one by one, suggestions confirmed together
//! - `sources`  literature database searches: URLs to ask, records read from the replies
//! - `openaccess` where a paper's full text can be had for free: addresses to ask, candidates read from the replies
//! - `settings` settings.json, and where it and the libraries live
//! - `tools`    what an agent can do with a library: the one table behind the command line, MCP and the assistant
//! - `mcp`      the Model Context Protocol replies for those tools
//! - `update`   which release is the newest, and whether it is newer than this build

pub mod ai;
pub mod annotation;
pub mod cite;
pub mod comment;
pub mod complete;
pub mod docx;
pub mod export;
pub mod extract;
pub mod figure;
pub mod filter;
pub mod floating;
pub mod fulltext;
pub mod import;
pub mod libraries;
pub mod lookup;
pub mod manuscript;
pub mod markdown;
pub mod mcp;
pub mod model;
pub mod openaccess;
pub mod organise;
pub mod pdftext;
pub mod provider;
pub mod retrieval;
pub mod review;
pub mod screening;
pub mod settings;
pub mod sources;
pub mod store;
pub mod structure;
pub mod time;
pub mod tools;
pub mod translation;
pub mod update;
pub mod zotero;
