//! Wisp Study's library engine. No GPUI in here: everything can be compiled
//! and tested on its own (`cargo test -p wisp_core`).
//!
//! - `model`    papers, custom field definitions, attachments, normalisation
//! - `libraries` the independent libraries under the data folder
//! - `store`    one library on disk: a SQLite file plus `papers/`
//! - `filter`   search, filters, sorting and the sidebar counts
//! - `import`   BibTeX and RIS readers
//! - `zotero`   read-only local Zotero import, with notes, tags and PDF copies
//! - `export`   BibTeX and CSV writers
//! - `archive`  a whole library in one `.wispstudy` file (a gzipped tar) for another Wisp Study, and taking one in
//! - `extract`  提取: a model's proposals for the custom fields of a paper, each with a sentence of the source
//! - `figure`   图片库: pieces of PDF pages kept as pictures, and the caption a page gives each
//! - `lookup`   Crossref / Europe PMC records → `Paper`
//! - `fulltext` page text of a stored PDF
//! - `pdftext`  that text read off a PDF's glyphs, with the breaks the page shows
//! - `structure` that text sorted into the parts of a paper and cut into sentences, for 写作
//! - `cite`     citations in a manuscript: keys, finding them, the reference list and BibTeX
//! - `headings` headings guessed by their look in a Word file without heading styles, for the person to confirm
//! - `pptx`     slides written as a PowerPoint file: what is on a slide is said, where it stands is decided here
//! - `docx`     Word files for 写作: read into paragraphs and comments, written again with the text as it stands and its comments
//! - `comment`  批注: remarks on words of a manuscript, the person's, the assistant's and those a Word file came with
//! - `revise`   改稿: the assistant's changes to a section as blocks of words to find and words to put there
//! - `markdown` the Markdown of an answer → blocks to draw
//! - `ai`       context building, request bodies and stream parsing for 文献库提问
//! - `speech`   朗读: a paper's text in pieces for DeepSeek's speech service, and the sound it sends
//! - `screening` the person's side of 筛选: the keys, a round of papers one by one, suggestions confirmed together
//! - `sources`  literature database searches: URLs to ask, records read from the replies
//! - `openaccess` where a paper's full text can be had for free: addresses to ask, candidates read from the replies
//! - `settings` settings.json, and where it and the libraries live
//! - `tools`    what an agent can do with a library: the one table behind the command line, MCP and the assistant
//! - `mcp`      the Model Context Protocol replies for those tools
//! - `offer`    a card of buttons the assistant puts under its answer, run only when the person presses one
//! - `connector` the person's own MCP servers the assistant may call; `oauth` signing in to one in the browser
//! - `update`   which release is the newest, and whether it is newer than this build

pub mod ai;
pub mod annotation;
pub mod archive;
pub mod cite;
pub mod citefind;
pub mod comment;
pub mod complete;
pub mod compose;
pub mod connector;
pub mod csl;
pub mod docx;
pub mod export;
pub mod extract;
pub mod figure;
pub mod filter;
pub mod floating;
pub mod fulltext;
pub mod headings;
pub mod imagegen;
pub mod import;
pub mod judge;
pub mod libraries;
pub mod lookup;
pub mod manuscript;
pub mod markdown;
pub mod mcp;
pub mod offer;
pub mod model;
pub mod notes;
pub mod oauth;
pub mod openaccess;
pub mod organise;
pub mod passage;
pub mod pptx;
pub mod pdftext;
pub mod provider;
pub mod restyle;
pub mod retrieval;
pub mod review;
pub mod revise;
pub mod screening;
pub mod speech;
pub mod selection;
pub mod settings;
pub mod skill;
pub mod sources;
pub mod store;
pub mod structure;
pub mod time;
pub mod tools;
pub mod translation;
pub mod update;
pub mod webpage;
pub mod zotero;
