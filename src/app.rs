//! `Workspace`, the window's root view: the home page of libraries, or one
//! open library. All state lives here; every click becomes a `Cmd` handled
//! by `run`, and the other modules render parts of it.

use crate::input::{InputEvent, TextInput};
use crate::pdf::{PdfEvent, PdfView, Zoom, ZoomTo};
use crate::settings::{SettingsPage, SettingsView, UpdateState};
use crate::shortcuts::{self, Dismiss};
use crate::theme;
use gpui::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use serde_json::Value;
use wisp_core::ai::{self, ChatEvent, Protocol, Scope, ToolCall};
use wisp_core::filter::{self, Filters, View};
use wisp_core::libraries::{self, LibraryInfo};
use wisp_core::lookup::{self, Identifier};
use wisp_core::model::*;
pub(crate) use wisp_core::settings::default_root;
use wisp_core::settings::{Settings, Task, settings_file};
use wisp_core::store::{Conversation, ConversationInfo, ImportBatch, SavedSearch, Store, StructureInfo, Suggestion, TrashEntry};
use wisp_core::manuscript::{Manuscript, Snapshot};
use wisp_core::{complete, export, fulltext, import, pdftext, structure, time, tools, zotero};
pub use crate::manuscripts::ManuscriptExport;

/// What the middle of an open library shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Papers,
    /// 文献库提问: questions over the current list.
    Ask,
    Trash,
    /// 筛选: the papers waiting for a decision, with what the assistant suggests for each.
    Screen,
    /// The guide of a new library: where to start, and searches composed with the assistant.
    Guide,
    /// 检索与导入: saved searches for the literature databases and what they brought in.
    Sources,
    /// 写作: the manuscript, and beside it how the library's papers put the same part.
    Write,
}

/// An edge between two columns of a library that the pointer can hold and move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Between the sidebar and the list.
    Sidebar,
    /// Between the list and the detail panel.
    Detail,
    /// Between a paper's conversation and its PDF.
    Split,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Info,
    Pdf,
    Ask,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterKey {
    Tag,
    Year,
    Reading,
    Screening,
    Fulltext,
    Source,
    Preprint,
    Field(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExportKind {
    Bibtex,
    Csv,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuKind {
    Add,
    Filter(FilterKey),
    Sort,
    Export,
    Reading(String),
    Screening(String),
    LibraryCard(String),
    /// Which model a task uses (设置 › AI).
    TaskModel(Task),
    /// How large the PDF's pages are drawn.
    PdfZoom,
    /// 写作: the library's manuscripts, one section's own, export, and the language.
    Manuscripts,
    Section(String),
    ManuscriptExport,
    WriteLanguage,
    /// The menus a right click opens (`src/context.rs`): on a paper of a list, a paper in the trash, a file
    /// of the selected paper, a tag of the sidebar, a past conversation (of the paper or the library), a
    /// text that can be selected (the whole of it), an answer (of which conversation, and its text).
    Paper(String),
    /// Several papers marked in the list.
    Papers(Vec<String>),
    Trashed(String),
    File(String),
    Tag(String),
    Conversation(bool, String),
    Text(String),
    Answer(bool, String),
}

/// What a person does to a review's record from the conversation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReviewDo {
    Requirements,
    ConfirmRequirements,
    Outline,
    ConfirmOutline,
    /// Keep a text as a section; its citations become rows of evidence.
    Section,
    Checks,
    Complete,
}

/// A text of a review's record that can be changed by hand.
#[derive(Clone, Debug, PartialEq)]
pub enum ReviewText {
    Requirements,
    Outline,
    /// The free notes on the evidence.
    Notes,
    Checks,
    /// A section, by its name.
    Section(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    // Home
    OpenLibrary(String),
    NewLibrary,
    EditLibrary(String),
    AskRemoveLibrary(String),
    RemoveLibrary(String),
    GoHome,
    RevealFolder,
    OpenUrl(String),
    // Settings
    OpenSettings(SettingsPage),
    CloseSettings,
    PickDataDir,
    ResetDataDir,
    RevealDataDir,
    ToggleAutoUpdate,
    CheckUpdate,
    SetAppearance(String),
    SetDensity(String),
    RecordShortcut(String),
    ClearShortcut(String),
    ResetShortcut(String),
    ResetShortcuts,
    PickProtocol(Protocol),
    /// Show another model service's fields on the AI page.
    SelectProfile(usize),
    AddProfile,
    AskRemoveProfile(usize),
    RemoveProfile(usize),
    /// How hard the model on show thinks, a key of `ai::EFFORTS`.
    SetEffort(String),
    /// Bind a task to a profile id; an empty id follows the default.
    SetTaskModel(Task, String),
    SetScreenWorkers(usize),
    ResetTranslationPrompt,
    /// One more slash command of the person's own, on 设置 › 命令.
    AddCommand,
    /// Show or fold up the fields of the n-th.
    SelectCommand(usize),
    AskRemoveCommand(usize),
    RemoveCommand(usize),
    // Collect
    AddById,
    AddManual,
    ImportReferences,
    ImportZotero,
    PickZotero,
    ShowZotero,
    ImportPdfs,
    ImportFolder,
    /// Show what came of each file of the last import.
    ShowImport,
    /// Put the report of the last import away.
    CloseImport,
    /// List the papers whose PDF gave no text.
    ShowTextless,
    /// Open a paper named in the import's report.
    OpenImported(String),
    /// Look for the open full text of one paper, or of every paper of the current list that has no PDF.
    FindPdfs(Option<String>),
    StopFetch,
    ShowFetch,
    CloseFetch,
    // The list
    SetView(View),
    SetPage(Page),
    SetFilter(FilterKey, String),
    ClearFilters,
    SetSort(String),
    Select(String),
    /// What is done with several papers marked in the list (`src/context.rs`): a new conversation about
    /// them alone, starred or not, a reading status, tags added (the dialog), their open full texts
    /// looked for, moved to the trash (after asking).
    AskPapers(Vec<String>),
    StarPapers(Vec<String>, bool),
    ReadPapers(Vec<String>, String),
    TagPapers(Vec<String>),
    FindPdfsOf(Vec<String>),
    AskTrashPapers(Vec<String>),
    TrashPapers(Vec<String>),
    /// Open a paper on one page of the detail panel.
    OpenPaper(String, Tab),
    CloseDetail,
    ToggleStar(String),
    Export(ExportKind),
    // One paper
    SetReading(String, String),
    SetScreening(String, String),
    EditPaper(String),
    TrashPaper(String),
    RestorePaper(String),
    AskDeletePaper(String),
    DeletePaper(String),
    OpenLink(String),
    AttachPdf(String),
    OpenFile(String),
    /// Show a stored file in the file manager.
    RevealFile(String),
    RemoveFile(String),
    /// Try once more to get the text of a stored file.
    RereadFile(String),
    /// Have a model that sees pictures read the pages of a file that gave no text.
    ReadPages(String),
    SetTab(Tab),
    ToggleWide,
    /// Reading mode: the selected paper has the whole window, or the library is back around it.
    ToggleReading,
    /// Put the library's sidebar away, or bring it back.
    ToggleSidebar,
    /// Draw the pages of the PDF on show larger, smaller, or at a given size.
    PdfZoom(ZoomTo),
    /// Cut a piece out of a page as a picture for a question, or stop doing so.
    PdfCut,
    /// Show the paper's PDF to the right of its conversation, or put it away.
    BesidePdf(bool),
    /// The paper's conversation floats over its PDF, or stands beside it (`Settings::floating`).
    FloatChat(bool),
    /// Fold the floating conversation into its button, or open it again.
    FoldChat(bool),
    /// Show the selected paper's title and abstract translated, or stop showing it.
    ToggleTranslation,
    RetryTranslation,
    TranslatePapers(Vec<String>, wisp_core::translation::Field),
    StopBatchTranslation,
    DismissBatchTranslation,
    // Custom fields
    ManageFields,
    EditField(String),
    DeleteField(String),
    MoveField(String, i32),
    ToggleFieldInTable(String),
    AddPreset,
    // Questions
    Ask(bool),
    /// Ask one of the suggested questions.
    Suggest(bool, String),
    NewChat(bool),
    /// Stop the answer being given.
    StopChat(bool),
    /// Show the past conversations, or go back to the one in hand.
    ToggleHistory(bool),
    /// Take up a past conversation again.
    OpenChat(bool, String),
    AskRemoveChat(bool, String),
    RemoveChat(bool, String),
    /// Take the n-th of the slash commands on offer.
    PickSlash(bool, usize),
    /// Choose pictures to go with the next question.
    AttachImages(bool),
    RemoveAttachment(bool, usize),
    /// Take a passage quoted for the next question away again.
    RemoveQuote(bool, usize),
    /// The person's answer to the assistant wanting to change the library.
    AnswerTools(bool, Consent),
    /// Unfold or fold the tool calls that start at this place in the conversation.
    ToggleTools(bool, usize),
    CopyText(String),
    /// Save an answer and its source labels to a new manuscript, at the person's request.
    AnswerToManuscript(String, String),
    /// Start the review record of the conversation with the library (`src/chat.rs`; `/review` does it by itself).
    StartReview,
    ReviewToManuscript,
    /// The review's record in place of the conversation, or the conversation again.
    ToggleReview,
    /// A step of the review with the text it takes (an answer, or what is selected in one; empty for a confirmation).
    Review(ReviewDo, String),
    /// The dialog that changes a text of the review's record.
    EditReviewText(ReviewText),
    AskRemoveReviewSection(String),
    RemoveReviewSection(String),
    /// The dialog for a claim and its source: a row of the record (its id), or a new one for this claim.
    EditReviewEvidence(Option<String>, String),
    PickEvidencePaper(String),
    PickEvidenceSource(String),
    PickEvidenceSection(String),
    /// The person's judgment of a row of evidence; the one it has already takes it back.
    JudgeReviewEvidence(String, wisp_core::review::EvidenceJudgment),
    AskRemoveReviewEvidence(String),
    RemoveReviewEvidence(String),
    /// Copy what is selected in a text of the window that is not an answer (`src/select.rs`).
    CopySelected,
    /// A tag of the library under another name (the dialog), or taken off every paper (after asking).
    RenameTag(String),
    AskRemoveTag(String),
    RemoveTag(String),
    /// The pointer over the text of an answer (by its place in the conversation): selecting it.
    PickText(bool, usize, crate::markdown::Pointer, crate::markdown::Place),
    /// Copy what is selected in an answer; or put it into the question box as a quotation.
    CopyPicked(bool),
    QuotePicked(bool),
    /// Make a note of the selected paper out of an answer about it: the question, and the answer.
    AnswerToNote(String, String),
    /// Unfold or fold the list of the papers the library's questions are put to.
    ToggleMaterials,
    /// Show every source of an answer (by its place in the conversation), or the first few again.
    ToggleSources(bool, usize),
    /// Open a paper; with a page, its PDF there; with a quoted sentence too, that sentence highlighted.
    GotoSource(String, Option<usize>, String),
    /// The same for a citation as an answer writes it: the label of a conversation, a page, a quotation.
    GotoCited(bool, String, Option<usize>, String),
    // Searches and imports
    NewSearch,
    EditSearch(String),
    AskRemoveSearch(String),
    RemoveSearch(String),
    /// Run a saved search without importing, to see what it finds.
    TrySearch(String),
    CloseTrial,
    RunSearch(String),
    RunAllSearches,
    AskUndoImport(String),
    UndoImport(String),
    PickSource(String),
    // The guide
    /// Show the guide; true starts at the choice a new library gets.
    OpenGuide(bool),
    GuideSearch,
    GuideToggleSource(String),
    GuideGenerate,
    GuideToggleIdea(usize),
    GuideImport,
    // Screening
    /// Ask the assistant about the pending papers it has not judged yet.
    ScreenStart,
    /// The assistant asked again about the papers already decided, against the goal as it is now.
    ScreenRecheck,
    ScreenStop,
    /// The person's decision on a paper, made on the screening page.
    ScreenDecide(String, String),
    /// Show only the queue's papers with this suggestion (a key of `SCREENINGS`), those without one (`none`), or all (empty).
    ScreenFilter(String),
    /// Take the assistant's suggestion for every waiting paper it gave this verdict, after asking.
    AskScreenAccept(String),
    ScreenAccept(String),
    /// How often the open library runs its searches on its own, in hours; 0 turns it off.
    SetAutoSync(u32),
    ToggleSearch(String),
    // Writing
    /// Write another section of the manuscript: its id, or a key of `structure::SECTIONS` for the first section of that kind.
    WriteSection(String),
    /// Write another of the library's manuscripts.
    WriteManuscript(String),
    NewManuscript,
    AskRemoveManuscript(String),
    RemoveManuscript(String),
    /// What the manuscript is written in: a code of `manuscript::LANGUAGES`.
    WriteLanguage(String),
    /// Add a section (`None`) or change one's name and kind: the dialog.
    EditSection(Option<String>),
    PickSectionKind(String),
    MoveSection(String, i64),
    AskRemoveSection(String),
    RemoveSection(String),
    /// The whole manuscript to read (true), or one section to write.
    WriteReading(bool),
    ExportManuscript(ManuscriptExport),
    /// The section's earlier versions.
    WriteHistory,
    HistoryPick(usize),
    /// Cite a paper (by id) at the caret: its key, `[@Zhang2024]` (`src/cite.rs`).
    WriteCite(String),
    /// The reference list of the whole manuscript (true) or of the part being written.
    WriteCiteScope(bool),
    /// Put citations in the part being written right: (start, end, what is there, what replaces it), last first.
    WriteCiteFix(Vec<(usize, usize, String, String)>),
    WriteCopyReferences,
    /// The cited papers as BibTeX.
    WriteExportCited,
    /// Have the model sort the full texts into parts (false), or read the pages of the files that gave no text (true).
    WriteRun(bool),
    WriteStop,
    /// Look for candidates: the selected text, or the paragraph at the caret.
    WriteFind,
    /// Only the papers with this tag (empty: any), and only the starred ones.
    WriteOnly(String, bool),
    /// Show or hide the sentences around a candidate, by its card's id.
    WriteUnfold(String),
    /// Have the model work on the selected text or the caret's paragraph; stop it; use what it gave.
    WriteAssist(ai::Assist),
    WriteAssistStop,
    WriteAssistUse(crate::write_assist::Use),
    // Dialogs
    CloseModal,
    SubmitModal,
    /// 按题目补全: look for the records a paper without its metadata may be.
    CompletePaper(String),
    /// Look at one of the records found, or (`None`) go back to all of them.
    CompleteChoose(Option<usize>),
    /// Take or leave one field of the record looked at.
    CompleteField(&'static str),
    PickColor(String),
    PickKind(FieldKind),
    PickOption(String, String),
    ToggleInTable,
}

pub struct MenuItem {
    pub label: SharedString,
    pub checked: bool,
    pub note: Option<SharedString>,
    pub cmd: Cmd,
}

pub struct Menu {
    pub at: Point<Pixels>,
    pub items: Vec<MenuItem>,
    /// The items a line is drawn above.
    pub breaks: Vec<usize>,
    /// The item the arrow keys are on; Enter takes it.
    pub active: Option<usize>,
    /// The paper the menu is about, marked in its list while the menu is open.
    pub paper: Option<String>,
}

pub struct Toast {
    pub message: SharedString,
    pub error: bool,
    pub at: Instant,
}

/// A search the assistant proposed in the guide, with what a trial run of it found.
pub struct GuideIdea {
    pub idea: ai::QueryIdea,
    /// Whether the person wants it saved and run.
    pub chosen: bool,
    pub trial: Option<Result<Value, String>>,
}

/// The guide page of a library: a choice of where to start, then searches
/// composed from a topic for the person to judge.
pub struct Guide {
    /// Still at the first choice (a new library), not yet composing searches.
    pub choosing: bool,
    pub keywords: Entity<TextInput>,
    /// The databases to compose searches for.
    pub kinds: Vec<String>,
    pub busy: bool,
    pub error: String,
    pub ideas: Vec<GuideIdea>,
    /// Bumped with every generation, so answers to an earlier one are dropped.
    round: u64,
}

impl Guide {
    fn new(topic: &str, cx: &mut App) -> Guide {
        let keywords = cx.new(|cx| TextInput::new("研究主题或关键词，中文英文都可以。例如：植物根尖 单细胞转录组 干细胞微环境", cx).multiline(2).with_text(topic));
        Guide { choosing: false, keywords, kinds: vec!["pubmed".into()], busy: false, error: String::new(), ideas: vec![], round: 0 }
    }
}

/// The screening page: what to screen against, and the assistant's run
/// over the papers still waiting for a decision.
pub struct Screen {
    pub criteria: Entity<TextInput>,
    /// Papers the assistant has yet to be asked about in this run, last first.
    todo: Vec<String>,
    /// The papers being read right now, in the order they were started.
    pub reading: Vec<String>,
    pub total: usize,
    pub done: usize,
    /// Papers of the run the assistant gave no usable answer for.
    pub failed: usize,
    /// Failures since the last answer that could be used.
    streak: usize,
    /// The model the run was started with.
    pub model: String,
    /// Which part of the queue is on show: a suggested verdict, `none` for the papers without a suggestion, empty for all.
    filter: String,
    pub error: String,
    /// Bumped when a run starts or stops, so answers to an earlier one are dropped.
    run: u64,
}

impl Screen {
    fn new(criteria: &str, cx: &mut App) -> Screen {
        let criteria = cx.new(|cx| TextInput::new("例如：只收以人类样本做单细胞测序的原始研究；综述、方法学文章不收。留空则按文献库的主题判断。", cx).multiline(3).with_text(criteria));
        Screen { criteria, todo: vec![], reading: vec![], total: 0, done: 0, failed: 0, streak: 0, model: String::new(), filter: String::new(), error: String::new(), run: 0 }
    }

    /// Whether the assistant is working through the papers right now.
    pub fn running(&self) -> bool {
        !self.reading.is_empty() || !self.todo.is_empty()
    }
}

/// 写作's groundwork under way: a model sorting full texts into the parts
/// of a paper, or reading the pages of files that gave no text.
#[derive(Default)]
pub struct Sorting {
    /// The run reads pages; otherwise it sorts parts.
    pub reading: bool,
    /// The files still to do, last first.
    todo: Vec<String>,
    /// The files being worked on.
    pub working: Vec<String>,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    streak: usize,
    pub model: String,
    pub error: String,
    /// The files whose pages were read only in part, longer than `READ_PAGES`: (title, pages read, pages in all).
    pub cut: Vec<(String, usize, usize)>,
    /// Bumped when a run starts or stops, so what still arrives for an earlier one is dropped.
    run: u64,
}

impl Sorting {
    pub fn running(&self) -> bool {
        !self.working.is_empty() || !self.todo.is_empty()
    }
}

/// The pages of one file a model is asked to read in one run: a paper is
/// rarely longer. What is left is read by the next run, from where this one stopped.
pub const READ_PAGES: usize = 40;

/// The writing page: the manuscript and section being written, and the
/// sentences the library's papers have in the same part. `Writing::kind`
/// is that part, `Writing::language` what the manuscript is written in
/// (both in `src/manuscripts.rs`).
pub struct Writing {
    /// The library's manuscripts, oldest first; a section's text is kept here as it is typed, as in the store.
    pub manuscripts: Vec<Manuscript>,
    /// The manuscript being written: its id.
    pub manuscript: String,
    /// The section being written: its id.
    pub section_id: String,
    /// The whole manuscript is shown to be read, instead of one section to write.
    pub reading: bool,
    /// The manuscript's title, edited in place.
    pub title: Entity<TextInput>,
    /// Counts the changes typed, so a pause is the timer of the last one still being the last.
    pub(crate) typed: u64,
    pub editor: Entity<TextInput>,
    /// Words to look for, instead of what is being written.
    pub find: Entity<TextInput>,
    /// The candidates: what was asked for, what was found (write_find.rs).
    pub search: crate::write_find::CandidateSearch,
    /// How each full text was sorted into parts, by file id.
    pub structures: BTreeMap<String, StructureInfo>,
    pub sorting: Sorting,
    pub scroll: ScrollHandle,
    /// The model's work on a paragraph (`write_assist.rs`).
    pub assist: crate::write_assist::Assisting,
}

/// A run of the screening assistant gives up after this many papers in a
/// row without a usable answer: a wrong key or a service that is down
/// would fail every paper the same way.
const SCREEN_GIVE_UP: usize = 3;

/// Where the translation of one text stands.
#[derive(Clone)]
pub enum Translated {
    Busy,
    Done(String),
    Failed(String),
}

/// The steps one question may take. Each is a request to the model; all but
/// the last end in tool calls. With the results before the last one the
/// model is told to answer from what it has (`ai::LAST_STEP_NOTE`).
const MAX_CHAT_STEPS: usize = 24;

/// The pictures one question may carry.
pub const MAX_ATTACHMENTS: usize = 8;
/// Passages of a PDF a question may carry.
pub const MAX_QUOTES: usize = 6;

/// A picture in a conversation: as it is drawn and sent, and the id it is kept under.
#[derive(Clone)]
pub struct ChatImage {
    pub id: String,
    pub image: Arc<Image>,
}

/// The pictures as a request carries them.
fn wire_images(images: &[ChatImage]) -> Vec<ai::ImageData> {
    images.iter().map(|picture| ai::ImageData { media_type: picture.image.format.mime_type().to_string(), data: ai::base64(&picture.image.bytes) }).collect()
}

/// Where a picture to attach comes from: the clipboard, or a file still to be read.
pub enum Picture {
    Bytes(Vec<u8>),
    File(PathBuf),
}

/// What a conversation shows, in order.
pub enum ChatItem {
    /// A question, as typed, with the pictures that went with it, and
    /// what went to the model where that is not the text (a slash command
    /// written out, passages from a PDF put before it).
    User { text: String, images: Vec<ChatImage>, prompt: String },
    /// Text of the model; one answer may come in several pieces with tool calls between them.
    Answer(String),
    /// A tool the model called, and what came back (summary, failed) once it has run.
    Tool { call: String, result: Option<(String, bool)> },
}

/// What the person says to the assistant wanting to change the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consent {
    Once,
    /// This and every later change in the same conversation.
    Always,
    Deny,
}

/// A tool call made ready to run.
pub struct PreparedCall {
    id: String,
    name: String,
    args: Result<Value, String>,
    writes: bool,
    /// The person is asked before it runs (`ToolDef::confirm`).
    pub confirm: bool,
    /// The call as the window words it.
    pub line: String,
}

/// The calls of a turn that wait for the person's consent, and where their lines stand in the conversation.
pub struct PendingTools {
    pub calls: Vec<PreparedCall>,
    first: usize,
}

/// The PDFs of an import read at the same time.
const IMPORT_WORKERS: usize = 3;

/// What became of one PDF of an import.
#[derive(Clone, Debug, PartialEq)]
pub enum Imported {
    Added,
    /// The library already held its paper: the file was attached to that.
    Attached,
    /// Its paper is in the trash: nothing was imported.
    InTrash,
    Failed(String),
}

pub struct ImportRow {
    /// The file's name.
    pub name: String,
    pub outcome: Imported,
    pub paper_id: Option<String>,
    /// No DOI was found in the file: its title and authors are still to be filled in.
    pub unrecognised: bool,
    /// Why no text came of the file; empty when it did.
    pub textless: String,
}

impl ImportRow {
    /// Whether the person still has something to do about this file.
    pub fn open(&self) -> bool {
        self.unrecognised || !self.textless.is_empty() || matches!(self.outcome, Imported::Failed(_) | Imported::InTrash)
    }
}

/// The PDFs dropped or picked in one go (more dropped meanwhile join it):
/// how far the reading is, then what came of each, until it is put away.
#[derive(Default)]
pub struct ImportRun {
    pub total: usize,
    /// Names of the files currently being read; duplicate names are separate jobs.
    pub working: Vec<String>,
    pub rows: Vec<ImportRow>,
}

#[derive(Default)]
pub struct ZoteroRun {
    pub id: String,
    pub running: bool,
    pub total: usize,
    pub current: String,
    pub rows: Vec<zotero::Row>,
    pub other_attachments: usize,
    pub error: String,
}

impl ZoteroRun {
    pub fn summary(&self) -> String {
        if self.running { return format!("正在导入 Zotero · {}/{} · {}", self.rows.len(), self.total, self.current) }
        format!("新增 {} 篇 · 已有 {} 篇 · 回收站 {} 篇 · 失败 {} 篇 · 新增 PDF {} 个 · 待处理 {} 项",
            self.rows.iter().filter(|r| r.added).count(),
            self.rows.iter().filter(|r| !r.added && r.paper_id.is_some()).count(),
            self.rows.iter().filter(|r| r.in_trash).count(),
            self.rows.iter().filter(|r| r.paper_id.is_none() && !r.in_trash).count(),
            self.rows.iter().map(|r| r.files_added).sum::<usize>(),
            self.rows.iter().map(|r| r.problems.len()).sum::<usize>())
    }
}

impl ImportRun {
    pub fn running(&self) -> bool {
        self.rows.len() < self.total
    }

    /// How many files came to this.
    pub fn count(&self, of: impl Fn(&ImportRow) -> bool) -> usize {
        self.rows.iter().filter(|row| of(row)).count()
    }

    /// What came of the files, in a line: 「新增 15 · 附到已有文献 3」.
    pub fn summary(&self) -> String {
        let mut facts = vec![];
        for (n, text) in [
            (self.count(|row| row.outcome == Imported::Added), "新增"),
            (self.count(|row| row.outcome == Imported::Attached), "附到已有文献"),
            (self.count(|row| row.outcome == Imported::InTrash), "在回收站里、未导入"),
            (self.count(|row| matches!(row.outcome, Imported::Failed(_))), "没有成功"),
        ] {
            if n > 0 { facts.push(format!("{text} {n}")) }
        }
        facts.join(" · ")
    }
}

/// What looking for the open full text of one paper came to.
pub struct FetchRow {
    pub paper_id: String,
    pub title: String,
    /// `saved`, `not_found` or `failed`, as `find_pdf` says.
    pub outcome: String,
    /// Where the file came from and what of its text; or why there is none.
    pub detail: String,
    /// The file was saved, and no text came of it.
    pub textless: bool,
}

/// A round of looking for open full texts: the papers still to ask about,
/// those being asked about, and what came of the rest, until it is put away.
#[derive(Default)]
pub struct FetchRun {
    pub total: usize,
    todo: Vec<String>,
    pub working: Vec<String>,
    pub rows: Vec<FetchRow>,
    /// Stop scheduling papers, but let requests already in flight report their results.
    pub stopped: bool,
    /// Bumped when a new round starts, so results from an earlier round are dropped.
    run: u64,
}

impl FetchRun {
    pub fn running(&self) -> bool {
        !self.todo.is_empty() || !self.working.is_empty()
    }

    pub fn count(&self, outcome: &str) -> usize {
        self.rows.iter().filter(|row| row.outcome == outcome).count()
    }

    pub fn remaining(&self) -> usize {
        self.total.saturating_sub(self.rows.len() + self.working.len())
    }

    pub fn status(&self) -> &'static str {
        match (self.stopped, self.running()) {
            (true, true) => "正在停止",
            (true, false) => "已停止",
            (false, true) => "正在查找",
            (false, false) => "查找完成",
        }
    }

    fn stop(&mut self) {
        self.stopped = true;
        self.todo.clear();
    }
}

/// The papers asked about at the same time.
const FETCH_WORKERS: usize = 2;

/// A passage selected in a paper's PDF, to go with the next question.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatQuote {
    pub paper_id: String,
    /// The 1-based page it stands on.
    pub page: usize,
    pub text: String,
}

/// One conversation: with the library (the current list) or with one paper.
pub struct Chat {
    pub input: Entity<TextInput>,
    pub messages: Vec<ChatItem>,
    /// The conversation as the model's protocol carries it: questions, the
    /// model's turns as it sent them, tool results. Only ever appended to.
    wire: Vec<Value>,
    /// The protocol `wire` is written in.
    protocol: Option<Protocol>,
    /// Requests made for the question being answered.
    pub steps: usize,
    /// What the model was given, built with the first question.
    pub ctx: Option<ai::Context>,
    pub busy: bool,
    pub error: String,
    /// Why the last answer ended early, if it did; while one is awaited, what it is waiting for.
    pub note: String,
    pub scroll: ScrollHandle,
    /// The answers (by their place in `messages`) whose sources are all on show.
    pub sources_open: HashSet<usize>,
    /// Bumped by 新对话 and by stopping, so what still arrives for the old run is dropped.
    pub generation: u64,
    /// The conversation in the library's history this is; empty until a question was asked.
    pub id: String,
    /// The conversation as it stands is in the library's history.
    kept: bool,
    created_at: String,
    /// What the labels of a conversation taken up again name, until its context is built anew.
    known: Vec<ai::Source>,
    /// The pictures already kept with the conversation.
    stored_images: HashSet<String>,
    /// Passages selected in a PDF that go with the next question.
    pub quotes: Vec<ChatQuote>,
    /// Pictures that go with the next question, and how many more are still being read.
    pub attachments: Vec<ChatImage>,
    pub attaching: usize,
    /// Which of the slash commands on offer is picked.
    pub slash: usize,
    /// The list of commands was closed with Escape; typing opens it again.
    slash_closed: bool,
    /// The past conversations, while their list is on show.
    pub history: Option<Vec<ConversationInfo>>,
    /// The tool calls waiting for the person's consent.
    pub pending: Option<PendingTools>,
    /// The person allowed every change for the rest of this conversation.
    allow_changes: bool,
    /// The calls running now, which get an answer of their own if the person stops.
    running: Vec<String>,
    /// The groups of tool lines (by the place of their first) that are unfolded.
    pub tools_open: HashSet<usize>,
    /// The list of what the model was (or will be) given is unfolded.
    pub materials_open: bool,
    /// The review this conversation writes, as last read from the library (`Library::load_review`); only the conversation with the library has one.
    pub review: Option<wisp_core::review::Review>,
    /// The quotations of the review's evidence looked up in their papers, by the row's id.
    pub review_checks: HashMap<String, ai::Quoted>,
    /// The review's record is on show in place of the conversation.
    pub review_open: bool,
    /// What is selected in an answer: the answer's place among the messages, the two ends in order of selecting, and whether the pointer is still held.
    pub picked: Option<(usize, crate::markdown::Place, crate::markdown::Place, bool)>,
    /// The quotations of the answers looked up in the papers they name, by label, page and quotation.
    pub checks: HashMap<(String, Option<usize>, String), ai::Quoted>,
    /// When the question being answered was asked.
    pub started: Option<Instant>,
}

/// 按题目补全: what is asked, the records found, and the one being
/// looked at with the fields to take from it. Nothing is written before
/// the person has picked a record and said which fields.
pub struct CompleteForm {
    pub paper_id: String,
    /// When the paper was last changed as the dialog opened: a change made meanwhile stops the writing.
    seen: String,
    pub title: Entity<TextInput>,
    pub author: Entity<TextInput>,
    pub year: Entity<TextInput>,
    /// The paper's title is the name of its file: not something to look for.
    pub placeholder: bool,
    /// The title in the box was read off the first page of the PDF.
    pub guessed: bool,
    pub busy: bool,
    pub found: Option<complete::Found>,
    /// For each record found, the other paper of the library that it is, if any: (id, title).
    pub held: Vec<Option<(String, String)>>,
    pub chosen: Option<usize>,
    pub fields: Vec<&'static str>,
}

pub struct PaperForm {
    /// The paper being edited; `None` adds a new one.
    pub id: Option<String>,
    pub title: Entity<TextInput>,
    pub authors: Entity<TextInput>,
    pub year: Entity<TextInput>,
    pub journal: Entity<TextInput>,
    pub doi: Entity<TextInput>,
    pub pmid: Entity<TextInput>,
    pub url: Entity<TextInput>,
    pub tags: Entity<TextInput>,
    pub abstract_text: Entity<TextInput>,
    /// Text and Tags fields, by field id.
    pub fields: Vec<(String, Entity<TextInput>)>,
    /// The chosen option of each Select field.
    pub selects: BTreeMap<String, String>,
}

pub enum Modal {
    Library { editing: Option<String>, name: Entity<TextInput>, topic: Entity<TextInput>, color: String },
    AddById { input: Entity<TextInput>, busy: bool },
    Paper(Box<PaperForm>),
    Complete(Box<CompleteForm>),
    Fields { editing: Option<String>, name: Entity<TextInput>, options: Entity<TextInput>, kind: FieldKind, in_table: bool },
    Search { editing: Option<String>, kind: String, name: Entity<TextInput>, query: Entity<TextInput>, max: Entity<TextInput> },
    Confirm { title: String, message: String, action: String, cmd: Cmd },
    /// Another name for a tag, on every paper that has it.
    Tag { tag: String, name: Entity<TextInput> },
    /// Tags for several papers at once.
    TagPapers { ids: Vec<String>, name: Entity<TextInput> },
    /// What came of each PDF of the last import (`Library::import`).
    ImportReport,
    Zotero,
    ZoteroReport,
    /// What came of looking for each paper's open full text (`Library::fetching`).
    FetchReport,
    /// An answer about a paper, to be looked over before it joins that paper's notes.
    Note { paper_id: String, text: Entity<TextInput> },
    /// 写作: a section's name and kind (`editing` its id, `None` for a new one).
    Section { editing: Option<String>, name: Entity<TextInput>, kind: String },
    /// 写作: a section's earlier versions, newest first.
    History { section: String, snapshots: Vec<Snapshot>, chosen: usize },
    /// A claim of a review and the source it rests on.
    ReviewEvidence(Box<crate::chat::EvidenceForm>),
    /// A text of a review's record, to change by hand.
    ReviewText { what: ReviewText, input: Entity<TextInput> },
}

pub struct Library {
    pub info: LibraryInfo,
    pub store: Store,
    pub papers: Vec<Paper>,
    pub fields: Vec<FieldDef>,
    pub files: Vec<PaperFile>,
    pub trash: Vec<TrashEntry>,
    pub filters: Filters,
    /// Indices into `papers` of the rows shown, in order.
    pub shown: Vec<usize>,
    pub search: Entity<TextInput>,
    pub page: Page,
    pub selected: Option<String>,
    /// The papers marked in the list with Shift or ⌘ / Ctrl, for doing something with all of them.
    pub marked: Vec<String>,
    /// Where a stretch marked with Shift starts, and where it reaches to.
    pub anchor: Option<String>,
    pub cursor: Option<String>,
    /// The papers the library's next conversation is about, when they were picked in the list; none means the list on show.
    pub asked: Vec<String>,
    pub tab: Tab,
    /// The detail panel takes most of the window (reading a PDF).
    pub wide: bool,
    /// Reading mode: the sidebar and the list are put away and the paper has the whole window.
    pub reading: bool,
    /// The paper's PDF is shown to the right of its conversation, where the panel has room for both.
    pub split: bool,
    /// The floating conversation's window or button while the pointer holds it (floating.rs).
    pub float_drag: Option<crate::floating::Drag>,
    /// The size of the area the floating conversation keeps within, as last drawn.
    pub float_area: (f32, f32),
    pub notes: Entity<TextInput>,
    pub pdf: Option<Entity<PdfView>>,
    pdf_events: Option<Subscription>,
    /// How large PDF pages are drawn; it stays from paper to paper.
    pub zoom: Zoom,
    pub library_chat: Chat,
    pub paper_chat: Chat,
    pub table_scroll: UniformListScrollHandle,
    pub detail_scroll: ScrollHandle,
    /// Files whose text is being extracted.
    pub extracting: HashSet<String>,
    pub guide: Guide,
    pub screen: Screen,
    /// The assistant's suggestion for each paper that has one.
    pub suggestions: HashMap<String, Suggestion>,
    /// Translations asked for in this window, by `store::translation_key`.
    pub translations: HashMap<String, Translated>,
    /// How many requests went out for each of them: the answer to an earlier one, back after a later, is dropped.
    pub translation_asks: HashMap<String, u64>,
    pub translation_run: Option<crate::translate::TranslationRun>,
    pub translation_session: Arc<()>,
    /// Whether the detail panel shows the selected paper translated. It
    /// stays on from paper to paper, so each one opened is translated.
    pub show_translation: bool,
    pub searches: Vec<SavedSearch>,
    /// The latest import batches, newest first.
    pub batches: Vec<ImportBatch>,
    /// The trial run on show: which search, and what it found once it is back.
    pub trial: Option<(String, Option<Result<Value, String>>)>,
    /// Searches being run right now.
    pub running: HashSet<String>,
    /// The PDFs being read in, or what came of the last lot until it is put away.
    pub import: Option<ImportRun>,
    pub zotero: Option<ZoteroRun>,
    /// The round of looking for open full texts, likewise.
    pub fetching: Option<FetchRun>,
    /// Where the fetched files came from, by file: (address, who listed it, version).
    pub origins: BTreeMap<String, (String, String, String)>,
    pub writing: Writing,
    pub citing: crate::cite::Citing,
    _subscriptions: Vec<Subscription>,
}

pub struct Workspace {
    pub focus: FocusHandle,
    pub settings: Settings,
    settings_path: PathBuf,
    pub root: PathBuf,
    pub libraries: Vec<LibraryInfo>,
    pub lib: Option<Library>,
    /// 设置, shown instead of the home page or the library while it is open.
    pub settings_view: Option<SettingsView>,
    pub update: UpdateState,
    /// The version the automatic check last announced, so it says it once.
    pub update_told: Option<String>,
    pub modal: Option<Modal>,
    pub modal_error: String,
    pub modal_subscriptions: Vec<Subscription>,
    pub menu: Option<Menu>,
    pub toast: Option<Toast>,
    pub home_scroll: ScrollHandle,
    /// The edge being dragged: the sidebar's, or the detail panel's.
    pub resizing: Option<Edge>,
    /// What is selected in a text of the window that is not an answer or a text field.
    pub picked: Option<crate::select::Picked>,
}

impl Chat {
    fn new(placeholder: &'static str, cx: &mut App) -> Chat {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(placeholder, cx).multiline(1);
            input.submit_on_enter = true;
            input.accepts_images = true;
            input
        });
        Chat {
            input,
            messages: vec![],
            wire: vec![],
            protocol: None,
            steps: 0,
            ctx: None,
            busy: false,
            error: String::new(),
            note: String::new(),
            scroll: ScrollHandle::new(),
            sources_open: Default::default(),
            generation: 0,
            id: String::new(),
            kept: false,
            created_at: String::new(),
            known: vec![],
            stored_images: Default::default(),
            quotes: vec![],
            attachments: vec![],
            attaching: 0,
            slash: 0,
            slash_closed: false,
            history: None,
            pending: None,
            allow_changes: false,
            running: vec![],
            tools_open: Default::default(),
            materials_open: false,
            review: None,
            review_checks: HashMap::new(),
            review_open: false,
            picked: None,
            checks: HashMap::new(),
            started: None,
        }
    }

    /// What the labels in the answers name.
    pub fn sources(&self) -> &[ai::Source] {
        self.ctx.as_ref().map_or(&self.known, |ctx| &ctx.sources)
    }

    /// The ends of what is selected in an answer, the earlier first.
    pub fn picked_range(&self) -> Option<(usize, crate::markdown::Place, crate::markdown::Place)> {
        let (at, from, to, _) = self.picked?;
        Some(if to < from { (at, to, from) } else { (at, from, to) })
    }

    /// The text selected in an answer.
    pub fn picked_text(&self) -> Option<String> {
        let (at, from, to) = self.picked_range()?;
        let ChatItem::Answer(answer) = self.messages.get(at)? else { return None };
        let text = crate::markdown::text_between(answer, from, to);
        (!text.trim().is_empty()).then_some(text)
    }

    /// The slash commands to offer for what is in the box right now.
    /// `own` are the person's commands from the settings.
    pub fn slash_list(&self, own: &[ai::Command], cx: &App) -> Vec<ai::Offer> {
        if self.slash_closed { vec![] } else { ai::slash_matches(self.input.read(cx).text(), own) }
    }

    /// Have the context built again with the next question. The labels
    /// stay what they were: the answers already given use them.
    fn forget_context(&mut self) {
        if let Some(ctx) = self.ctx.take() { self.known = ctx.sources }
        // The texts may have changed: the quotations are looked up again.
        self.checks.clear();
    }

    /// An answer piece that never got any text leaves no empty bubble behind.
    fn drop_empty_answer(&mut self) {
        if matches!(self.messages.last(), Some(ChatItem::Answer(text)) if text.is_empty()) { self.messages.pop(); }
    }

    /// The question has been answered, or given up on.
    fn end_run(&mut self) {
        self.busy = false;
        self.started = None;
        self.pending = None;
        self.running.clear();
        self.drop_empty_answer();
        if self.note.is_empty() && self.error.is_empty() && !matches!(self.messages.last(), Some(ChatItem::Answer(_))) { self.note = "模型没有返回内容。".into() }
    }

    /// The conversation as it is kept in the library: what was said, and
    /// the papers its answers cite, so the labels still lead somewhere later.
    fn transcript(&self) -> ai::Transcript {
        let mut cited: HashSet<String> = HashSet::new();
        let items = self
            .messages
            .iter()
            .filter_map(|item| match item {
                ChatItem::User { text, images, prompt } => Some(ai::Said::User { text: text.clone(), images: images.iter().map(|picture| picture.id.clone()).collect(), prompt: prompt.clone() }),
                ChatItem::Answer(text) if !text.is_empty() => {
                    cited.extend(ai::citations(text).into_iter().map(|citation| citation.label));
                    Some(ai::Said::Answer { text: text.clone() })
                }
                ChatItem::Answer(_) => None,
                ChatItem::Tool { call, result } => {
                    let (result, failed) = result.clone().unwrap_or_default();
                    Some(ai::Said::Tool { call: call.clone(), result, failed })
                }
            })
            .collect();
        ai::Transcript { items, sources: self.sources().iter().filter(|source| cited.contains(&source.label)).cloned().collect() }
    }

    fn reset(&mut self, cx: &mut App) {
        self.messages.clear();
        self.wire.clear();
        self.protocol = None;
        self.steps = 0;
        self.ctx = None;
        self.busy = false;
        self.error.clear();
        self.note.clear();
        self.sources_open.clear();
        self.generation += 1;
        self.id.clear();
        self.kept = false;
        self.created_at.clear();
        self.known.clear();
        self.stored_images.clear();
        self.quotes.clear();
        self.attachments.clear();
        self.attaching = 0;
        self.slash = 0;
        self.slash_closed = false;
        self.history = None;
        self.pending = None;
        self.allow_changes = false;
        self.running.clear();
        self.tools_open.clear();
        self.review = None;
        self.review_checks.clear();
        self.review_open = false;
        self.picked = None;
        self.checks.clear();
        self.started = None;
        self.input.update(cx, |input, cx| {
            input.choosing = false;
            input.set_text("", cx);
        });
    }
}

impl Library {
    pub fn chat(&self, about_paper: bool) -> &Chat {
        if about_paper { &self.paper_chat } else { &self.library_chat }
    }

    fn chat_mut(&mut self, about_paper: bool) -> &mut Chat {
        if about_paper { &mut self.paper_chat } else { &mut self.library_chat }
    }

    pub fn paper(&self, id: &str) -> Option<&Paper> {
        self.papers.iter().find(|p| p.id == id)
    }

    /// The papers a question to the library is put to: those picked for it in the list, else the list on show.
    pub fn scope_papers(&self) -> Vec<&Paper> {
        if self.asked.is_empty() { return self.shown.iter().map(|&i| &self.papers[i]).collect() }
        let asked: HashSet<&str> = self.asked.iter().map(String::as_str).collect();
        self.papers.iter().filter(|p| asked.contains(p.id.as_str())).collect()
    }

    pub fn selected_paper(&self) -> Option<&Paper> {
        self.selected.as_deref().and_then(|id| self.paper(id))
    }

    pub fn paper_files(&self, id: &str) -> Vec<&PaperFile> {
        self.files.iter().filter(|f| f.paper_id == id).collect()
    }

    /// The stored PDF to preview and read: the first full-text PDF, else any PDF.
    pub fn pdf_file(&self, id: &str) -> Option<&PaperFile> {
        let files = self.paper_files(id);
        files.iter().find(|f| f.kind == "fulltext" && f.format == "pdf").or(files.iter().find(|f| f.format == "pdf")).copied()
    }

    /// The suggestion that counts for a paper: one made against the
    /// screening goal as it is now. An older one is shown, but not acted on.
    pub fn current_suggestion(&self, paper_id: &str) -> Option<&Suggestion> {
        self.suggestions.get(paper_id).filter(|s| s.made_for(&self.info.criteria))
    }

    /// Whether a decided paper is back for another look: asked again
    /// against the goal as it is now, the assistant suggested otherwise,
    /// and the person has not looked at it since.
    fn rechecking(&self, paper: &Paper) -> bool {
        paper.screening != "pending" && self.current_suggestion(&paper.id).is_some_and(|s| s.verdict != paper.screening && s.created_at > paper.reviewed_at)
    }

    /// The papers waiting for a decision, and the decided ones back for
    /// another look: the suggested ones first, by verdict and then by how
    /// sure the assistant was, then those it disagreed with the person on,
    /// those suggested for an earlier goal, and the ones without a suggestion.
    pub fn screen_queue(&self) -> Vec<&Paper> {
        let rank = |p: &Paper| match self.screen_group(&p.id) {
            "differs" => (20, -self.suggestions[&p.id].confidence.unwrap_or(0.)),
            "stale" => (50, 0.),
            "none" => (99, 0.),
            verdict => (SCREENINGS.iter().position(|(key, _)| *key == verdict).unwrap_or(9), -self.suggestions[&p.id].confidence.unwrap_or(0.)),
        };
        let mut queue: Vec<&Paper> = self.papers.iter().filter(|p| p.screening == "pending" || self.rechecking(p)).collect();
        // Stable: within a group the library's own order holds.
        queue.sort_by(|a, b| rank(a).partial_cmp(&rank(b)).unwrap_or(std::cmp::Ordering::Equal));
        queue
    }

    /// Which part of the queue a paper belongs to: its suggested verdict,
    /// `differs` for a decided paper the assistant now sees otherwise,
    /// `stale` for a suggestion made against an earlier goal, or `none`.
    pub fn screen_group(&self, paper_id: &str) -> &str {
        let Some(s) = self.suggestions.get(paper_id) else { return "none" };
        if !s.made_for(&self.info.criteria) { return "stale" }
        if self.paper(paper_id).is_some_and(|p| p.screening != "pending") { return "differs" }
        s.verdict.as_str()
    }

    /// The papers the assistant is still to be asked about against the
    /// goal as it is now: those waiting for a decision, or, with `decided`,
    /// those the person has decided.
    pub fn screen_unasked(&self, decided: bool) -> Vec<&Paper> {
        self.papers.iter().filter(|p| (p.screening != "pending") == decided && self.current_suggestion(&p.id).is_none()).collect()
    }

    /// The part of the queue on show, and its filter. A part that has
    /// emptied (the last of its papers was decided) gives way to the whole queue.
    pub fn screen_view(&self) -> (Vec<&Paper>, &str) {
        let queue = self.screen_queue();
        let filter = self.screen.filter.as_str();
        if filter.is_empty() || !queue.iter().any(|p| self.screen_group(&p.id) == filter) { return (queue, "") }
        (queue.into_iter().filter(|p| self.screen_group(&p.id) == filter).collect(), filter)
    }

    /// A full text was read that could not be before: the next question
    /// is given the library as it is now. An answer on its way keeps what it has.
    fn texts_changed(&mut self) {
        for chat in [&mut self.paper_chat, &mut self.library_chat] {
            if !chat.busy { chat.forget_context() }
        }
    }

    /// Look up, in the papers they name, the quotations of the answers
    /// that have not been looked up yet. An answer still being written is
    /// left until it is whole.
    pub fn check_quotes(&mut self, about_paper: bool) {
        let chat = if about_paper { &self.paper_chat } else { &self.library_chat };
        let finished = if chat.busy { chat.messages.len().saturating_sub(1) } else { chat.messages.len() };
        let mut wanted: Vec<(ai::Citation, String)> = vec![];
        for item in &chat.messages[..finished] {
            let ChatItem::Answer(text) = item else { continue };
            for citation in ai::citations(text) {
                if citation.quote.is_empty() || chat.checks.contains_key(&(citation.label.clone(), citation.page, citation.quote.clone())) { continue }
                let Some(source) = chat.sources().iter().find(|source| source.label == citation.label) else { continue };
                wanted.push((citation, source.paper_id.clone()));
            }
        }
        if wanted.is_empty() { return }
        // Each paper's pages are read once, however often it is quoted.
        let mut texts: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let mut checks = vec![];
        for (citation, paper_id) in wanted {
            let Some(paper) = self.papers.iter().find(|paper| paper.id == paper_id) else { continue };
            let pages = texts.entry(paper_id.clone()).or_insert_with(|| self.store.paper_pages(&paper_id).ok().flatten());
            let found = ai::check_quote(&citation.quote, citation.page, paper, pages.as_deref());
            checks.push(((citation.label, citation.page, citation.quote), found));
        }
        self.chat_mut(about_paper).checks.extend(checks);
    }

    /// Read the review of the conversation with the library again, and look its quotations up in the papers.
    /// Called when the conversation, the record or the papers change, never while drawing.
    pub fn load_review(&mut self) -> anyhow::Result<()> {
        let review = if self.library_chat.id.is_empty() { None } else { self.store.review_for_conversation(&self.library_chat.id)? };
        let mut texts: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let mut checks = HashMap::new();
        for row in review.iter().flat_map(|review| &review.evidence_rows).filter(|row| !row.quote.is_empty()) {
            let Some(paper) = self.papers.iter().find(|paper| paper.id == row.paper_id) else { continue };
            let pages = texts.entry(row.paper_id.clone()).or_insert_with(|| self.store.paper_pages(&row.paper_id).ok().flatten());
            checks.insert(row.id.clone(), ai::check_quote(&row.quote, row.page, paper, pages.as_deref()));
        }
        if review.is_none() { self.library_chat.review_open = false }
        (self.library_chat.review, self.library_chat.review_checks) = (review, checks);
        Ok(())
    }

    pub fn refilter(&mut self) {
        self.shown = filter::filter_papers(&self.papers, &self.filters, time::now_millis());
    }

    /// Read everything again after a change.
    pub fn reload(&mut self) -> anyhow::Result<()> {
        self.papers = self.store.list_papers()?;
        self.fields = self.store.list_fields()?;
        self.files = self.store.list_files()?;
        self.trash = self.store.list_trash()?;
        self.searches = self.store.list_searches()?;
        self.suggestions = self.store.list_suggestions()?.into_iter().map(|s| (s.paper_id.clone(), s)).collect();
        self.batches = self.store.list_batches(30)?;
        self.writing.structures = self.store.structures()?;
        self.origins = self.store.file_origins()?;
        // Filters on a field that no longer exists would hide everything.
        let known: HashSet<&str> = self.fields.iter().map(|f| f.id.as_str()).collect();
        self.filters.fields.retain(|id, _| known.contains(id.as_str()));
        if self.selected.as_ref().is_some_and(|id| self.paper(id).is_none()) {
            self.selected = None;
            self.pdf = None;
        }
        let held: HashSet<&str> = self.papers.iter().map(|p| p.id.as_str()).collect();
        let gone = |ids: &Vec<String>| ids.iter().any(|id| !held.contains(id.as_str()));
        if gone(&self.marked) || gone(&self.asked) {
            let held: HashSet<String> = held.into_iter().map(str::to_string).collect();
            self.marked.retain(|id| held.contains(id));
            self.asked.retain(|id| held.contains(id));
        }
        self.refilter();
        // A paper changed or gone is another source for the review's evidence.
        self.load_review()?;
        Ok(())
    }
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings_path = settings_file();
        let mut toast = None;
        let settings = Settings::load(&settings_path).unwrap_or_else(|err| {
            toast = Some(Toast { message: format!("{err:#}；本次使用默认设置。").into(), error: true, at: Instant::now() });
            Settings::default()
        });
        wisp_net::set_proxy(&settings.proxy);
        let root = settings.data_root(&default_root());
        let _ = std::fs::create_dir_all(&root);
        let focus = cx.focus_handle();
        window.focus(&focus);
        theme::set_accent("blue");
        shortcuts::rebuild(&settings.shortcuts, cx);
        let libraries = libraries::list(&root);
        let mut this = Workspace { focus, settings, settings_path, root, libraries, lib: None, settings_view: None, update: UpdateState::Idle, update_told: None, modal: None, modal_error: String::new(), modal_subscriptions: vec![], menu: None, toast, home_scroll: ScrollHandle::new(), resizing: None, picked: None };
        this.apply_appearance(window);
        cx.observe_window_appearance(window, |this, window, _| this.apply_appearance(window)).detach();
        this.watch_updates(cx);
        this.watch_searches(cx);
        this
    }

    pub fn on(&self, cmd: Cmd, cx: &mut Context<Self>) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _, window, cx| this.run(cmd.clone(), window, cx))
    }

    pub fn on_menu(&self, kind: MenuKind, cx: &mut Context<Self>) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, ev: &ClickEvent, _, cx| this.open_menu(kind.clone(), ev.position(), cx))
    }

    pub fn say(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_toast(message.into(), false, cx);
    }

    pub fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_toast(message.into(), true, cx);
    }

    fn show_toast(&mut self, message: SharedString, error: bool, cx: &mut Context<Self>) {
        let at = Instant::now();
        self.toast = Some(Toast { message, error, at });
        cx.notify();
        // Errors stay longer: they usually need reading twice.
        let seconds = if error { 7 } else { 3 };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(seconds)).await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.at == at) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Run a store operation on the open library, reload, and report a failure.
    pub(crate) fn change(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Library) -> anyhow::Result<()>) -> bool {
        let Some(lib) = self.lib.as_mut() else { return false };
        let result = f(lib).and_then(|_| lib.reload());
        cx.notify();
        match result {
            Ok(()) => true,
            Err(err) => {
                self.fail(format!("{err:#}"), cx);
                false
            }
        }
    }

    pub fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.settings.save(&self.settings_path) {
            self.fail(format!("{err:#}"), cx);
        }
    }

    // ---- menus

    pub(crate) fn open_menu(&mut self, kind: MenuKind, at: Point<Pixels>, cx: &mut Context<Self>) {
        let item = |label: &str, cmd: Cmd| MenuItem { label: label.to_string().into(), checked: false, note: None, cmd };
        let mut items = vec![];
        let mut breaks = vec![];
        let paper = match &kind {
            MenuKind::Paper(id) => Some(id.clone()),
            _ => None,
        };
        match kind {
            MenuKind::Paper(_) | MenuKind::Papers(_) | MenuKind::Trashed(_) | MenuKind::File(_) | MenuKind::Tag(_) | MenuKind::Conversation(..) | MenuKind::Text(_) | MenuKind::Answer(..) => (items, breaks) = self.context_menu(&kind),
            MenuKind::Add => {
                items.push(item("通过 DOI / PMID 添加…", Cmd::AddById));
                items.push(item("导入 PDF 文件…", Cmd::ImportPdfs));
                items.push(item("导入文件夹…", Cmd::ImportFolder));
                items.push(item("导入 BibTeX / RIS…", Cmd::ImportReferences));
                items.push(item("从 Zotero 导入…", Cmd::ImportZotero));
                items.push(item("手动录入…", Cmd::AddManual));
            }
            MenuKind::Manuscripts | MenuKind::Section(_) | MenuKind::ManuscriptExport | MenuKind::WriteLanguage => items = self.manuscript_menu(&kind),
            MenuKind::Export => {
                items.push(item("BibTeX（.bib）", Cmd::Export(ExportKind::Bibtex)));
                items.push(item("表格（.csv）", Cmd::Export(ExportKind::Csv)));
            }
            MenuKind::LibraryCard(id) => {
                items.push(item("打开", Cmd::OpenLibrary(id.clone())));
                items.push(item("编辑名称与主题…", Cmd::EditLibrary(id.clone())));
                items.push(item("移除文献库…", Cmd::AskRemoveLibrary(id)));
            }
            MenuKind::PdfZoom => {
                let Some(current) = self.lib.as_ref().and_then(|lib| lib.pdf.as_ref()).map(|pdf| pdf.read(cx).zoom()) else { return };
                let fits = [("适应宽度", Zoom::FitWidth), ("适应整页", Zoom::FitPage)].map(|(label, zoom)| (label.to_string(), zoom));
                let sizes = [50., 75., 100., 125., 150., 200., 300., 400.].map(|percent| (if percent == 100. { "100%（实际大小）".to_string() } else { format!("{percent}%") }, Zoom::Percent(percent)));
                for (label, zoom) in fits.into_iter().chain(sizes) {
                    items.push(MenuItem { label: label.into(), checked: current == zoom, note: None, cmd: Cmd::PdfZoom(ZoomTo::Set(zoom)) });
                }
            }
            MenuKind::Sort => {
                let Some(lib) = &self.lib else { return };
                let current = if lib.filters.sort.is_empty() { "newest" } else { lib.filters.sort.as_str() };
                for (value, label) in [("newest", "最近添加"), ("year-desc", "发表年份 ↓"), ("year-asc", "发表年份 ↑"), ("title-asc", "题目 A→Z"), ("title-desc", "题目 Z→A"), ("journal-asc", "期刊 A→Z"), ("authors-asc", "作者 A→Z")] {
                    items.push(MenuItem { label: label.into(), checked: current == value, note: None, cmd: Cmd::SetSort(value.into()) });
                }
            }
            MenuKind::Reading(id) => {
                let Some(current) = self.lib.as_ref().and_then(|l| l.paper(&id)).map(|p| p.reading.clone()) else { return };
                for (value, label) in READINGS {
                    items.push(MenuItem { label: label.into(), checked: current == value, note: None, cmd: Cmd::SetReading(id.clone(), value.into()) });
                }
            }
            MenuKind::TaskModel(task) => {
                let bound = self.settings.tasks.get(task).to_string();
                // Questions always have a model: the first one until another is chosen.
                let chosen = if task == Task::Chat { self.settings.profile_for(task).map(|p| p.id.clone()).unwrap_or_default() } else { bound };
                if task != Task::Chat { items.push(MenuItem { label: "与提问相同".into(), checked: chosen.is_empty(), note: None, cmd: Cmd::SetTaskModel(task, String::new()) }) }
                for profile in &self.settings.profiles {
                    items.push(MenuItem { label: profile.title().into(), checked: chosen == profile.id, note: Some(profile.model.clone().into()), cmd: Cmd::SetTaskModel(task, profile.id.clone()) });
                }
            }
            MenuKind::Screening(id) => {
                let Some(current) = self.lib.as_ref().and_then(|l| l.paper(&id)).map(|p| p.screening.clone()) else { return };
                let hints = [("pending", "还没有判断"), ("included", "符合主题，重点阅读"), ("excluded", "可能有用，保留备查"), ("uncertain", "暂时判断不了"), ("irrelevant", "与主题无关，默认隐藏")];
                for (value, label) in SCREENINGS {
                    let hint = hints.iter().find(|(v, _)| *v == value).map(|(_, h)| SharedString::from(*h));
                    items.push(MenuItem { label: label.into(), checked: current == value, note: hint, cmd: Cmd::SetScreening(id.clone(), value.into()) });
                }
            }
            MenuKind::Filter(key) => {
                let Some(lib) = &self.lib else { return };
                let f = &lib.filters;
                let fixed = |table: &[(&str, &str)]| table.iter().map(|(v, l)| (v.to_string(), l.to_string(), None)).collect::<Vec<_>>();
                let counted = |list: Vec<(String, usize)>| list.into_iter().take(40).map(|(v, n)| (v.clone(), v, Some(n))).collect::<Vec<_>>();
                let (current, choices): (String, Vec<(String, String, Option<usize>)>) = match &key {
                    FilterKey::Tag => (f.tag.clone(), counted(filter::tag_counts(&lib.papers))),
                    FilterKey::Year => (f.year.clone(), counted(filter::years(&lib.papers))),
                    FilterKey::Source => (f.source.clone(), counted(filter::source_counts(&lib.papers))),
                    FilterKey::Reading => (f.reading.clone(), fixed(&READINGS)),
                    FilterKey::Screening => (f.screening.clone(), fixed(&SCREENINGS)),
                    FilterKey::Fulltext => (f.fulltext.clone(), fixed(&FULLTEXTS)),
                    FilterKey::Preprint => (f.preprint.clone(), fixed(&[("yes", "只看预印本"), ("no", "不含预印本")])),
                    FilterKey::Field(id) => (f.fields.get(id).cloned().unwrap_or_default(), counted(filter::field_counts(&lib.papers, id))),
                };
                items.push(MenuItem { label: "全部".into(), checked: current.is_empty(), note: None, cmd: Cmd::SetFilter(key.clone(), String::new()) });
                for (value, label, count) in choices {
                    items.push(MenuItem { label: label.into(), checked: current == value, note: count.map(|n| n.to_string().into()), cmd: Cmd::SetFilter(key.clone(), value) });
                }
            }
        }
        if items.is_empty() { return }
        self.menu = Some(Menu { at, items, breaks, active: None, paper });
        cx.notify();
    }

    // ---- commands

    pub fn run(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        // Anything else than a key ends the wait for a shortcut's new key.
        if !matches!(cmd, Cmd::RecordShortcut(_)) { self.stop_recording(cx) }
        cx.notify();
        match cmd {
            Cmd::OpenLibrary(id) => self.open_library(&id, window, cx),
            Cmd::GoHome => {
                self.write_snapshot("leave");
                self.lib = None;
                self.settings_view = None;
                self.modal = None;
                theme::set_accent("blue");
                self.libraries = libraries::list(&self.root);
                window.set_window_title("Wisp Paper");
                window.focus(&self.focus);
            }
            Cmd::NewLibrary => self.library_dialog(None, window, cx),
            Cmd::EditLibrary(id) => self.library_dialog(Some(id), window, cx),
            Cmd::AskRemoveLibrary(id) => {
                let Some(info) = self.libraries.iter().find(|l| l.id == id).or(self.lib.as_ref().map(|l| &l.info)) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm {
                    title: format!("移除文献库「{}」？", info.name),
                    message: format!("文献库会从首页消失。它的文件夹（{} 篇文献、PDF 和笔记）会移到数据目录下的 removed 文件夹，不会被删除，需要时可以手动移回。", info.paper_count),
                    action: "移除".into(),
                    cmd: Cmd::RemoveLibrary(id),
                });
            }
            Cmd::RemoveLibrary(id) => {
                self.modal = None;
                // The database must be closed before its folder can move.
                if self.lib.as_ref().is_some_and(|l| l.info.id == id) { self.lib = None }
                let Some(info) = self.libraries.iter().find(|l| l.id == id).cloned() else { return };
                match libraries::remove(&self.root, &info) {
                    Ok(target) => self.say(format!("已移到 {}", target.display()), cx),
                    Err(err) => self.fail(format!("{err:#}"), cx),
                }
                self.libraries = libraries::list(&self.root);
            }
            Cmd::RevealFolder => {
                let path = self.lib.as_ref().map_or(self.root.clone(), |l| l.info.dir.clone());
                cx.open_with_system(&path);
            }
            Cmd::OpenUrl(url) => cx.open_url(&url),

            Cmd::OpenSettings(page) => self.open_settings(page, window, cx),
            Cmd::CloseSettings => self.close_settings(window, cx),
            Cmd::PickDataDir => self.pick_data_dir(cx),
            Cmd::ResetDataDir => self.set_data_dir(None, cx),
            Cmd::RevealDataDir => cx.open_with_system(&self.root),
            Cmd::ToggleAutoUpdate => {
                self.settings.auto_update = !self.settings.auto_update;
                self.save_settings(cx);
                if self.settings.auto_update { self.check_update(false, cx) }
            }
            Cmd::CheckUpdate => self.check_update(true, cx),
            Cmd::SetAppearance(value) => {
                self.settings.appearance = value;
                self.save_settings(cx);
                self.apply_appearance(window);
            }
            Cmd::SetDensity(value) => {
                self.settings.density = value;
                self.save_settings(cx);
            }
            Cmd::RecordShortcut(id) => self.start_recording(id, window, cx),
            Cmd::ClearShortcut(id) => self.set_shortcut(&id, Some(String::new()), cx),
            Cmd::ResetShortcut(id) => self.set_shortcut(&id, None, cx),
            Cmd::ResetShortcuts => self.reset_shortcuts(cx),
            Cmd::PickProtocol(value) => self.pick_protocol(value, cx),
            Cmd::SelectProfile(index) => self.select_profile(index, cx),
            Cmd::AddProfile => self.add_profile(cx),
            Cmd::AskRemoveProfile(index) => {
                let Some(profile) = self.settings.profiles.get(index) else { return };
                let serves: Vec<&str> = Task::ALL.into_iter().filter(|task| self.settings.tasks.get(*task) == profile.id).map(Task::label).collect();
                let after = if serves.is_empty() { String::new() } else { format!("它现在用于{}，删除后这些任务改用默认的模型。", serves.join("、")) };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: format!("删除模型「{}」？", profile.title()), message: format!("它的地址、API Key 和设置会从 settings.json 里删除。{after}"), action: "删除".into(), cmd: Cmd::RemoveProfile(index) });
            }
            Cmd::RemoveProfile(index) => {
                self.run(Cmd::CloseModal, window, cx);
                self.remove_profile(index, cx);
            }
            Cmd::SetEffort(value) => self.set_effort(value, cx),
            Cmd::SetTaskModel(task, id) => {
                self.settings.tasks.set(task, &id);
                self.save_settings(cx);
            }
            Cmd::SetScreenWorkers(count) => {
                self.settings.screen_workers = count.clamp(1, wisp_core::settings::MAX_SCREEN_WORKERS);
                self.save_settings(cx);
            }
            Cmd::ResetTranslationPrompt => self.reset_translation_prompt(cx),
            Cmd::AddCommand => self.add_command(cx),
            Cmd::SelectCommand(index) => self.select_command(index, cx),
            Cmd::AskRemoveCommand(index) => {
                let Some(command) = self.settings.commands.get(index) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: format!("删除命令 /{}？", command.name), message: "它的模板会从 settings.json 里删除。用过它的对话不受影响：对话里保存着当时发出的内容。".into(), action: "删除".into(), cmd: Cmd::RemoveCommand(index) });
            }
            Cmd::RemoveCommand(index) => {
                self.run(Cmd::CloseModal, window, cx);
                self.remove_command(index, cx);
            }

            Cmd::AddById => {
                let input = self.modal_input("10.1038/s41586-020-2649-2、PMID 或它们的链接", "", false, window, cx);
                input.read(cx).focus(window);
                self.modal_error.clear();
                self.modal = Some(Modal::AddById { input, busy: false });
            }
            Cmd::AddManual => self.paper_dialog(None, window, cx),
            Cmd::EditPaper(id) => self.paper_dialog(Some(id), window, cx),
            Cmd::ImportReferences => self.pick_files("导入", false, cx),
            Cmd::ImportZotero => {
                if self.lib.is_none() { return self.fail("请先打开一个文献库。", cx) }
                self.modal_error.clear();
                self.modal = Some(if self.lib.as_ref().and_then(|l| l.zotero.as_ref()).is_some_and(|r| r.running) { Modal::ZoteroReport } else { Modal::Zotero });
            }
            Cmd::PickZotero => self.pick_zotero(cx),
            Cmd::ShowZotero => {
                self.modal_error.clear();
                self.modal = Some(Modal::ZoteroReport);
            }
            Cmd::ImportPdfs => self.pick_files("导入", true, cx),
            Cmd::ImportFolder => self.pick_folders(cx),
            Cmd::ShowImport => {
                self.modal_subscriptions.clear();
                self.modal_error.clear();
                self.modal = Some(Modal::ImportReport);
            }
            Cmd::CloseImport => {
                if let Some(lib) = self.lib.as_mut() && lib.import.as_ref().is_some_and(|run| !run.running()) { lib.import = None }
                if matches!(self.modal, Some(Modal::ImportReport)) { self.run(Cmd::CloseModal, window, cx) }
            }
            Cmd::OpenImported(id) => {
                self.run(Cmd::CloseModal, window, cx);
                if let Some(lib) = self.lib.as_mut() && matches!(lib.page, Page::Trash | Page::Sources | Page::Guide) { lib.page = Page::Papers }
                self.select(Some(id), window, cx);
            }
            Cmd::FindPdfs(only) => self.fetch_start(only, cx),
            Cmd::StopFetch => {
                if let Some(run) = self.lib.as_mut().and_then(|lib| lib.fetching.as_mut()) {
                    run.stop();
                }
            }
            Cmd::ShowFetch => {
                self.modal_subscriptions.clear();
                self.modal_error.clear();
                self.modal = Some(Modal::FetchReport);
            }
            Cmd::CloseFetch => {
                if let Some(lib) = self.lib.as_mut() && lib.fetching.as_ref().is_some_and(|run| !run.running()) { lib.fetching = None }
                if matches!(self.modal, Some(Modal::FetchReport)) { self.run(Cmd::CloseModal, window, cx) }
            }
            Cmd::ShowTextless => {
                if matches!(self.modal, Some(Modal::ImportReport)) { self.run(Cmd::CloseModal, window, cx) }
                if let Some(lib) = self.lib.as_mut() {
                    lib.filters = Filters { fulltext: "textless".into(), ..Default::default() };
                    lib.search.update(cx, |input, cx| input.set_text("", cx));
                    lib.page = Page::Papers;
                    lib.refilter();
                }
            }

            Cmd::SetView(view) => {
                if let Some(lib) = self.lib.as_mut() {
                    lib.filters.view = view;
                    lib.page = Page::Papers;
                    lib.refilter();
                }
            }
            Cmd::SetPage(page) => {
                if self.lib.as_ref().is_some_and(|l| l.page == Page::Write && page != Page::Write) { self.write_snapshot("leave") }
                if page == Page::Write { self.write_ensure(cx) }
                if let Some(lib) = self.lib.as_mut() {
                    lib.page = page;
                    lib.reading = false;
                    if page == Page::Ask { lib.library_chat.input.read(cx).focus(window) }
                    if page == Page::Write {
                        lib.writing.editor.read(cx).focus(window);
                        lib.writing.structures = lib.store.structures().unwrap_or_default();
                        self.write_texts_changed(cx);
                    }
                }
            }
            Cmd::WriteSection(_) | Cmd::WriteManuscript(_) | Cmd::NewManuscript | Cmd::AskRemoveManuscript(_) | Cmd::RemoveManuscript(_) | Cmd::WriteLanguage(_) | Cmd::EditSection(_) | Cmd::PickSectionKind(_) | Cmd::MoveSection(..) | Cmd::AskRemoveSection(_) | Cmd::RemoveSection(_) | Cmd::WriteReading(_) | Cmd::ExportManuscript(_) | Cmd::WriteHistory | Cmd::HistoryPick(_) => self.run_manuscript(cmd, window, cx),
            Cmd::WriteCite(id) => {
                self.cite_paper(&id, cx);
                if let Some(lib) = self.lib.as_ref() { lib.writing.editor.read(cx).focus(window) }
            }
            Cmd::WriteCiteScope(whole) => {
                if let Some(lib) = self.lib.as_mut() { lib.citing.whole = whole }
            }
            Cmd::WriteCiteFix(pieces) => self.cite_fix(pieces, cx),
            Cmd::WriteCopyReferences => self.copy_references(cx),
            Cmd::WriteExportCited => self.export_cited(cx),
            Cmd::WriteRun(reading) => self.write_run_start(reading, None, cx),
            Cmd::WriteFind => self.write_find(false, cx),
            Cmd::WriteOnly(tag, starred) => self.write_narrow(tag, starred, cx),
            Cmd::WriteUnfold(card) => {
                if let Some(unfolded) = self.lib.as_mut().map(|lib| &mut lib.writing.search.unfolded) && !unfolded.remove(&card) { unfolded.insert(card); }
            }
            Cmd::WriteAssist(assist) => self.assist_start(assist, cx),
            Cmd::WriteAssistStop => self.assist_stop(cx),
            Cmd::WriteAssistUse(how) => self.assist_use(how, window, cx),
            Cmd::WriteStop => {
                if let Some(lib) = self.lib.as_mut() {
                    let sorting = &mut lib.writing.sorting;
                    sorting.run += 1;
                    sorting.todo.clear();
                    sorting.working.clear();
                }
            }
            Cmd::SetFilter(key, value) => {
                if let Some(lib) = self.lib.as_mut() {
                    let f = &mut lib.filters;
                    match key {
                        FilterKey::Tag => f.tag = value,
                        FilterKey::Year => f.year = value,
                        FilterKey::Reading => f.reading = value,
                        FilterKey::Screening => f.screening = value,
                        FilterKey::Fulltext => f.fulltext = value,
                        FilterKey::Source => f.source = value,
                        FilterKey::Preprint => f.preprint = value,
                        FilterKey::Field(id) => {
                            if value.is_empty() { f.fields.remove(&id); } else { f.fields.insert(id, value); }
                        }
                    }
                    lib.page = Page::Papers;
                    lib.refilter();
                }
            }
            Cmd::ClearFilters => {
                if let Some(lib) = self.lib.as_mut() {
                    lib.filters.clear();
                    lib.search.update(cx, |input, cx| input.set_text("", cx));
                    lib.refilter();
                }
            }
            Cmd::SetSort(sort) => {
                if let Some(lib) = self.lib.as_mut() {
                    lib.filters.sort = sort;
                    lib.refilter();
                }
            }
            Cmd::Select(id) => self.select(Some(id), window, cx),
            Cmd::AskPapers(_) | Cmd::StarPapers(..) | Cmd::ReadPapers(..) | Cmd::TagPapers(_) | Cmd::FindPdfsOf(_) | Cmd::AskTrashPapers(_) | Cmd::TrashPapers(_) => self.run_batch(cmd, window, cx),
            Cmd::OpenPaper(id, tab) => {
                self.select(Some(id), window, cx);
                self.run(Cmd::SetTab(tab), window, cx);
            }
            Cmd::CloseDetail => self.select(None, window, cx),
            Cmd::ToggleStar(id) => {
                self.edit_paper(&id, cx, |p| p.starred = !p.starred);
            }
            Cmd::Export(kind) => self.export(kind, cx),

            Cmd::SetReading(id, value) => {
                self.edit_paper(&id, cx, |p| p.reading = value);
            }
            Cmd::SetScreening(id, value) => {
                self.edit_paper(&id, cx, |p| p.decide(&value, ""));
            }
            Cmd::TrashPaper(id) => {
                // The paper on show goes: the one that takes its row is shown next, so a list can be gone through.
                let row = self.lib.as_ref().filter(|lib| lib.selected.as_deref() == Some(id.as_str())).and_then(|lib| lib.shown.iter().position(|&i| lib.papers[i].id == id));
                if self.change(cx, |lib| lib.store.trash_paper(&id)) {
                    let next = row.and_then(|row| self.lib.as_ref().and_then(|lib| lib.shown.get(row).or(lib.shown.last()).map(|&i| lib.papers[i].id.clone())));
                    if next.is_some() { self.select(next, window, cx) }
                    self.say("已移到回收站。", cx)
                }
            }
            Cmd::RestorePaper(id) => {
                if self.change(cx, |lib| lib.store.restore_paper(&id)) { self.say("已恢复。", cx) }
            }
            Cmd::AskDeletePaper(id) => {
                let title = self.lib.as_ref().and_then(|l| l.trash.iter().find(|t| t.paper.id == id)).map(|t| t.paper.title.clone()).unwrap_or_default();
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: "彻底删除这篇文献？".into(), message: format!("「{title}」的记录、笔记和词条会被删除，无法恢复。它的 PDF 会移到文献库文件夹下的 removed 文件夹。"), action: "彻底删除".into(), cmd: Cmd::DeletePaper(id) });
            }
            Cmd::DeletePaper(id) => {
                self.modal = None;
                self.change(cx, |lib| lib.store.delete_paper(&id));
            }
            Cmd::OpenLink(id) => {
                if let Some(link) = self.lib.as_ref().and_then(|l| l.paper(&id)).map(|p| p.link()).filter(|l| !l.is_empty()) {
                    cx.open_url(&link);
                }
            }
            Cmd::AttachPdf(id) => self.pick_attachment(id, cx),
            Cmd::OpenFile(id) => {
                if let Some(lib) = &self.lib && let Some(file) = lib.files.iter().find(|f| f.id == id) {
                    cx.open_with_system(&lib.store.file_path(file));
                }
            }
            Cmd::RevealFile(id) => {
                if let Some(lib) = &self.lib && let Some(file) = lib.files.iter().find(|f| f.id == id) {
                    cx.reveal_path(&lib.store.file_path(file));
                }
            }
            Cmd::RemoveFile(id) => {
                if self.change(cx, |lib| lib.store.remove_file(&id)) {
                    if let Some(lib) = self.lib.as_mut() { lib.pdf = None }
                    self.say("附件已移除（文件移到了 removed 文件夹）。", cx);
                }
            }
            Cmd::RereadFile(id) => self.reread_file(id, cx),
            Cmd::ReadPages(id) => self.write_run_start(true, Some(id), cx),
            Cmd::SetTab(tab) => {
                // Without a model the question box is not drawn: the keyboard stays with the window, so Esc is still heard.
                let asking = !self.settings.profile_for_or_default(Task::Chat).key().is_empty();
                if let Some(lib) = self.lib.as_mut() {
                    lib.tab = tab;
                    if tab == Tab::Ask && asking { lib.paper_chat.input.read(cx).focus(window) } else if tab == Tab::Ask { window.focus(&self.focus) }
                }
                self.ensure_pdf(cx);
            }
            Cmd::ToggleWide => {
                if let Some(lib) = self.lib.as_mut() { lib.wide = !lib.wide }
            }
            Cmd::ToggleReading => {
                if let Some(lib) = self.lib.as_mut().filter(|lib| lib.selected.is_some()) {
                    lib.reading = !lib.reading;
                    // Reading starts at the paper itself: the PDF, when there is one and the record was on show.
                    if lib.reading && lib.tab == Tab::Info && lib.selected.clone().is_some_and(|id| lib.pdf_file(&id).is_some()) { lib.tab = Tab::Pdf }
                }
                self.ensure_pdf(cx);
            }
            Cmd::ToggleSidebar => {
                self.settings.sidebar_hidden = !self.settings.sidebar_hidden;
                self.save_settings(cx);
            }
            Cmd::PdfCut => {
                if let Some(pdf) = self.lib.as_ref().and_then(|lib| lib.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.toggle_cutting(cx)) }
            }
            Cmd::BesidePdf(show) => {
                if let Some(lib) = self.lib.as_mut() { lib.split = show }
                // A panel too narrow for the two is widened; a window too small for that keeps the conversation.
                if show && self.detail_width(window) < crate::detail::SPLIT_MIN && let Some(lib) = self.lib.as_mut() { lib.wide = true }
                self.ensure_pdf(cx);
            }
            Cmd::FloatChat(on) => self.float_chat(on, window, cx),
            Cmd::FoldChat(fold) => self.fold_chat(fold, window, cx),
            Cmd::PdfZoom(to) => {
                if let Some(pdf) = self.lib.as_ref().filter(|lib| lib.tab == Tab::Pdf || lib.split).and_then(|lib| lib.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.zoom_to(to, cx)) }
            }
            Cmd::ToggleTranslation => {
                if let Some(lib) = self.lib.as_mut() { lib.show_translation = !lib.show_translation }
                self.load_title_translations();
                self.translate_selected(true, cx);
            }
            Cmd::RetryTranslation => self.translate_selected(true, cx),
            Cmd::TranslatePapers(ids, field) => self.translate_batch(ids, field, cx),
            Cmd::StopBatchTranslation => {
                if let Some(run) = self.lib.as_mut().and_then(|lib| lib.translation_run.as_mut()) { run.batch.stop() }
            }
            Cmd::DismissBatchTranslation => {
                if let Some(lib) = self.lib.as_mut().filter(|lib| lib.translation_run.as_ref().is_some_and(|run| !run.batch.running())) { lib.translation_run = None }
            }

            Cmd::ManageFields => self.fields_dialog(None, window, cx),
            Cmd::EditField(id) => self.fields_dialog(Some(id), window, cx),
            Cmd::DeleteField(id) => {
                self.change(cx, |lib| lib.store.delete_field(&id));
                self.fields_dialog(None, window, cx);
            }
            Cmd::MoveField(id, step) => {
                self.change(cx, |lib| lib.store.move_field(&id, step));
            }
            Cmd::ToggleFieldInTable(id) => {
                self.change(cx, |lib| {
                    let Some(mut field) = lib.fields.iter().find(|f| f.id == id).cloned() else { return Ok(()) };
                    field.in_table = !field.in_table;
                    lib.store.save_field(&mut field)
                });
            }
            Cmd::AddPreset => {
                self.change(cx, |lib| {
                    // BiblioCell's single-cell fields, as a starting point.
                    for (name, kind, options) in [("物种", FieldKind::Tags, vec![]), ("组织", FieldKind::Tags, vec![]), ("测序类型", FieldKind::Tags, vec!["scRNA-seq", "snRNA-seq", "scATAC-seq", "Multiome", "空间转录组"]), ("测序平台", FieldKind::Tags, vec!["10x Genomics Chromium", "BD Rhapsody", "Smart-seq2"])] {
                        if lib.fields.iter().any(|f| f.name == name) { continue }
                        lib.store.save_field(&mut FieldDef { name: name.into(), kind, options: options.into_iter().map(String::from).collect(), in_table: name == "物种", ..Default::default() })?;
                    }
                    Ok(())
                });
            }

            Cmd::Ask(paper) => self.ask(paper, cx),
            Cmd::Suggest(paper, question) => {
                if let Some(lib) = self.lib.as_ref() {
                    let chat = if paper { &lib.paper_chat } else { &lib.library_chat };
                    chat.input.update(cx, |input, cx| input.set_text(&question, cx));
                }
                self.ask(paper, cx);
            }
            Cmd::NewChat(paper) => {
                // An answer still arriving is stopped first, so that what there is of it is kept.
                self.stop_chat(paper, cx);
                // A conversation whose last save failed is tried once more; if that fails too, it says so itself.
                if self.lib.as_ref().is_some_and(|lib| !lib.chat(paper).kept) { self.save_chat(paper, cx) }
                let mut kept = false;
                if let Some(lib) = self.lib.as_mut() {
                    let chat = if paper { &mut lib.paper_chat } else { &mut lib.library_chat };
                    kept = chat.kept;
                    chat.reset(cx);
                    chat.input.read(cx).focus(window);
                    // A new conversation with the library is about the list on show again.
                    if !paper { lib.asked.clear() }
                }
                if kept { self.say("上一段对话已保存，在「历史」里可以找到。", cx) }
            }
            Cmd::StopChat(paper) => self.stop_chat(paper, cx),
            Cmd::ToggleHistory(paper) => {
                if let Some(lib) = self.lib.as_mut() {
                    let about = if paper { lib.selected.clone().unwrap_or_default() } else { String::new() };
                    let listed = if lib.chat(paper).history.is_some() { Ok(None) } else { lib.store.list_conversations(&about).map(Some) };
                    match listed {
                        Ok(list) => lib.chat_mut(paper).history = list,
                        Err(err) => self.fail(format!("{err:#}"), cx),
                    }
                }
            }
            Cmd::OpenChat(paper, id) => {
                self.stop_chat(paper, cx);
                self.open_chat(paper, &id, cx);
                if let Some(lib) = self.lib.as_ref() { lib.chat(paper).input.read(cx).focus(window) }
            }
            Cmd::AskRemoveChat(paper, id) => {
                let Some(title) = self.lib.as_ref().and_then(|l| l.chat(paper).history.as_ref()?.iter().find(|c| c.id == id).map(|c| c.title.clone())) else { return };
                // A review is reached through its conversation alone, so it goes with it: say so first.
                let reviewed = self.lib.as_ref().is_some_and(|l| l.store.review_for_conversation(&id).is_ok_and(|review| review.is_some()));
                self.modal_error.clear();
                let warning = if reviewed { "\n\n这次对话的综述记录（写作要求、大纲、正文和证据）会一起删除；已经存为稿件的内容不受影响。" } else { "" };
                self.modal = Some(Modal::Confirm { title: "删除这次对话？".into(), message: format!("「{title}」连同其中的图片会从这个文献库的历史对话里删除，不能恢复。文献库里的文献不受影响。{warning}"), action: "删除".into(), cmd: Cmd::RemoveChat(paper, id) });
            }
            Cmd::RemoveChat(paper, id) => {
                if let Some(lib) = self.lib.as_mut() {
                    let about = if paper { lib.selected.clone().unwrap_or_default() } else { String::new() };
                    let result = lib.store.remove_conversation(&id).and_then(|()| lib.store.list_conversations(&about));
                    let chat = lib.chat_mut(paper);
                    // The conversation in hand, once removed, would only be saved again with the next answer.
                    if chat.id == id {
                        let listed = chat.history.take();
                        chat.reset(cx);
                        chat.history = listed;
                    }
                    match result {
                        Ok(list) => chat.history = Some(list),
                        Err(err) => self.fail(format!("{err:#}"), cx),
                    }
                }
                self.run(Cmd::CloseModal, window, cx);
            }
            Cmd::PickSlash(paper, at) => {
                self.pick_slash(paper, at, cx);
                if let Some(lib) = self.lib.as_ref() { lib.chat(paper).input.read(cx).focus(window) }
            }
            Cmd::AttachImages(paper) => {
                let extensions: Vec<SharedString> = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"].into_iter().map(SharedString::from).collect();
                let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("添加图片".into()), extensions: Some(extensions) });
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(Some(paths))) = picked.await {
                        this.update(cx, |this, cx| this.attach_images(paper, paths.into_iter().map(Picture::File).collect(), cx)).ok();
                    }
                })
                .detach();
            }
            Cmd::RemoveAttachment(paper, at) => {
                if let Some(lib) = self.lib.as_mut() {
                    let chat = lib.chat_mut(paper);
                    if at < chat.attachments.len() { chat.attachments.remove(at); }
                }
            }
            Cmd::RemoveQuote(paper, at) => {
                if let Some(lib) = self.lib.as_mut() {
                    let chat = lib.chat_mut(paper);
                    if at < chat.quotes.len() { chat.quotes.remove(at); }
                }
            }
            Cmd::AnswerTools(paper, consent) => {
                let Some(pending) = self.lib.as_mut().and_then(|lib| {
                    let chat = lib.chat_mut(paper);
                    if consent == Consent::Always { chat.allow_changes = true }
                    chat.pending.take()
                }) else {
                    return;
                };
                self.execute_tools(paper, pending.calls, pending.first, consent != Consent::Deny, cx);
            }
            Cmd::ToggleTools(paper, at) => {
                if let Some(lib) = self.lib.as_mut() {
                    let chat = lib.chat_mut(paper);
                    if !chat.tools_open.remove(&at) { chat.tools_open.insert(at); }
                }
            }
            Cmd::AnswerToManuscript(question, answer) => self.answer_to_manuscript(question, answer, window, cx),
            Cmd::StartReview => self.start_review(cx),
            Cmd::ReviewToManuscript => self.review_to_manuscript(window, cx),
            Cmd::ToggleReview => {
                if let Some(lib) = self.lib.as_mut() { lib.library_chat.review_open = !lib.library_chat.review_open && lib.library_chat.review.is_some() }
            }
            Cmd::Review(what, text) => self.review_do(what, text, cx),
            Cmd::EditReviewText(what) => self.edit_review_text(what, window, cx),
            Cmd::AskRemoveReviewSection(name) => {
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: format!("删除综述记录中的「{name}」？"), message: "这一节会从综述记录中移除，它的引用带来的、你还没有判断过的证据也一起移除。已创建的稿件不受影响。".into(), action: "删除".into(), cmd: Cmd::RemoveReviewSection(name) });
            }
            Cmd::RemoveReviewSection(name) => {
                self.review_apply(wisp_core::review::ReviewAction::RemoveSection { name }, "这一节已从综述记录中移除", cx);
                self.run(Cmd::CloseModal, window, cx);
            }
            Cmd::EditReviewEvidence(id, claim) => self.edit_review_evidence(id, claim, window, cx),
            Cmd::PickEvidencePaper(id) => {
                if let Some(Modal::ReviewEvidence(form)) = self.modal.as_mut() { form.paper_id = id }
            }
            Cmd::PickEvidenceSource(source) => {
                if let Some(Modal::ReviewEvidence(form)) = self.modal.as_mut() { form.source = source }
            }
            Cmd::PickEvidenceSection(section) => {
                if let Some(Modal::ReviewEvidence(form)) = self.modal.as_mut() { form.section = section }
            }
            Cmd::JudgeReviewEvidence(id, judgment) => {
                use wisp_core::review::{EvidenceJudgment, ReviewAction};
                // The judgment a row already has, clicked again, is taken back.
                let has = self.lib.as_ref().and_then(|lib| lib.library_chat.review.as_ref()?.evidence_rows.iter().find(|row| row.id == id).map(|row| row.judgment == judgment && row.source_current));
                let judgment = if has == Some(true) { EvidenceJudgment::Unreviewed } else { judgment };
                self.review_apply(ReviewAction::JudgeEvidence { id, judgment }, "", cx);
            }
            Cmd::AskRemoveReviewEvidence(id) => {
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: "删除这条证据记录？".into(), message: "这条论断与来源的关联会从综述记录中移除，文献和正文不受影响。".into(), action: "删除".into(), cmd: Cmd::RemoveReviewEvidence(id) });
            }
            Cmd::RemoveReviewEvidence(id) => {
                self.review_apply(wisp_core::review::ReviewAction::RemoveEvidence { id }, "证据记录已删除", cx);
                self.run(Cmd::CloseModal, window, cx);
            }
            Cmd::CopyText(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.say("已复制", cx);
            }
            Cmd::CopySelected => {
                if let Some(text) = self.picked.as_ref().map(|picked| picked.chosen().to_string()).filter(|text| !text.is_empty()) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.say("已复制", cx);
                }
            }
            Cmd::RenameTag(tag) => {
                self.modal_subscriptions.clear();
                self.modal_error.clear();
                let name = self.modal_input("标签的新名字", &tag, false, window, cx);
                name.read(cx).focus(window);
                self.modal = Some(Modal::Tag { tag, name });
            }
            Cmd::AskRemoveTag(tag) => {
                let count = self.lib.as_ref().map_or(0, |lib| lib.papers.iter().filter(|p| p.tags.contains(&tag)).count());
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: format!("去掉标签「{tag}」？"), message: format!("它会从 {count} 篇文献上去掉。文献本身不动。"), action: "去掉标签".into(), cmd: Cmd::RemoveTag(tag) });
            }
            Cmd::RemoveTag(tag) => {
                self.modal = None;
                if self.retag(&tag, None, cx) { self.say("标签已去掉。", cx) }
            }
            Cmd::AnswerToNote(question, answer) => {
                let Some(lib) = self.lib.as_ref() else { return };
                let Some(paper_id) = lib.selected.clone() else { return };
                let note = ai::note_from_answer(&question, &answer, lib.paper_chat.sources(), &paper_id, &time::date_text(&time::now_iso()));
                self.modal_subscriptions.clear();
                self.modal_error.clear();
                let text = cx.new(|cx| TextInput::new("要记下的内容", cx).multiline(8).with_text(&note));
                text.read(cx).focus(window);
                self.modal = Some(Modal::Note { paper_id, text });
            }
            Cmd::PickText(paper, at, pointer, place) => {
                use crate::markdown::Pointer;
                let Some(lib) = self.lib.as_mut() else { return };
                // The keyboard goes to the window, so that copying finds the selection and not a text box.
                if pointer == Pointer::Down {
                    window.focus(&self.focus);
                    self.picked = None;
                }
                // One selection at a time: starting one in a conversation ends the other's.
                if pointer == Pointer::Down { lib.chat_mut(!paper).picked = None }
                let chat = lib.chat_mut(paper);
                match (pointer, chat.picked.as_mut()) {
                    (Pointer::Down, _) => chat.picked = Some((at, place, place, true)),
                    (Pointer::Drag, Some((answer, _, to, true))) if *answer == at => *to = place,
                    (Pointer::Up, Some((_, from, to, held))) if *held => {
                        *held = false;
                        // A click selects nothing.
                        if from == to { chat.picked = None }
                    }
                    _ => {}
                }
            }
            Cmd::CopyPicked(paper) => {
                if let Some(text) = self.lib.as_ref().and_then(|lib| lib.chat(paper).picked_text()) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.say("已复制", cx);
                }
            }
            Cmd::QuotePicked(paper) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let chat = lib.chat_mut(paper);
                let Some(text) = chat.picked_text() else { return };
                chat.picked = None;
                // What the assistant said, as a quotation in the box: its words, marked as such, before the reader's own.
                let quoted: String = text.lines().map(|line| format!("> {line}\n")).collect();
                chat.input.update(cx, |input, cx| {
                    let before = input.text().trim_end().to_string();
                    input.set_text(&if before.is_empty() { format!("{quoted}\n") } else { format!("{before}\n\n{quoted}\n") }, cx);
                });
                chat.input.read(cx).focus(window);
            }
            Cmd::ToggleMaterials => {
                if let Some(lib) = self.lib.as_mut() { lib.library_chat.materials_open = !lib.library_chat.materials_open }
            }
            Cmd::ToggleSources(paper, at) => {
                if let Some(lib) = self.lib.as_mut() {
                    let chat = if paper { &mut lib.paper_chat } else { &mut lib.library_chat };
                    if !chat.sources_open.remove(&at) { chat.sources_open.insert(at); }
                }
            }
            Cmd::GotoCited(paper, label, page, quote) => {
                let Some(lib) = self.lib.as_ref() else { return };
                let chat = lib.chat(paper);
                let Some(id) = chat.sources().iter().find(|source| source.label == label).map(|source| source.paper_id.clone()) else { return self.fail(format!("这次对话里没有 {label} 这篇文献。"), cx) };
                // Where the sentence was found, when it is not where the answer says.
                let page = match chat.checks.get(&(label, page, quote.clone())) {
                    Some(ai::Quoted::Elsewhere(at) | ai::Quoted::Begins(at, _)) => Some(*at),
                    _ => page,
                };
                self.run(Cmd::GotoSource(id, page, quote), window, cx);
            }
            Cmd::GotoSource(id, page, quote) => {
                // Cited from the paper's own conversation: the PDF opens to its right, where the window has room.
                let mut beside = self.lib.as_ref().is_some_and(|lib| lib.tab == Tab::Ask && lib.selected.as_deref() == Some(id.as_str()) && lib.pdf_file(&id).is_some()) && page.is_some();
                self.select(Some(id), window, cx);
                if beside {
                    self.run(Cmd::BesidePdf(true), window, cx);
                    // A window too small for the two side by side shows the PDF in the conversation's place.
                    beside = self.detail_width(window) >= crate::detail::SPLIT_MIN;
                }
                if let Some(page) = page && let Some(lib) = self.lib.as_mut() {
                    if !beside { lib.tab = Tab::Pdf }
                    self.ensure_pdf(cx);
                    if let Some(pdf) = self.lib.as_ref().and_then(|l| l.pdf.clone()) {
                        pdf.update(cx, |pdf, cx| if quote.is_empty() { pdf.go_to(page, cx) } else { pdf.find(page, &quote, cx) });
                    }
                }
            }

            Cmd::NewSearch => self.search_dialog(None, window, cx),
            Cmd::EditSearch(id) => self.search_dialog(Some(id), window, cx),
            Cmd::AskRemoveSearch(id) => {
                let Some(search) = self.lib.as_ref().and_then(|l| l.searches.iter().find(|s| s.id == id)) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: format!("删除检索式「{}」？", search.name), message: "它导入过的文献和运行记录都会保留，只是不再运行这个检索式。".into(), action: "删除".into(), cmd: Cmd::RemoveSearch(id) });
            }
            Cmd::RemoveSearch(id) => {
                self.change(cx, |lib| lib.store.remove_search(&id));
                self.run(Cmd::CloseModal, window, cx);
            }
            Cmd::TrySearch(id) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let Some(search) = lib.searches.iter().find(|s| s.id == id) else { return };
                let args = serde_json::json!({"source": search.kind, "query": search.query, "limit": 20});
                lib.trial = Some((id.clone(), None));
                self.library_tool("search_online", args, cx, move |this, result, _| {
                    // Only when this trial is still the one on show.
                    if let Some(lib) = this.lib.as_mut() && let Some((shown, found)) = lib.trial.as_mut() && *shown == id { *found = Some(result) }
                });
            }
            Cmd::CloseTrial => {
                if let Some(lib) = self.lib.as_mut() { lib.trial = None }
            }
            Cmd::RunSearch(id) => {
                let Some(lib) = self.lib.as_mut() else { return };
                if !lib.running.insert(id.clone()) { return }
                self.library_tool("run_search", serde_json::json!({"id": id}), cx, move |this, result, cx| {
                    if let Some(lib) = this.lib.as_mut() { lib.running.remove(&id); }
                    // The run wrote through its own connection, failed runs included.
                    this.change(cx, |_| Ok(()));
                    match result {
                        Ok(run) => this.say(format!("{}：新增 {} 篇，跳过已有的 {} 篇", run["search"].as_str().unwrap_or("检索"), run["added"], run["skipped"]), cx),
                        Err(err) => this.fail(err, cx),
                    }
                });
            }
            Cmd::RunAllSearches => {
                let ids: Vec<String> = self.lib.as_ref().map(|l| l.searches.iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect()).unwrap_or_default();
                for id in ids {
                    self.run(Cmd::RunSearch(id), window, cx);
                }
            }
            Cmd::AskUndoImport(id) => {
                let Some(batch) = self.lib.as_ref().and_then(|l| l.batches.iter().find(|b| b.id == id)) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm {
                    title: "撤销这次导入？".into(),
                    message: format!("{}\n\n这次导入新增的 {} 篇文献里，还没有处理过的会被删除；已经做过收录判断、读过、收藏、写过笔记、加过标签或附件的会保留。", batch.label, batch.added),
                    action: "撤销导入".into(),
                    cmd: Cmd::UndoImport(id),
                });
            }
            Cmd::UndoImport(id) => {
                let mut undone = None;
                if self.change(cx, |lib| lib.store.undo_batch(&id).map(|done| undone = Some(done)))
                    && let Some(done) = undone
                {
                    self.say(if done.kept.is_empty() { format!("已撤销：删除了 {} 篇", done.removed) } else { format!("已撤销：删除了 {} 篇，保留了处理过的 {} 篇", done.removed, done.kept.len()) }, cx);
                }
                self.run(Cmd::CloseModal, window, cx);
            }
            Cmd::PickSource(value) => {
                if let Some(Modal::Search { kind, .. }) = self.modal.as_mut() { *kind = value }
            }

            Cmd::SetAutoSync(hours) => {
                let Some(lib) = self.lib.as_mut() else { return };
                lib.info.auto_sync_hours = hours;
                if let Err(err) = libraries::save(&lib.info) { self.fail(format!("{err:#}"), cx) }
                self.libraries = libraries::list(&self.root);
            }
            Cmd::ToggleSearch(id) => {
                self.change(cx, |lib| {
                    let Some(mut search) = lib.searches.iter().find(|s| s.id == id).cloned() else { return Ok(()) };
                    search.enabled = !search.enabled;
                    lib.store.save_search(&mut search)
                });
            }
            Cmd::ScreenStart => self.screen_start(false, cx),
            Cmd::ScreenRecheck => self.screen_start(true, cx),
            Cmd::ScreenStop => {
                if let Some(lib) = self.lib.as_mut() {
                    // Answers still on their way belong to a run that is over.
                    lib.screen.run += 1;
                    lib.screen.todo.clear();
                    lib.screen.reading.clear();
                }
            }
            Cmd::ScreenDecide(id, verdict) => {
                let Some(lib) = self.lib.as_ref() else { return };
                // The paper after this one in the queue on show comes up next; the one before when it was the last.
                let queue: Vec<&str> = lib.screen_view().0.iter().map(|p| p.id.as_str()).collect();
                let next = queue.iter().position(|p| *p == id).and_then(|at| queue.get(at + 1).or(at.checked_sub(1).and_then(|before| queue.get(before)))).map(|p| p.to_string());
                // Agreeing with the assistant takes its reason along; keeping one's own decision keeps its reason; deciding otherwise leaves the reason to the person.
                let kept = lib.paper(&id).filter(|p| p.screening == verdict).map(|p| p.screening_reason.clone());
                let reason = lib.current_suggestion(&id).filter(|s| s.verdict == verdict).map(|s| s.reason.clone()).or(kept).unwrap_or_default();
                if self.edit_paper(&id, cx, |p| p.decide(&verdict, &reason)) { self.select(next, window, cx) }
            }
            Cmd::ScreenFilter(filter) => {
                if let Some(lib) = self.lib.as_mut() { lib.screen.filter = filter }
            }
            Cmd::AskScreenAccept(verdict) => {
                let Some(lib) = self.lib.as_ref() else { return };
                let count = lib.screen_queue().iter().filter(|p| lib.screen_group(&p.id) == verdict).count();
                if count == 0 { return }
                let name = label(&SCREENINGS, &verdict);
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm {
                    title: format!("把这 {count} 篇都判断为「{name}」？"),
                    message: format!("助手对这 {count} 篇待判断文献的建议都是「{name}」。采纳后，它们的收录判断改为「{name}」，判断理由用助手给出的理由。之后仍然可以在每篇文献的详情里改回来。"),
                    action: format!("采纳 {count} 篇"),
                    cmd: Cmd::ScreenAccept(verdict),
                });
            }
            Cmd::ScreenAccept(verdict) => {
                self.run(Cmd::CloseModal, window, cx);
                let Some(lib) = self.lib.as_ref() else { return };
                let agreed: Vec<(String, String)> = lib.screen_queue().iter().filter(|p| lib.screen_group(&p.id) == verdict).map(|p| (p.id.clone(), lib.suggestions[&p.id].reason.clone())).collect();
                let done = self.change(cx, |lib| {
                    for (id, reason) in &agreed {
                        let mut paper = lib.store.get_paper(id)?;
                        paper.decide(&verdict, reason);
                        lib.store.save_paper(&mut paper)?;
                    }
                    Ok(())
                });
                if done {
                    self.select(None, window, cx);
                    self.say(format!("已把 {} 篇判断为「{}」。", agreed.len(), label(&SCREENINGS, &verdict)), cx);
                }
            }

            Cmd::OpenGuide(choosing) => {
                if let Some(lib) = self.lib.as_mut() {
                    lib.page = Page::Guide;
                    lib.guide.choosing = choosing;
                    if !choosing { lib.guide.keywords.read(cx).focus(window) }
                }
            }
            Cmd::GuideSearch => {
                if let Some(lib) = self.lib.as_mut() {
                    lib.guide.choosing = false;
                    lib.guide.keywords.read(cx).focus(window);
                }
            }
            Cmd::GuideToggleSource(kind) => {
                if let Some(lib) = self.lib.as_mut() {
                    let kinds = &mut lib.guide.kinds;
                    // At least one database stays chosen.
                    if let Some(at) = kinds.iter().position(|k| *k == kind) { if kinds.len() > 1 { kinds.remove(at); } } else { kinds.push(kind) }
                }
            }
            Cmd::GuideGenerate => self.guide_generate(cx),
            Cmd::GuideToggleIdea(index) => {
                if let Some(idea) = self.lib.as_mut().and_then(|l| l.guide.ideas.get_mut(index)) { idea.chosen = !idea.chosen }
            }
            Cmd::GuideImport => {
                let Some(lib) = self.lib.as_mut() else { return };
                let mut saved = vec![];
                let mut failure = None;
                for entry in lib.guide.ideas.iter().filter(|entry| entry.chosen) {
                    // Named after what the search aims at, as far as a name may go.
                    let mut search = SavedSearch { kind: entry.idea.source.clone(), name: entry.idea.why.chars().take(40).collect(), query: entry.idea.query.clone(), max_results: wisp_core::sources::MAX_RESULTS, enabled: true, ..Default::default() };
                    match lib.store.save_search(&mut search) {
                        Ok(()) => saved.push(search.id),
                        Err(err) => failure = Some(format!("{err:#}")),
                    }
                }
                if saved.is_empty() && failure.is_none() { return }
                // What was saved is on the searches page from here on.
                lib.guide.ideas.retain(|entry| !entry.chosen);
                lib.page = Page::Sources;
                self.change(cx, |_| Ok(()));
                if let Some(err) = failure { self.fail(err, cx) }
                for id in saved {
                    self.run(Cmd::RunSearch(id), window, cx);
                }
            }

            Cmd::CloseModal => {
                self.modal = None;
                self.modal_subscriptions.clear();
                window.focus(&self.focus);
            }
            Cmd::SubmitModal => self.submit_modal(window, cx),
            Cmd::CompletePaper(id) => self.complete_dialog(id, window, cx),
            Cmd::CompleteChoose(at) => {
                self.modal_error.clear();
                let Some(Modal::Complete(form)) = self.modal.as_mut() else { return };
                form.chosen = at.filter(|at| form.found.as_ref().is_some_and(|found| *at < found.candidates.len()));
                form.fields = match (form.chosen, self.lib.as_ref().and_then(|lib| lib.paper(&form.paper_id))) {
                    (Some(at), Some(paper)) => match &form.found.as_ref().unwrap().candidates[at] {
                        candidate if candidate.similarity >= complete::ALIKE_ENOUGH => complete::suggested(paper, &candidate.paper, form.placeholder),
                        _ => vec![],
                    },
                    _ => vec![],
                };
            }
            Cmd::CompleteField(key) => {
                let Some(Modal::Complete(form)) = self.modal.as_mut() else { return };
                match form.fields.iter().position(|taken| *taken == key) {
                    Some(at) => { form.fields.remove(at); }
                    None => form.fields.push(key),
                }
            }
            Cmd::PickColor(value) => {
                if let Some(Modal::Library { color, .. }) = self.modal.as_mut() { *color = value }
            }
            Cmd::PickKind(value) => {
                if let Some(Modal::Fields { kind, .. }) = self.modal.as_mut() { *kind = value }
            }
            Cmd::ToggleInTable => {
                if let Some(Modal::Fields { in_table, .. }) = self.modal.as_mut() { *in_table = !*in_table }
            }
            Cmd::PickOption(field, option) => {
                if let Some(Modal::Paper(form)) = self.modal.as_mut() {
                    if form.selects.get(&field) == Some(&option) { form.selects.remove(&field); } else { form.selects.insert(field, option); }
                }
            }
        }
    }

    pub fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
        } else if self.modal.is_some() {
            self.run(Cmd::CloseModal, window, cx);
        } else if self.settings_view.is_some() {
            self.close_settings(window, cx);
        } else if self.picked.take().is_some() {
        } else if self.lib.as_ref().is_some_and(|l| !l.marked.is_empty()) {
            if let Some(lib) = self.lib.as_mut() { lib.marked.clear() }
        } else if self.lib.as_ref().is_some_and(|l| l.reading && l.selected.is_some()) {
            // Out of reading mode first: the paper stays open where it was read.
            if let Some(lib) = self.lib.as_mut() { lib.reading = false }
        } else if self.lib.as_ref().is_some_and(|l| l.selected.is_some()) {
            self.select(None, window, cx);
        }
        cx.notify();
    }

    /// A keyboard shortcut (shortcuts.rs) was pressed.
    pub fn shortcut(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A dialog keeps the keyboard to itself.
        if self.modal.is_some() { return }
        let in_settings = self.settings_view.is_some();
        let cmd = match id {
            "settings" if !in_settings => Cmd::OpenSettings(SettingsPage::General),
            "home" => Cmd::GoHome,
            "new-library" if !in_settings && self.lib.is_none() => Cmd::NewLibrary,
            // The rest act on the open library, when it is the one in view.
            _ if in_settings || self.lib.is_none() => return,
            "search" => {
                if let Some(lib) = self.lib.as_mut() {
                    lib.page = Page::Papers;
                    lib.reading = false;
                    lib.search.read(cx).focus(window);
                    cx.notify();
                }
                return;
            }
            "add" => Cmd::AddById,
            "import-pdf" => Cmd::ImportPdfs,
            "ask" => Cmd::SetPage(Page::Ask),
            "detail" if self.lib.as_ref().is_some_and(|l| l.selected.is_some()) => Cmd::ToggleWide,
            "sidebar" => Cmd::ToggleSidebar,
            "reading" if self.lib.as_ref().is_some_and(|l| l.selected.is_some()) => Cmd::ToggleReading,
            // The PDF's size, while a PDF is on show.
            "zoom-in" => Cmd::PdfZoom(ZoomTo::In),
            "zoom-out" => Cmd::PdfZoom(ZoomTo::Out),
            "zoom-fit" => Cmd::PdfZoom(ZoomTo::Set(Zoom::FitWidth)),
            "write-find" if self.lib.as_ref().is_some_and(|l| l.page == Page::Write) => Cmd::WriteFind,
            // What is selected in an answer: the paper's conversation first, when both have something.
            "copy-picked" if self.picked.is_some() => Cmd::CopySelected,
            "copy-picked" => match self.lib.as_ref().map(|lib| (lib.paper_chat.picked.is_some(), lib.library_chat.picked.is_some())) {
                Some((true, _)) => Cmd::CopyPicked(true),
                Some((false, true)) => Cmd::CopyPicked(false),
                _ => return,
            },
            _ => return,
        };
        self.run(cmd, window, cx);
    }

    // ---- libraries

    fn open_library(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut info) = self.libraries.iter().find(|l| l.id == id).cloned() else { return };
        let store = match Store::open(&info.dir) {
            Ok(store) => store,
            Err(err) => return self.fail(format!("{err:#}"), cx),
        };
        self.write_snapshot("leave");
        info.opened_at = time::now_iso();
        let _ = libraries::save(&info);
        theme::set_accent(&info.color);
        window.set_window_title(&format!("{} — Wisp Paper", info.name));
        let search = cx.new(|cx| TextInput::new("搜索题目、作者、DOI、标签、词条、笔记…", cx));
        let notes = cx.new(|cx| TextInput::new("读这篇文献时想到的…", cx).multiline(4));
        let subscriptions = vec![
            cx.subscribe(&search, |this, input, event: &InputEvent, cx| {
                if *event == InputEvent::Changed && let Some(lib) = this.lib.as_mut() {
                    lib.filters.search = input.read(cx).text().to_string();
                    lib.page = Page::Papers;
                    lib.refilter();
                    cx.notify();
                }
            }),
            cx.subscribe(&notes, |this, input, event: &InputEvent, cx| {
                if *event == InputEvent::Changed {
                    let text = input.read(cx).text().to_string();
                    if let Some(id) = this.lib.as_ref().and_then(|l| l.selected.clone()) {
                        this.edit_paper(&id, cx, |p| p.notes = text);
                    }
                }
            }),
        ];
        let library_chat = Chat::new("向文献库提问，输入 / 选择命令，可以粘贴图片", cx);
        let paper_chat = Chat::new("就这篇文献提问，输入 / 选择命令", cx);
        let mut subscriptions = subscriptions;
        for (chat, paper) in [(&library_chat, false), (&paper_chat, true)] {
            subscriptions.push(cx.subscribe(&chat.input, move |this, _, event: &InputEvent, cx| this.chat_input(paper, *event, cx)));
        }
        let guide = Guide::new(&info.topic, cx);
        let screen = Screen::new(&info.criteria, cx);
        // The goal is the library's, kept with it as it is typed; suggestions made against another one stop counting.
        subscriptions.push(cx.subscribe(&screen.criteria, |this, input, event: &InputEvent, cx| {
            if *event != InputEvent::Changed { return }
            let goal = input.read(cx).text().trim().to_string();
            let Some(lib) = this.lib.as_mut().filter(|l| l.info.criteria != goal) else { return };
            lib.info.criteria = goal;
            if let Err(err) = libraries::save(&lib.info) { this.fail(format!("筛选目标没有保存：{err:#}"), cx) }
        }));
        let manuscripts = store.manuscripts().unwrap_or_default();
        let (manuscript, section_id) = Writing::latest(&manuscripts);
        let opened = manuscripts.iter().find(|m| m.id == manuscript);
        let draft = opened.and_then(|m| m.section(&section_id)).map(|s| s.body.clone()).unwrap_or_default();
        let title = cx.new(|cx| TextInput::new(wisp_core::manuscript::UNTITLED, cx).with_text(&opened.map(|m| m.title.clone()).unwrap_or_default()));
        subscriptions.push(cx.subscribe(&title, |this, input, event: &InputEvent, cx| {
            if *event == InputEvent::Changed { this.write_title_typed(input.read(cx).text().to_string(), cx) }
        }));
        let editor = cx.new(|cx| TextInput::new("在这里写。写好一段，按「查找候选句」的快捷键，右边就列出文献库里的文献在同一部分相近的写法；中文或别的语言也可以，先写下想表达的意思。", cx).multiline(14).with_text(&draft));
        let find = cx.new(|cx| TextInput::new("找句子：输入词或短语，例如 remains unclear", cx));
        subscriptions.push(cx.subscribe(&editor, |this, input, event: &InputEvent, cx| {
            if *event != InputEvent::Changed { return }
            let text = input.read(cx).text().to_string();
            this.write_typed(text, cx);
        }));
        // Candidates are asked for: the find box on Enter, the manuscript by the shortcut.
        subscriptions.push(cx.subscribe(&find, |this, _, event: &InputEvent, cx| if *event == InputEvent::Submit { this.write_find(true, cx) }));
        let writing = Writing { manuscripts, manuscript, section_id, reading: false, title, typed: 0, editor, find, search: Default::default(), structures: BTreeMap::new(), sorting: Sorting::default(), scroll: ScrollHandle::new(), assist: Default::default() };
        let citing = crate::cite::Citing::new(&store, &mut subscriptions, cx);
        let mut lib = Library {
            info,
            store,
            papers: vec![],
            fields: vec![],
            files: vec![],
            trash: vec![],
            filters: Filters::default(),
            shown: vec![],
            search,
            page: Page::Papers,
            selected: None,
            tab: Tab::Info,
            wide: false,
            reading: false,
            split: true,
            float_drag: None,
            float_area: (0., 0.),
            notes,
            pdf: None,
            pdf_events: None,
            zoom: Zoom::FitWidth,
            library_chat,
            paper_chat,
            marked: vec![],
            anchor: None,
            cursor: None,
            asked: vec![],
            table_scroll: UniformListScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            extracting: HashSet::new(),
            guide,
            screen,
            suggestions: HashMap::new(),
            translations: HashMap::new(),
            translation_asks: HashMap::new(),
            translation_run: None,
            translation_session: Arc::new(()),
            show_translation: false,
            searches: vec![],
            batches: vec![],
            trial: None,
            running: HashSet::new(),
            import: None,
            zotero: None,
            fetching: None,
            origins: BTreeMap::new(),
            writing,
            citing,
            _subscriptions: subscriptions,
        };
        if let Err(err) = lib.reload() {
            return self.fail(format!("{err:#}"), cx);
        }
        self.lib = Some(lib);
        window.focus(&self.focus);
        self.extract_missing(cx);
    }

    // ---- one paper

    pub(crate) fn select(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        if lib.selected == id { return }
        self.picked = None;
        // One paper on show ends a marking of several.
        if id.is_some() && let Some(lib) = self.lib.as_mut() { lib.marked.clear() }
        // The conversation with the paper being left is kept as far as it got.
        self.stop_chat(true, cx);
        let Some(lib) = self.lib.as_mut() else { return };
        lib.selected = id;
        lib.pdf = None;
        lib.paper_chat.reset(cx);
        lib.detail_scroll = ScrollHandle::new();
        match lib.selected_paper() {
            Some(paper) => {
                let notes = paper.notes.clone();
                lib.notes.update(cx, |input, cx| input.set_text(&notes, cx));
            }
            None => {
                lib.wide = false;
                lib.reading = false;
                lib.tab = Tab::Info;
                window.focus(&self.focus);
            }
        }
        self.ensure_pdf(cx);
        self.translate_selected(false, cx);
        cx.notify();
    }

    /// Translate a passage selected in the PDF: with the model bound to
    /// translation from what is kept, or, with `profile`, asked of that model
    /// for this once (the settings stay as they are) and kept in place of
    /// the earlier translation.
    fn translate_passage(&mut self, text: String, profile: Option<String>, cx: &mut Context<Self>) {
        let chosen = profile.as_deref().and_then(|id| self.settings.profiles.iter().find(|p| p.id == id)).cloned();
        let again = chosen.is_some();
        let profile = chosen.unwrap_or_else(|| self.settings.profile_for_or_default(Task::Translate));
        // A click is a request: what failed before is asked again.
        let prompt = self.settings.translation_prompt.clone();
        self.translate_text(&text, &profile, &prompt, true, again, cx);
        self.sync_pdf_translation(cx);
    }

    /// Tell the PDF viewer how the translation on its card stands, and which models it may be asked of.
    pub(crate) fn sync_pdf_translation(&mut self, cx: &mut Context<Self>) {
        let prompt = ai::translation_prompt(&self.settings.translation_prompt).to_string();
        let models: Vec<(String, String)> = self.settings.profiles.iter().map(|p| (p.id.clone(), p.title())).collect();
        let usual = self.settings.profile_for_or_default(Task::Translate);
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(pdf) = lib.pdf.clone() else { return };
        let Some((text, profile)) = pdf.read(cx).translation_asked() else { return };
        let state = lib.translations.get(&wisp_core::store::translation_key(&text, &prompt)).cloned().unwrap_or(Translated::Busy);
        let model = profile.and_then(|id| self.settings.profiles.iter().find(|p| p.id == id)).unwrap_or(&usual);
        let model = (model.id.clone(), model.title());
        pdf.update(cx, |pdf, cx| pdf.show_translation(state, model, models, cx));
    }

    /// Open the PDF viewer when its tab is showing and the paper has a PDF.
    fn ensure_pdf(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        if !(lib.tab == Tab::Pdf || (lib.tab == Tab::Ask && lib.split)) || lib.pdf.is_some() { return }
        let Some(path) = lib.selected.as_deref().and_then(|id| lib.pdf_file(id)).map(|f| lib.store.file_path(f)) else { return };
        let zoom = lib.zoom;
        let pdf = cx.new(|cx| PdfView::new(&path, zoom, cx));
        lib.pdf_events = Some(cx.subscribe(&pdf, |this, _, event: &PdfEvent, cx| this.pdf_event(event, cx)));
        lib.pdf = Some(pdf);
    }

    /// What the PDF viewer reports: its selection copied, or handed to a conversation.
    fn pdf_event(&mut self, event: &PdfEvent, cx: &mut Context<Self>) {
        match event {
            PdfEvent::Copied => self.say("已复制", cx),
            PdfEvent::Translate(text, profile) => self.translate_passage(text.clone(), profile.clone(), cx),
            PdfEvent::Picture(png, to_library) => {
                self.attach_images(!*to_library, vec![Picture::Bytes(png.clone())], cx);
                if *to_library { self.say("截图已加到文献库对话的输入框上方。", cx) } else { self.show_paper_chat(cx) }
            }
            PdfEvent::Quote(page, text, to_library) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let Some(paper_id) = lib.selected.clone() else { return };
                let quote = ChatQuote { paper_id, page: *page, text: text.clone() };
                let chat = lib.chat_mut(!*to_library);
                if chat.quotes.len() >= MAX_QUOTES { return self.fail(format!("一个问题最多带 {MAX_QUOTES} 段原文。先发出去，或者去掉一段。"), cx) }
                if !chat.quotes.contains(&quote) { chat.quotes.push(quote) }
                chat.history = None;
                if *to_library {
                    self.say("已加到文献库对话的输入框上方。", cx);
                } else {
                    self.show_paper_chat(cx);
                }
                cx.notify();
            }
        }
    }

    /// Something from the PDF went to the paper's own conversation: the
    /// conversation comes up with the PDF to its right. The panel is
    /// widened for the two; a window too small for both shows the
    /// conversation, with what was added over its box.
    fn show_paper_chat(&mut self, cx: &mut Context<Self>) {
        let narrow = self.settings.detail_width < crate::detail::SPLIT_MIN;
        self.unfold_chat(cx);
        let Some(lib) = self.lib.as_mut() else { return };
        if !(lib.split && lib.tab == Tab::Ask) {
            (lib.split, lib.tab) = (true, Tab::Ask);
            if narrow { lib.wide = true }
            self.say("已加到这篇文献的提问里。", cx);
        }
        cx.notify();
    }

    /// Change one paper and save it.
    /// A tag under another name on every paper that has it (one that has both keeps one), or taken off
    /// them. The list stays on the same papers when it is narrowed to that tag.
    fn retag(&mut self, tag: &str, name: Option<&str>, cx: &mut Context<Self>) -> bool {
        self.change(cx, |lib| {
            let ids: Vec<String> = lib.papers.iter().filter(|p| p.tags.iter().any(|t| t == tag)).map(|p| p.id.clone()).collect();
            for id in ids {
                let mut paper = lib.store.get_paper(&id)?;
                let mut tags: Vec<String> = vec![];
                for old in &paper.tags {
                    let kept = if old == tag { name.map(str::to_string) } else { Some(old.clone()) };
                    if let Some(kept) = kept && !tags.contains(&kept) { tags.push(kept) }
                }
                paper.tags = tags;
                lib.store.save_paper(&mut paper)?;
            }
            if lib.filters.tag == tag { lib.filters.tag = name.unwrap_or("").to_string() }
            Ok(())
        })
    }

    fn edit_paper(&mut self, id: &str, cx: &mut Context<Self>, edit: impl FnOnce(&mut Paper)) -> bool {
        self.change(cx, |lib| {
            let mut paper = lib.store.get_paper(id)?;
            edit(&mut paper);
            lib.store.save_paper(&mut paper)
        })
    }

    // ---- collecting

    fn pick_zotero(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        if lib.zotero.as_ref().is_some_and(|run| run.running) { return }
        let library = lib.info.id.clone();
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("选择 zotero.sqlite".into()), extensions: Some(vec!["sqlite".into()]) });
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            this.update(cx, |this, cx| {
                if this.lib.as_ref().is_none_or(|l| l.info.id != library) { return }
                match result {
                    Ok(Ok(Some(paths))) => { if let Some(path) = paths.into_iter().next() { this.import_zotero(path, cx) } }
                    Ok(Ok(None)) => {}
                    Ok(Err(err)) => { this.modal_error = format!("无法打开文件选择器：{err:#}"); }
                    Err(err) => { this.modal_error = format!("文件选择器未能返回结果：{err}"); }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    fn import_zotero(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.zotero.as_ref().is_some_and(|run| run.running) { return }
        let library = lib.info.id.clone();
        let root = lib.store.root().to_path_buf();
        let run_id = new_id("Z");
        lib.zotero = Some(ZoteroRun { id: run_id.clone(), running: true, current: "正在读取数据库…".into(), ..Default::default() });
        lib.page = Page::Papers;
        self.modal = Some(Modal::ZoteroReport);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let parsed = cx.background_executor().spawn(async move { zotero::read(&path) }).await;
            let entries = this.update(cx, |this, cx| {
                let run = this.lib.as_mut().filter(|l| l.info.id == library)?.zotero.as_mut().filter(|r| r.id == run_id)?;
                let entries = match parsed {
                    Ok(imported) => {
                        run.total = imported.entries.len();
                        run.other_attachments = imported.other_attachments;
                        Some(imported.entries)
                    }
                    Err(err) => { run.error = format!("{err:#}"); run.running = false; None }
                };
                cx.notify();
                entries
            });
            let Ok(Some(entries)) = entries else { return };
            for entry in entries {
                let active = this.update(cx, |this, cx| {
                    let Some(run) = this.lib.as_mut().filter(|l| l.info.id == library).and_then(|l| l.zotero.as_mut()).filter(|r| r.id == run_id) else { return false };
                    run.current = entry.paper.title.clone();
                    cx.notify();
                    true
                });
                if !matches!(active, Ok(true)) { return }
                let folder = root.clone();
                let row = cx.background_executor().spawn(async move {
                    match Store::open(&folder) {
                        Ok(mut store) => zotero::import_entry(&mut store, entry),
                        Err(err) => zotero::Row { title: entry.paper.title, problems: vec![format!("{err:#}")], ..Default::default() },
                    }
                }).await;
                let active = this.update(cx, |this, cx| {
                    let Some(run) = this.lib.as_mut().filter(|l| l.info.id == library).and_then(|l| l.zotero.as_mut()).filter(|r| r.id == run_id) else { return false };
                    run.rows.push(row);
                    cx.notify();
                    true
                });
                if !matches!(active, Ok(true)) { return }
            }
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return };
                let Some(run) = lib.zotero.as_mut().filter(|r| r.id == run_id) else { return };
                run.running = false;
                if let Err(err) = lib.reload() { this.fail(format!("{err:#}"), cx) }
                this.extract_missing(cx);
                cx.notify();
            }).ok();
        }).detach();
    }

    fn pick_files(&mut self, prompt: &'static str, pdfs: bool, cx: &mut Context<Self>) {
        let extensions: Vec<SharedString> = if pdfs { vec!["pdf".into()] } else { vec!["bib".into(), "ris".into(), "txt".into(), "enw".into()] };
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(prompt.into()), extensions: Some(extensions) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| this.import_paths(paths, cx)).ok();
            }
        })
        .detach();
    }

    /// Folders picked: every PDF under them, tagged with the folders it lies in.
    fn pick_folders(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("导入".into()), extensions: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| this.import_paths(paths, cx)).ok();
            }
        })
        .detach();
    }

    fn pick_attachment(&mut self, paper_id: String, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("添加".into()), extensions: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| {
                    let mut added = 0;
                    this.change(cx, |lib| {
                        let has_fulltext = lib.files.iter().any(|f| f.paper_id == paper_id && f.kind == "fulltext");
                        for (i, path) in paths.iter().enumerate() {
                            let is_pdf = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
                            // The first PDF of a paper is its full text; the rest are supplements.
                            let kind = if is_pdf && !has_fulltext && i == 0 { "fulltext" } else { "supplement" };
                            lib.store.import_file(&paper_id, path, kind)?;
                            added += 1;
                        }
                        Ok(())
                    });
                    if added > 0 {
                        if let Some(lib) = this.lib.as_mut() { lib.pdf = None }
                        this.ensure_pdf(cx);
                        this.extract_missing(cx);
                        this.say(format!("已添加 {added} 个附件。"), cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Files picked or dropped: PDFs become papers, reference files are
    /// imported, and a folder gives every PDF under it, tagged with the
    /// folders between it and the file (`import::folder_pdfs`).
    pub fn import_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.lib.is_none() { return self.fail("请先打开一个文献库，再拖入文件。", cx) }
        let mut pdfs: Vec<(PathBuf, Vec<String>)> = vec![];
        let mut others = vec![];
        let mut empty_folders = vec![];
        for path in paths {
            if path.is_dir() {
                let found = import::folder_pdfs(&path);
                if found.is_empty() { empty_folders.push(path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()) }
                pdfs.extend(found);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
                pdfs.push((path, vec![]));
            } else {
                others.push(path);
            }
        }
        if !empty_folders.is_empty() && pdfs.is_empty() && others.is_empty() {
            return self.fail(format!("文件夹「{}」里没有 PDF。", empty_folders.join("」「")), cx);
        }
        let mut added = vec![];
        let mut skipped = 0;
        let mut errors = vec![];
        for path in others {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let parsed = std::fs::read(&path).map_err(|e| format!("无法读取 {name}：{e}")).and_then(|bytes| import::parse_references(&String::from_utf8_lossy(&bytes), &format!("导入 · {name}")).map_err(|e| format!("{name}：{e}")));
            match parsed {
                Ok(papers) => {
                    let mut result = None;
                    self.change(cx, |lib| {
                        result = Some(lib.store.import_papers(papers)?);
                        Ok(())
                    });
                    if let Some(result) = result {
                        skipped += result.skipped.len();
                        added.extend(result.added.into_iter().map(|p| p.id));
                    }
                }
                Err(err) => errors.push(err),
            }
        }
        if !errors.is_empty() {
            self.fail(errors.join("；"), cx);
        } else if !added.is_empty() || skipped > 0 {
            self.say(format!("导入 {} 篇{}。", added.len(), if skipped > 0 { format!("，跳过 {skipped} 篇已有的") } else { String::new() }), cx);
        }
        if pdfs.is_empty() { return }
        let Some(lib) = self.lib.as_mut() else { return };
        // More files while some are still being read join them; a report already read gives way.
        match lib.import.as_mut().filter(|run| run.running()) {
            Some(run) => run.total += pdfs.len(),
            None => lib.import = Some(ImportRun { total: pdfs.len(), ..Default::default() }),
        }
        let library = lib.info.id.clone();
        cx.notify();
        // Most of the wait is the two services asked about each DOI: a few files are read side by side.
        let queue = std::rc::Rc::new(std::cell::RefCell::new(pdfs.into_iter().rev().collect::<Vec<_>>()));
        for _ in 0..IMPORT_WORKERS.min(queue.borrow().len()) {
            let (queue, library) = (queue.clone(), library.clone());
            cx.spawn(async move |this, cx| {
                loop {
                    let Some((path, tags)) = queue.borrow_mut().pop() else { break };
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let known = this.update(cx, |this, cx| {
                        let Some(run) = this.lib.as_mut().filter(|l| l.info.id == library).and_then(|l| l.import.as_mut()) else { return false };
                        run.working.push(name);
                        cx.notify();
                        true
                    });
                    if !matches!(known, Ok(true)) { break }
                    // Read the text, look for a DOI on the first pages, and ask Crossref about it.
                    let file = path.clone();
                    let (pages, found) = cx
                        .background_executor()
                        .spawn(async move {
                            let pages = fulltext::file_pages(&file);
                            let doi = pages.as_ref().ok().and_then(|pages| lookup::find_doi(&pages.iter().take(2).cloned().collect::<Vec<_>>().join("\n")));
                            let found = doi.and_then(|doi| wisp_net::lookup_paper(&Identifier::Doi(doi)).ok());
                            (pages, found)
                        })
                        .await;
                    let done = this.update(cx, |this, cx| {
                        if this.lib.as_ref().is_none_or(|l| l.info.id != library) { return false }
                        this.add_pdf(&path, &tags, pages, found, cx);
                        true
                    });
                    if !matches!(done, Ok(true)) { break }
                }
            })
            .detach();
        }
    }

    /// One read PDF: a new paper with the file attached, or the file attached
    /// to the paper the library already holds for its DOI. What came of it
    /// joins the import's report; a single file says so at once instead.
    /// `tags` (the folders the file lay in) go to the paper, a paper already
    /// held included.
    fn add_pdf(&mut self, path: &std::path::Path, tags: &[String], pages: Result<Vec<String>, String>, found: Option<Paper>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let stem = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "未命名文献".into());
        let recognised = found.is_some();
        let mut paper = found.unwrap_or_else(|| Paper { title: stem.clone(), ..Default::default() });
        paper.source = "PDF 导入".into();
        paper.tags = clean_strings(paper.tags.iter().chain(tags));
        let mut row = ImportRow { name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| stem.clone()), outcome: Imported::Added, paper_id: None, unrecognised: false, textless: String::new() };
        let stored = (|| -> anyhow::Result<()> {
            let target = match lib.store.existing_paper(&{
                let mut probe = paper.clone();
                probe.normalize();
                probe
            })? {
                Some(existing) => {
                    if lib.trash.iter().any(|t| t.paper.id == existing) {
                        row.outcome = Imported::InTrash;
                        return Ok(());
                    }
                    row.outcome = Imported::Attached;
                    let mut held = lib.store.get_paper(&existing)?;
                    if tags.iter().any(|tag| !held.tags.contains(tag)) {
                        held.tags = clean_strings(held.tags.iter().chain(tags));
                        lib.store.save_paper(&mut held)?;
                    }
                    existing
                }
                None => {
                    row.unrecognised = !recognised;
                    lib.store.add_paper(paper)?.id
                }
            };
            let kind = if lib.store.paper_files(&target)?.iter().any(|f| f.kind == "fulltext") { "supplement" } else { "fulltext" };
            let file = lib.store.import_file(&target, path, kind)?;
            if lib.store.needs_structure(&file.id)? { lib.store.save_fulltext_pages(&file.id, &pages)? }
            // A supplement without text takes nothing from the questions: the paper's own text is there.
            if kind == "fulltext" && let Err(reason) = &pages { row.textless = reason.clone() }
            row.paper_id = Some(target);
            Ok(())
        })()
        .and_then(|()| lib.reload());
        if let Err(err) = stored { row.outcome = Imported::Failed(format!("{err:#}")) }
        let title = row.paper_id.as_deref().and_then(|id| lib.paper(id)).map(|p| p.title.clone()).unwrap_or_default();
        let said = match &row.outcome {
            Imported::Added if row.unrecognised => format!("已添加「{stem}」。没有识别出 DOI：可以在文献详情里「按题目补全」。"),
            Imported::Added => format!("已添加：{title}"),
            Imported::Attached => format!("「{stem}」已在文献库中，PDF 已附到原条目。"),
            Imported::InTrash => format!("「{stem}」对应的文献在回收站中，未导入。"),
            Imported::Failed(err) => format!("「{stem}」没有导入：{err}"),
        };
        let said = if row.textless.is_empty() { said } else { format!("{said} 但没有读出文字（{}），全文暂时不能用于提问。", fulltext::failure_brief(&row.textless)) };
        let failed = matches!(row.outcome, Imported::Failed(_));
        let Some(run) = lib.import.as_mut() else { return };
        if let Some(at) = run.working.iter().position(|name| name == &row.name) { run.working.remove(at); }
        run.rows.push(row);
        let (single, finished) = (run.total == 1, !run.running());
        if finished {
            run.working.clear();
            // The paper the last file became is opened when nothing else is.
            if let Some(id) = run.rows.iter().rev().find_map(|row| row.paper_id.clone()) && lib.selected.is_none() {
                lib.selected = Some(id);
                let notes = lib.selected_paper().map(|p| p.notes.clone()).unwrap_or_default();
                lib.notes.update(cx, |input, cx| input.set_text(&notes, cx));
            }
        }
        // One file is one sentence; several are a report that stays until it is read.
        if single {
            lib.import = None;
            if failed { self.fail(said, cx) } else { self.say(said, cx) }
        }
        cx.notify();
    }

    /// Try once more to get the text of a stored file: after the reader has
    /// improved, or the file was put right outside the app.
    fn reread_file(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let Some(file) = lib.files.iter().find(|f| f.id == id) else { return };
        if !lib.extracting.insert(id.clone()) { return }
        let (library, path) = (lib.info.id.clone(), lib.store.file_path(file));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let pages = cx.background_executor().spawn(async move { fulltext::file_pages(&path) }).await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return };
                lib.extracting.remove(&id);
                let said = match &pages {
                    Ok(pages) => Ok(format!("读出了 {} 页文字，现在可以用于提问。", pages.len())),
                    Err(reason) => Err(format!("还是没有读出文字：{reason}")),
                };
                let kept = lib.store.save_fulltext_pages(&id, &pages).and_then(|()| lib.reload());
                lib.texts_changed();
                match (kept, said) {
                    (Err(err), _) => this.fail(format!("{err:#}"), cx),
                    (Ok(()), Ok(said)) => this.say(said, cx),
                    (Ok(()), Err(said)) => this.fail(said, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Read the text of the stored full texts that have not been read and
    /// sorted into parts yet, one at a time: new files, and those an
    /// earlier build read without telling headings from paragraphs.
    pub fn extract_missing(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let _ = lib.store.restructure_outdated();
        let mut todo = vec![];
        for file in &lib.files {
            if file.kind != "fulltext" || lib.extracting.contains(&file.id) { continue }
            if matches!(lib.store.needs_structure(&file.id), Ok(true)) {
                todo.push((file.id.clone(), lib.store.file_path(file), lib.store.fulltext_pages(&file.id).ok().flatten()));
            }
        }
        if todo.is_empty() { return }
        lib.extracting.extend(todo.iter().map(|(id, ..)| id.clone()));
        let library = lib.info.id.clone();
        cx.spawn(async move |this, cx| {
            for (id, path, known) in todo {
                let pages = cx.background_executor().spawn(async move { fulltext::reread(&path, known) }).await;
                let same = this.update(cx, |this, cx| {
                    let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return false };
                    lib.extracting.remove(&id);
                    let _ = lib.store.save_fulltext_pages(&id, &pages);
                    // Whether a text came of it shows in the list.
                    let _ = lib.reload();
                    // The candidates on show were found without this paper.
                    if lib.extracting.is_empty() { this.write_texts_changed(cx) }
                    cx.notify();
                    true
                });
                if !matches!(same, Ok(true)) { break }
            }
        })
        .detach();
    }

    fn export(&mut self, kind: ExportKind, cx: &mut Context<Self>) {
        let Some(lib) = &self.lib else { return };
        let papers: Vec<&Paper> = lib.shown.iter().map(|&i| &lib.papers[i]).collect();
        if papers.is_empty() { return self.fail("当前列表是空的，没有可导出的文献。", cx) }
        let (text, extension) = match kind {
            ExportKind::Bibtex => (export::bibtex(&papers), "bib"),
            ExportKind::Csv => (export::papers_csv(&papers, &lib.fields), "csv"),
        };
        let count = papers.len();
        let name = format!("{}.{extension}", lib.info.name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-"));
        let directory = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(|| self.root.clone());
        let picked = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut path))) = picked.await {
                if path.extension().is_none() { path.set_extension(extension); }
                let result = std::fs::write(&path, text);
                this.update(cx, |this, cx| match result {
                    Ok(()) => this.say(format!("已导出 {count} 篇到 {}", path.display()), cx),
                    Err(err) => this.fail(format!("无法写入 {}：{err}", path.display()), cx),
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- questions

    /// A key or a paste in a conversation's box.
    fn chat_input(&mut self, about_paper: bool, event: InputEvent, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = lib.chat_mut(about_paper);
        let (offered, picked) = (chat.slash_list(&self.settings.commands, cx).len(), chat.slash);
        match event {
            // Enter on a command in the list takes it; otherwise the message goes.
            InputEvent::Submit if offered > 0 => return self.pick_slash(about_paper, picked, cx),
            InputEvent::Submit => return self.ask(about_paper, cx),
            InputEvent::Tab if offered > 0 => return self.pick_slash(about_paper, picked, cx),
            InputEvent::Changed => {
                chat.slash_closed = false;
                chat.slash = 0;
            }
            InputEvent::Up if offered > 0 => chat.slash = (chat.slash + offered - 1) % offered,
            InputEvent::Down if offered > 0 => chat.slash = (chat.slash + 1) % offered,
            InputEvent::Cancel => chat.slash_closed = true,
            InputEvent::Images => {
                let pictures = chat.input.update(cx, |input, _| input.take_images()).into_iter().map(|image| Picture::Bytes(image.bytes)).collect();
                return self.attach_images(about_paper, pictures, cx);
            }
            _ => {}
        }
        // While the list is on show the arrow keys and Tab are its keys.
        let choosing = !chat.slash_list(&self.settings.commands, cx).is_empty();
        chat.input.update(cx, |input, _| input.choosing = choosing);
        cx.notify();
    }

    /// Take one of the slash commands on offer: one the window carries out
    /// runs, the others are written out for their subject to follow, or
    /// to be sent as they are.
    fn pick_slash(&mut self, about_paper: bool, at: usize, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = lib.chat_mut(about_paper);
        let Some(command) = chat.slash_list(&self.settings.commands, cx).into_iter().nth(at) else { return };
        let alone = command.runs;
        chat.slash = 0;
        chat.input.update(cx, |input, cx| {
            input.choosing = false;
            input.set_text(&if alone { format!("/{}", command.name) } else { format!("/{} ", command.name) }, cx);
        });
        if alone { self.ask(about_paper, cx) }
        cx.notify();
    }

    /// Read pictures in and put them with the next question. Each is made
    /// one a model takes (`picture::prepare`) off the main thread.
    fn attach_images(&mut self, about_paper: bool, mut pictures: Vec<Picture>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let (library, selected) = (lib.info.id.clone(), lib.selected.clone());
        let chat = lib.chat_mut(about_paper);
        let room = MAX_ATTACHMENTS.saturating_sub(chat.attachments.len() + chat.attaching);
        let too_many = pictures.len() > room;
        pictures.truncate(room);
        chat.attaching += pictures.len();
        if too_many { self.fail(format!("一次提问最多带 {MAX_ATTACHMENTS} 张图片。"), cx) }
        if pictures.is_empty() { return }
        let work = cx.background_executor().spawn(async move {
            let read = |picture: Picture| match picture {
                Picture::Bytes(bytes) => Ok(bytes),
                Picture::File(path) => std::fs::read(&path).map_err(|err| format!("无法读取 {}：{err}", path.display())),
            };
            pictures.into_iter().map(|picture| read(picture).and_then(crate::picture::prepare)).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let prepared = work.await;
            this.update(cx, |this, cx| {
                // Another library, or another paper's conversation by now: the pictures were not meant for it.
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library && (!about_paper || l.selected == selected)) else { return };
                let chat = lib.chat_mut(about_paper);
                chat.attaching = chat.attaching.saturating_sub(prepared.len());
                let mut failed = None;
                for picture in prepared {
                    match picture {
                        Ok(image) => chat.attachments.push(ChatImage { id: new_id("I"), image: Arc::new(image) }),
                        Err(err) => failed = Some(err),
                    }
                }
                if let Some(err) = failed { this.fail(err, cx) }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn ask(&mut self, about_paper: bool, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        let Some(lib) = self.lib.as_mut() else { return };
        let paper_id = lib.selected.clone();
        let chat = if about_paper { &mut lib.paper_chat } else { &mut lib.library_chat };
        let question = chat.input.read(cx).text().trim().to_string();
        let begin_review = !about_paper && question.strip_prefix("/review").is_some_and(|rest| rest.starts_with(char::is_whitespace));
        if (question.is_empty() && chat.attachments.is_empty() && chat.quotes.is_empty()) || chat.busy { return }
        chat.error.clear();
        chat.note.clear();
        if chat.attaching > 0 {
            chat.error = "图片还在读取，稍等一下再发送。".into();
            return cx.notify();
        }
        // `/search …` and the like stand for a longer request; the bubble shows what was typed.
        let prompt = match ai::read_input(if question.is_empty() && !chat.quotes.is_empty() { "这段原文说了什么？在这篇文献里起什么作用？" } else { &question }, &self.settings.commands) {
            Ok(ai::Input::Ask(prompt)) => prompt,
            Ok(ai::Input::NewChat) => {
                chat.reset(cx);
                return cx.notify();
            }
            Err(err) => {
                chat.error = err;
                return cx.notify();
            }
        };
        chat.history = None;

        // A changed context or protocol starts the wire again from the text of what was said:
        // the model's own turns are only valid in the conversation that produced them.
        let replay = chat.ctx.is_none() || chat.protocol != Some(profile.protocol);
        if chat.ctx.is_none() {
            let scope = match (about_paper, &paper_id) {
                (true, Some(id)) => Scope::Paper(id.clone()),
                (true, None) => return,
                _ => Scope::Library,
            };
            let fulltext = match lib.store.all_fulltext() {
                Ok(fulltext) => fulltext,
                Err(err) => {
                    chat.error = format!("{err:#}");
                    return cx.notify();
                }
            };
            // A question about one paper reads that paper; one about the library reads the current list.
            let papers: Vec<&Paper> = match &scope {
                Scope::Paper(_) => lib.papers.iter().collect(),
                // As `Library::scope_papers`, field by field: the conversation is held meanwhile.
                Scope::Library if !lib.asked.is_empty() => lib.papers.iter().filter(|p| lib.asked.contains(&p.id)).collect(),
                Scope::Library => lib.shown.iter().map(|&i| &lib.papers[i]).collect(),
            };
            if papers.is_empty() {
                chat.error = "当前列表里没有文献。先添加文献，或清空筛选条件。".into();
                return cx.notify();
            }
            // A conversation taken up from the history keeps the labels its answers already use.
            let mut ctx = ai::build_context(&lib.info.name, &lib.info.topic, &papers, &lib.fields, &fulltext, &scope, &prompt, &chat.known);
            // The tools reach every paper of the library, so every paper can be cited.
            ctx.add_sources(&lib.papers);
            chat.ctx = Some(ctx);
        }
        if replay {
            // Questions with their pictures, as they were sent (what a slash command stood for then); the pieces of one answer as one turn.
            let mut wire: Vec<Value> = vec![];
            let mut answer = String::new();
            for item in &chat.messages {
                match item {
                    ChatItem::User { text, images, prompt } => {
                        if !answer.is_empty() { wire.push(ai::assistant_message(&std::mem::take(&mut answer))) }
                        // A question from before the request was kept is read again as it would be now.
                        let sent = if prompt.is_empty() { ai::expand_slash(text) } else { prompt.clone() };
                        wire.push(ai::user_message_with(profile.protocol, &sent, &wire_images(images)));
                    }
                    ChatItem::Answer(text) if !text.is_empty() => {
                        if !answer.is_empty() { answer.push_str("\n\n") }
                        answer.push_str(text);
                    }
                    _ => {}
                }
            }
            if !answer.is_empty() { wire.push(ai::assistant_message(&answer)) }
            chat.wire = wire;
            chat.protocol = Some(profile.protocol);
        }
        // The passages selected in a PDF go first, written as the answers cite: the labels are known by now.
        let quotes = std::mem::take(&mut chat.quotes);
        let quoted: Vec<String> = quotes.iter().filter_map(|quote| Some(ai::quoted_passage(&chat.sources().iter().find(|source| source.paper_id == quote.paper_id)?.label, quote.page, &quote.text))).collect();
        let (prompt, question) = if quoted.is_empty() {
            (prompt, question)
        } else {
            (format!("我在文献的 PDF 里选中了下面的原文，问题是就它提的；引用它时沿用这里的出处写法。\n{}\n\n{prompt}", quoted.join("\n")), [quoted.join("\n"), question].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n"))
        };
        // The record, not what the model says of itself, is where the review stands: each question of a review's conversation says so.
        // Only what steers the writing goes along (the sections by name: their text was said in this conversation, and sending it with every question would send it many times over).
        let prompt = match chat.review.as_ref().filter(|_| !about_paper) {
            Some(review) => format!("{prompt}\n\n{}", crate::chat::review_note(review)),
            None => prompt,
        };
        let images = std::mem::take(&mut chat.attachments);
        chat.wire.push(ai::user_message_with(profile.protocol, &prompt, &wire_images(&images)));
        // Kept with the question, so the conversation taken up later asks what was asked now, whatever the command says by then.
        let sent = if prompt == question { String::new() } else { prompt };
        chat.messages.push(ChatItem::User { text: question, images, prompt: sent });
        chat.steps = 0;
        chat.started = Some(Instant::now());
        let sent = self.chat_step(about_paper, cx);
        if let Some(lib) = self.lib.as_mut() {
            let chat = if about_paper { &mut lib.paper_chat } else { &mut lib.library_chat };
            if sent {
                chat.input.update(cx, |input, cx| input.set_text("", cx));
            } else {
                // Nothing was sent: the question stays in the box, its pictures beside it, to be asked again.
                chat.wire.pop();
                if let Some(ChatItem::User { images, .. }) = chat.messages.pop() { chat.attachments = images }
                chat.quotes = quotes;
                if chat.messages.is_empty() { chat.ctx = None }
            }
        }
        // Kept from the question on, so that closing the window mid-answer does not lose it.
        if sent {
            self.save_chat(about_paper, cx);
            // The transcript is saved first so the review and its conversation
            // can be associated atomically, including across application restarts.
            if begin_review { self.start_review(cx) }
        }
        cx.notify();
    }

    /// Send the conversation to the model and follow its turn: the text as
    /// it arrives, then either the end of the answer or the tools it asked
    /// for. False when the request could not be made.
    fn chat_step(&mut self, about_paper: bool, cx: &mut Context<Self>) -> bool {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        let Some(lib) = self.lib.as_mut() else { return false };
        let chat = if about_paper { &mut lib.paper_chat } else { &mut lib.library_chat };
        let Some(ctx) = chat.ctx.as_ref() else { return false };
        let request = match ai::chat_request(&profile, ctx, &chat.wire, &tools::library_catalog()) {
            Ok(request) => request,
            Err(err) => {
                chat.error = err;
                chat.end_run();
                return false;
            }
        };
        chat.messages.push(ChatItem::Answer(String::new()));
        chat.busy = true;
        chat.steps += 1;
        chat.scroll.scroll_to_bottom();
        let generation = chat.generation;
        let library = lib.info.id.clone();

        let (sender, events) = async_channel::unbounded();
        let protocol = profile.protocol;
        cx.background_executor().spawn(async move { wisp_net::stream_chat(request, protocol, sender) }).detach();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let alive = this.update(cx, |this, cx| {
                    let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return false };
                    let chat = if about_paper { &mut lib.paper_chat } else { &mut lib.library_chat };
                    if chat.generation != generation { return false }
                    // Keep following the answer while the reader is at the bottom.
                    let at_bottom = chat.scroll.offset().y <= -chat.scroll.max_offset().height + px(40.);
                    let mut calls = None;
                    let more = match event {
                        ChatEvent::Text(text) => {
                            chat.note.clear();
                            if let Some(ChatItem::Answer(answer)) = chat.messages.last_mut() { answer.push_str(&text) }
                            true
                        }
                        ChatEvent::Retrying(attempt, why) => {
                            chat.note = format!("模型服务暂时没有回应（{why}），正在重试，第 {attempt} 次…");
                            true
                        }
                        ChatEvent::Error(error) => {
                            chat.note.clear();
                            chat.error = error;
                            chat.end_run();
                            false
                        }
                        ChatEvent::Done(turn) => {
                            chat.note.clear();
                            chat.wire.extend(turn.wire());
                            if turn.calls.is_empty() {
                                chat.note = ai::stop_note(&turn.stop).unwrap_or_default().to_string();
                                chat.end_run();
                            } else if !turn.runnable() || chat.steps >= MAX_CHAT_STEPS {
                                // The calls still get an answer: a conversation that ends on an unanswered call cannot continue.
                                let (why, note) = if turn.runnable() {
                                    ("已达到单次提问的步数上限，工具没有执行。", "已达到单次提问的步数上限。可以继续提问，让它接着做。")
                                } else {
                                    ("回答被截断，工具没有执行。", ai::stop_note(&turn.stop).unwrap_or("回答没有正常结束。"))
                                };
                                let results: Vec<(String, String, bool)> = turn.calls.iter().map(|call| (call.id.clone(), why.to_string(), true)).collect();
                                chat.wire.extend(ai::tool_results(protocol, &results));
                                chat.note = note.to_string();
                                chat.end_run();
                            } else {
                                chat.drop_empty_answer();
                                calls = Some(turn.calls);
                            }
                            false
                        }
                    };
                    if at_bottom { chat.scroll.scroll_to_bottom() }
                    let ended = !chat.busy;
                    if let Some(calls) = calls { this.run_tools(about_paper, calls, cx) }
                    if ended { this.save_chat(about_paper, cx) }
                    cx.notify();
                    more
                });
                if !matches!(alive, Ok(true)) { break }
            }
        })
        .detach();
        true
    }

    /// Make the tools a turn asked for ready, show them, and run them:
    /// at once, or after the person's consent when one of them would bring
    /// papers in, take them out, or drop a saved search.
    fn run_tools(&mut self, about_paper: bool, calls: Vec<ToolCall>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = lib.chat_mut(about_paper);
        let first = chat.messages.len();
        let catalog = tools::catalog();
        let mut prepared = vec![];
        for call in calls {
            let tool = catalog.iter().find(|t| t.name == call.name);
            let line = tools::describe_call(&call.name, call.args.as_ref().unwrap_or(&Value::Null));
            chat.messages.push(ChatItem::Tool { call: line.clone(), result: None });
            // The model may name a paper by its label.
            let args = call.args.map(|mut args| {
                ai::resolve_labels(&mut args, chat.sources());
                args
            });
            // A call that cannot run as written has nothing to ask about: it fails by itself.
            let confirm = args.is_ok() && tool.is_some_and(|t| t.confirm);
            prepared.push(PreparedCall { id: call.id, name: call.name, args, writes: tool.is_some_and(|t| t.writes), confirm, line });
        }
        chat.scroll.scroll_to_bottom();
        if prepared.iter().any(|call| call.confirm) && !chat.allow_changes {
            chat.pending = Some(PendingTools { calls: prepared, first });
            return cx.notify();
        }
        self.execute_tools(about_paper, prepared, first, true, cx);
    }

    /// Run a turn's tools off the main thread, then hand the results to
    /// the model with the next step. Calls that only read run side by
    /// side; a turn that changes the library runs in the order it was
    /// given. Without the person's consent (`allowed`) the calls that asked
    /// for it are answered with a refusal and the others still run.
    fn execute_tools(&mut self, about_paper: bool, calls: Vec<PreparedCall>, first: usize, allowed: bool, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let info = lib.info.clone();
        let chat = lib.chat_mut(about_paper);
        let Some(protocol) = chat.protocol else { return };
        let sources: Arc<Vec<ai::Source>> = Arc::new(chat.sources().to_vec());
        let generation = chat.generation;
        chat.running = calls.iter().map(|call| call.id.clone()).collect();
        let wrote = calls.iter().any(|call| call.writes && (allowed || !call.confirm));
        let library = info.id.clone();
        let executor = cx.background_executor().clone();
        let tasks: Vec<gpui::Task<Vec<(String, String, bool, String)>>> = if wrote || calls.len() == 1 {
            vec![executor.spawn(async move { calls.into_iter().map(|call| run_tool(&info, &sources, call, allowed)).collect() })]
        } else {
            calls
                .into_iter()
                .map(|call| {
                    let (info, sources) = (info.clone(), sources.clone());
                    executor.spawn(async move { vec![run_tool(&info, &sources, call, allowed)] })
                })
                .collect()
        };
        cx.spawn(async move |this, cx| {
            let mut done = vec![];
            for task in tasks {
                done.extend(task.await);
            }
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return };
                // A change was made whether or not its answer is still wanted: the window reads the library again.
                let reloaded = if wrote { lib.reload() } else { Ok(()) };
                let chat = lib.chat_mut(about_paper);
                if chat.generation == generation {
                    for (n, (_, _, failed, shown)) in done.iter().enumerate() {
                        if let Some(ChatItem::Tool { result, .. }) = chat.messages.get_mut(first + n) { *result = Some((shown.clone(), *failed)) }
                    }
                    let results: Vec<(String, String, bool)> = done.into_iter().map(|(id, output, failed, _)| (id, output, failed)).collect();
                    let mut results = ai::tool_results(protocol, &results);
                    // One step is left: the model is told to answer with what it has.
                    if chat.steps + 1 >= MAX_CHAT_STEPS { results = ai::with_note(protocol, results, ai::LAST_STEP_NOTE) }
                    chat.wire.extend(results);
                    chat.running.clear();
                    if !this.chat_step(about_paper, cx) { this.save_chat(about_paper, cx) }
                }
                if let Err(err) = reloaded { this.fail(format!("{err:#}"), cx) }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Stop the answer being given. What still arrives for it is dropped;
    /// the conversation stays one that can go on: the calls in flight are
    /// answered as stopped, and the text so far stays as the model's turn.
    fn stop_chat(&mut self, about_paper: bool, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = lib.chat_mut(about_paper);
        if !chat.busy { return }
        chat.generation += 1;
        let unanswered: Vec<String> = match chat.pending.take() {
            Some(pending) => pending.calls.into_iter().map(|call| call.id).collect(),
            None => std::mem::take(&mut chat.running),
        };
        if let Some(protocol) = chat.protocol {
            if !unanswered.is_empty() {
                let results: Vec<(String, String, bool)> = unanswered.into_iter().map(|id| (id, "用户中止了这次提问，这个工具的结果没有交回。".to_string(), true)).collect();
                chat.wire.extend(ai::tool_results(protocol, &results));
            } else if let Some(ChatItem::Answer(text)) = chat.messages.last() && !text.is_empty() {
                chat.wire.push(ai::assistant_message(text));
            }
        }
        for item in chat.messages.iter_mut() {
            if let ChatItem::Tool { result, .. } = item && result.is_none() { *result = Some(("已中止".into(), true)) }
        }
        chat.note = "已停止。".into();
        chat.end_run();
        self.save_chat(about_paper, cx);
        cx.notify();
    }

    /// Keep the conversation in the library's history, as it stands.
    fn save_chat(&mut self, about_paper: bool, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let paper_id = match (about_paper, &lib.selected) {
            (true, Some(id)) => id.clone(),
            (true, None) => return,
            _ => String::new(),
        };
        let chat = if about_paper { &mut lib.paper_chat } else { &mut lib.library_chat };
        if !chat.messages.iter().any(|item| matches!(item, ChatItem::User { .. })) { return }
        let mut conversation = Conversation { id: chat.id.clone(), paper_id, transcript: chat.transcript(), created_at: chat.created_at.clone(), ..Default::default() };
        let mut kept = vec![];
        let result = lib.store.save_conversation(&mut conversation).and_then(|()| {
            for item in &chat.messages {
                let ChatItem::User { images, .. } = item else { continue };
                for picture in images.iter().filter(|picture| !chat.stored_images.contains(&picture.id)) {
                    lib.store.save_conversation_image(&conversation.id, &picture.id, picture.image.format.mime_type(), &picture.image.bytes)?;
                    kept.push(picture.id.clone());
                }
            }
            Ok(())
        });
        chat.stored_images.extend(kept);
        (chat.id, chat.created_at) = (conversation.id, conversation.created_at);
        chat.kept = result.is_ok();
        if let Err(err) = result { self.fail(format!("这次对话没能保存：{err:#}"), cx) }
    }

    /// Take up a conversation from the history: what was said is shown
    /// again, and the next question continues it. The model is given the
    /// library as it is now, under the labels the old answers use.
    fn open_chat(&mut self, about_paper: bool, id: &str, cx: &mut Context<Self>) {
        self.open_chat_said(about_paper, id, cx);
        if !about_paper && let Some(Err(err)) = self.lib.as_mut().map(|lib| lib.load_review()) { self.fail(format!("无法读取综述记录：{err:#}"), cx) }
    }

    fn open_chat_said(&mut self, about_paper: bool, id: &str, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let conversation = match lib.store.conversation(id) {
            Ok(Some(conversation)) => conversation,
            Ok(None) => return self.fail("这次对话已经不在了。", cx),
            Err(err) => return self.fail(format!("{err:#}"), cx),
        };
        if !about_paper { lib.asked.clear() }
        let chat = if about_paper { &mut lib.paper_chat } else { &mut lib.library_chat };
        chat.reset(cx);
        for item in conversation.transcript.items {
            chat.messages.push(match item {
                ai::Said::User { text, images, prompt } => {
                    let mut pictures = vec![];
                    for image_id in images {
                        // A picture that cannot be read back is left out; the question stays.
                        let Ok(Some((media_type, data))) = lib.store.conversation_image(&conversation.id, &image_id) else { continue };
                        let Some(format) = crate::picture::format_of(&media_type) else { continue };
                        chat.stored_images.insert(image_id.clone());
                        pictures.push(ChatImage { id: image_id, image: Arc::new(Image::from_bytes(format, data)) });
                    }
                    ChatItem::User { text, images: pictures, prompt }
                }
                ai::Said::Answer { text } => ChatItem::Answer(text),
                ai::Said::Tool { call, result, failed } => ChatItem::Tool { call, result: Some((result, failed)) },
            });
        }
        chat.known = conversation.transcript.sources;
        (chat.id, chat.created_at) = (conversation.id, conversation.created_at);
        chat.kept = true;
        chat.scroll = ScrollHandle::new();
        chat.scroll.scroll_to_bottom();
        cx.notify();
    }

    // ---- dialogs: building and submitting (rendering is in dialogs.rs)

    pub fn modal_input(&mut self, placeholder: &'static str, text: &str, multiline: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextInput> {
        let input = cx.new(|cx| {
            let input = TextInput::new(placeholder, cx);
            if multiline { input.multiline(3).with_text(text) } else { input.with_text(text) }
        });
        if !multiline {
            let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if *event == InputEvent::Submit { this.run(Cmd::SubmitModal, window, cx) }
            });
            self.modal_subscriptions.push(subscription);
        }
        input
    }

    /// Open 按题目补全 for a paper. Its title goes into the box unless it is
    /// only the name of its file; then the first page of its PDF is read
    /// for one, which the person looks over before anything is asked.
    fn complete_dialog(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        let Some(lib) = &self.lib else { return };
        let Some(paper) = lib.paper(&id).cloned() else { return };
        let files: Vec<PaperFile> = lib.paper_files(&id).into_iter().cloned().collect();
        let placeholder = complete::placeholder_title(&paper, &files);
        let pdfs: Vec<PathBuf> = files.iter().filter(|file| file.format == "pdf").map(|file| lib.store.file_path(file)).collect();
        let library = lib.info.id.clone();
        let title = self.modal_input("文献的题目", if placeholder { "" } else { &paper.title }, true, window, cx);
        title.update(cx, |input, _| input.min_lines = 2);
        let first_author = paper.authors.split(';').next().unwrap_or("").trim().to_string();
        let author = self.modal_input("一位作者的姓名，可不填", &first_author, false, window, cx);
        let year = self.modal_input("例如 2019，可不填", &paper.year.map(|y| y.to_string()).unwrap_or_default(), false, window, cx);
        title.read(cx).focus(window);
        self.modal = Some(Modal::Complete(Box::new(CompleteForm { paper_id: id.clone(), seen: paper.updated_at.clone(), title, author, year, placeholder, guessed: false, busy: false, found: None, held: vec![], chosen: None, fields: vec![] })));
        cx.notify();
        if !placeholder || pdfs.is_empty() { return }
        cx.spawn(async move |this, cx| {
            let shown = cx.background_executor().spawn(async move { pdfs.into_iter().find_map(|path| pdftext::title_guess(std::fs::read(path).ok()?)) }).await;
            this.update(cx, |this, cx| {
                if this.lib.as_ref().is_none_or(|l| l.info.id != library) { return }
                let (Some(Modal::Complete(form)), Some(shown)) = (this.modal.as_mut(), shown) else { return };
                // Only into a box the person has not written in meanwhile.
                if form.paper_id != id || !form.title.read(cx).text().trim().is_empty() { return }
                form.title.update(cx, |input, cx| input.set_text(&shown, cx));
                form.guessed = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 按题目补全, the first step: ask the sources about the title in the box.
    fn complete_find(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Complete(form)) = self.modal.as_mut() else { return };
        if form.busy { return }
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let title = text(&form.title);
        if title.is_empty() {
            self.modal_error = "写上题目再查找。".into();
            return;
        }
        let year = match text(&form.year) {
            year if year.is_empty() => None,
            year => match year.parse::<i32>().ok().filter(|y| (1800..=2200).contains(y)) {
                Some(year) => Some(year),
                None => {
                    self.modal_error = "年份应为数字，例如 2019；不确定可以留空。".into();
                    return;
                }
            },
        };
        let query = complete::Query { title, author: text(&form.author), year };
        form.busy = true;
        let paper_id = form.paper_id.clone();
        let library = self.lib.as_ref().map(|l| l.info.id.clone()).unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { complete::find(&query, &wisp_net::get_text) }).await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_ref().filter(|l| l.info.id == library) else { return };
                let Some(Modal::Complete(form)) = this.modal.as_mut().filter(|modal| matches!(modal, Modal::Complete(form) if form.paper_id == paper_id)) else { return };
                form.busy = false;
                // A record the library holds as another paper cannot be written into this one.
                form.held = found.candidates.iter().map(|candidate| lib.store.existing_paper(&candidate.paper).ok().flatten().filter(|id| *id != paper_id).map(|id| (id.clone(), lib.paper(&id).or_else(|| lib.trash.iter().map(|entry| &entry.paper).find(|paper| paper.id == id)).map(|paper| paper.title.clone()).unwrap_or_default()))).collect();
                if found.asked.iter().all(|asked| !asked.error.is_empty()) { this.modal_error = "几个来源都没有查成，见下面的原因。检查网络或「设置 › 网络」里的代理后再试。".into() }
                form.chosen = None;
                form.found = Some(found);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 按题目补全, the last step: write the fields taken from the record
    /// looked at into the paper.
    fn complete_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Complete(form)) = self.modal.as_ref() else { return };
        let (Some(at), Some(found)) = (form.chosen, form.found.as_ref()) else { return };
        let (id, candidate, fields) = (form.paper_id.clone(), found.candidates[at].clone(), form.fields.clone());
        let problem = if form.held.get(at).is_some_and(Option::is_some) {
            Some("文献库里已经有这条记录了，不能再写进这一篇。")
        } else if fields.is_empty() {
            Some("没有勾选要写入的字段。")
        } else if self.lib.as_ref().and_then(|lib| lib.paper(&id)).is_none_or(|paper| paper.updated_at != form.seen) {
            Some("这篇文献在查找期间被改过。关掉这个窗口重新打开，按它现在的内容再核对。")
        } else {
            None
        };
        if let Some(problem) = problem {
            self.modal_error = problem.into();
            return;
        }
        if !self.edit_paper(&id, cx, |paper| complete::apply(paper, &candidate, &fields)) { return }
        self.run(Cmd::CloseModal, window, cx);
        self.say(format!("已补全：{}", self.lib.as_ref().and_then(|lib| lib.paper(&id)).map(|paper| paper.title.clone()).unwrap_or_default()), cx);
    }

    fn library_dialog(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        let current = editing.as_ref().and_then(|id| self.libraries.iter().find(|l| &l.id == id).or(self.lib.as_ref().map(|l| &l.info)).cloned());
        let (name, topic, color) = current.map_or((String::new(), String::new(), libraries::COLORS[self.libraries.len() % libraries::COLORS.len()].to_string()), |l| (l.name, l.topic, l.color));
        let name = self.modal_input("例如：植物根尖单细胞", &name, false, window, cx);
        let topic = self.modal_input("这个文献库收集什么？关注哪些问题？（会作为 AI 提问的背景）", &topic, true, window, cx);
        name.read(cx).focus(window);
        self.modal = Some(Modal::Library { editing, name, topic, color });
    }

    fn paper_dialog(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        let Some(lib) = &self.lib else { return };
        let paper = id.as_ref().and_then(|id| lib.paper(id)).cloned().unwrap_or_default();
        let fields = lib.fields.clone();
        let title = self.modal_input("文献题目", &paper.title, true, window, cx);
        title.update(cx, |input, _| input.min_lines = 2);
        let abstract_text = self.modal_input("摘要", &paper.abstract_text, true, window, cx);
        abstract_text.update(cx, |input, _| input.min_lines = 4);
        let mut form = PaperForm {
            id,
            title,
            authors: self.modal_input("作者，用分号分隔：Zhang, Wei; Li, Na", &paper.authors, false, window, cx),
            year: self.modal_input("例如 2024", &paper.year.map(|y| y.to_string()).unwrap_or_default(), false, window, cx),
            journal: self.modal_input("期刊或会议", &paper.journal, false, window, cx),
            doi: self.modal_input("10.xxxx/…", &paper.doi, false, window, cx),
            pmid: self.modal_input("PubMed 编号", &paper.pmid, false, window, cx),
            url: self.modal_input("https://…", &paper.url, false, window, cx),
            tags: self.modal_input("标签，用逗号分隔", &paper.tags.join(", "), false, window, cx),
            abstract_text,
            fields: vec![],
            selects: BTreeMap::new(),
        };
        for field in fields {
            let values = paper.fields.get(&field.id).cloned().unwrap_or_default();
            match field.kind {
                FieldKind::Select => {
                    if let Some(value) = values.first() { form.selects.insert(field.id.clone(), value.clone()); }
                }
                FieldKind::Text => form.fields.push((field.id.clone(), self.modal_input("", &values.join(" "), false, window, cx))),
                FieldKind::Tags => form.fields.push((field.id.clone(), self.modal_input("多个值用逗号分隔", &values.join(", "), false, window, cx))),
            }
        }
        form.title.read(cx).focus(window);
        self.modal = Some(Modal::Paper(Box::new(form)));
    }

    fn fields_dialog(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        let field = editing.as_ref().and_then(|id| self.lib.as_ref()?.fields.iter().find(|f| &f.id == id).cloned()).unwrap_or(FieldDef { in_table: true, kind: FieldKind::Tags, ..Default::default() });
        let name = self.modal_input("词条名称，例如：物种、研究方法、所属课题", &field.name, false, window, cx);
        let options = self.modal_input("可选值，用逗号分隔（单选必填）", &field.options.join(", "), false, window, cx);
        self.modal = Some(Modal::Fields { editing, name, options, kind: field.kind, in_table: field.in_table });
    }

    fn search_dialog(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.modal_subscriptions.clear();
        self.modal_error.clear();
        let search = editing.as_ref().and_then(|id| self.lib.as_ref()?.searches.iter().find(|s| &s.id == id).cloned()).unwrap_or(SavedSearch { kind: "pubmed".into(), max_results: 50, enabled: true, ..Default::default() });
        let name = self.modal_input("名称，例如：根尖单细胞（可以留空）", &search.name, false, window, cx);
        let query = self.modal_input("检索式", &search.query, true, window, cx);
        let max = self.modal_input("50", &search.max_results.to_string(), false, window, cx);
        self.modal = Some(Modal::Search { editing, kind: search.kind, name, query, max });
    }

    /// Ask the model for searches on the guide's topic, then try each one so
    /// the person can judge them by what they find.
    fn guide_generate(&mut self, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.guide.busy { return }
        let keywords = lib.guide.keywords.read(cx).text().trim().to_string();
        lib.guide.error.clear();
        if keywords.is_empty() && lib.info.topic.trim().is_empty() {
            lib.guide.error = "先写下研究主题或几个关键词。".into();
            return cx.notify();
        }
        let (system, prompt) = ai::query_prompt(&lib.info.name, &lib.info.topic, &keywords, &lib.guide.kinds);
        let request = match ai::plain_request(&profile, &system, &prompt) {
            Ok(request) => request,
            Err(err) => {
                lib.guide.error = err;
                return cx.notify();
            }
        };
        lib.guide.busy = true;
        lib.guide.round += 1;
        let (round, library, protocol) = (lib.guide.round, lib.info.id.clone(), profile.protocol);
        cx.notify();
        let work = cx.background_executor().spawn(async move { wisp_net::complete(request, protocol).and_then(|reply| ai::parse_queries(&reply)) });
        cx.spawn(async move |this, cx| {
            let ideas = work.await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library && l.guide.round == round) else { return };
                lib.guide.busy = false;
                match ideas {
                    Err(err) => lib.guide.error = err,
                    Ok(ideas) => {
                        lib.guide.ideas = ideas.into_iter().map(|idea| GuideIdea { idea, chosen: false, trial: None }).collect();
                        let trials: Vec<(usize, Value)> = lib.guide.ideas.iter().enumerate().map(|(i, entry)| (i, serde_json::json!({"source": entry.idea.source, "query": entry.idea.query, "limit": 8}))).collect();
                        for (index, args) in trials {
                            this.library_tool("search_online", args, cx, move |this, result, _| {
                                // Only for the ideas this trial was started for.
                                if let Some(lib) = this.lib.as_mut().filter(|l| l.guide.round == round) && let Some(entry) = lib.guide.ideas.get_mut(index) { entry.trial = Some(result) }
                            });
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// While the app runs, look once a minute for saved searches whose turn
    /// has come, in every library that updates on its own, and run them.
    /// What was missed while the app was closed is due at the first look.
    fn watch_searches(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(20)).await;
            loop {
                let Ok(root) = this.update(cx, |this, _| this.root.clone()) else { break };
                // One look at a time: the next waits for this one's runs to end.
                let outcome = cx.background_executor().spawn(async move { tools::call(&root, "sync_searches", &serde_json::json!({}), &wisp_net::get_text, &wisp_net::download) }).await;
                let alive = this.update(cx, |this, cx| {
                    let Ok(outcome) = outcome else { return };
                    let runs = outcome["runs"].as_array().cloned().unwrap_or_default();
                    if runs.is_empty() { return }
                    this.libraries = libraries::list(&this.root);
                    // The open library shows what came in; the others show it when opened.
                    let Some(open) = this.lib.as_ref().map(|l| l.info.id.clone()) else { return cx.notify() };
                    let mine: Vec<&Value> = runs.iter().filter(|run| run["libraryId"] == open.as_str()).collect();
                    if mine.is_empty() { return }
                    this.change(cx, |_| Ok(()));
                    let added: u64 = mine.iter().filter_map(|run| run["added"].as_u64()).sum();
                    if added > 0 { this.say(format!("自动更新：新增 {added} 篇待判断的文献"), cx) }
                });
                if alive.is_err() { break }
                cx.background_executor().timer(std::time::Duration::from_secs(60)).await;
            }
        })
        .detach();
    }

    /// Start asking the assistant about the pending papers it has not judged
    /// against the goal as it is now (with `decided`, the decided ones),
    /// several at a time (设置 › AI says how many, and with which model).
    /// What it says is saved as a suggestion; the decision stays with the
    /// person, and a decided paper it sees otherwise comes back to the queue.
    fn screen_start(&mut self, decided: bool, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Screen);
        let workers = self.settings.screen_workers;
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.screen.running() { return }
        lib.screen.error.clear();
        // The key and address are checked once here, not once per paper.
        if let Err(err) = ai::plain_request(&profile, "s", "p") {
            lib.screen.error = err;
            return cx.notify();
        }
        let mut todo: Vec<String> = lib.screen_unasked(decided).iter().map(|p| p.id.clone()).collect();
        if todo.is_empty() {
            lib.screen.error = if decided { "已判断的文献都有按当前目标的建议了。" } else { "待判断的文献都已经有按当前目标的建议了。" }.into();
            return cx.notify();
        }
        todo.reverse();
        let screen = &mut lib.screen;
        screen.run += 1;
        (screen.total, screen.done, screen.failed, screen.streak, screen.todo) = (todo.len(), 0, 0, 0, todo);
        screen.reading.clear();
        screen.model = format!("{} · {}", profile.title(), profile.model);
        let run = screen.run;
        for _ in 0..workers {
            self.screen_next(run, cx);
        }
        cx.notify();
    }

    /// Ask about the next paper of the run, and about the one after it once
    /// the answer is in. A paper without a usable answer is counted and
    /// left waiting; several of them in a row end the run.
    fn screen_next(&mut self, run: u64, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Screen);
        let Some(lib) = self.lib.as_mut().filter(|l| l.screen.run == run) else { return };
        let Some(id) = lib.screen.todo.pop() else { return };
        let Some(paper) = lib.paper(&id).cloned() else { return self.screen_next(run, cx) };
        let pages = lib.store.paper_pages(&id).ok().flatten();
        let (system, prompt, basis) = ai::screen_prompt(&lib.info.topic, &lib.info.criteria, &paper, pages.as_deref());
        let author = format!("{} · {}", profile.title(), profile.model);
        // What it is judged against, kept with the answer: the goal may change while it is read.
        let criteria = Some(lib.info.criteria.clone());
        // Nothing to read: say so without asking, rather than have the model guess from a title.
        if basis == "none" {
            let mut suggestion = Suggestion { paper_id: id, verdict: "uncertain".into(), reason: "这篇文献没有摘要，也没有保存全文，无法判断。".into(), basis: basis.into(), author, criteria, ..Default::default() };
            let saved = lib.store.save_suggestion(&mut suggestion);
            lib.screen.done += 1;
            if saved.is_ok() { lib.suggestions.insert(suggestion.paper_id.clone(), suggestion); }
            return self.screen_next(run, cx);
        }
        let request = match ai::plain_request(&profile, &system, &prompt) {
            Ok(request) => request,
            Err(err) => {
                lib.screen.error = err;
                lib.screen.todo.clear();
                return;
            }
        };
        lib.screen.reading.push(id);
        let (library, protocol) = (lib.info.id.clone(), profile.protocol);
        let work = cx.background_executor().spawn(async move {
            let judged = wisp_net::complete(request, protocol).and_then(|reply| ai::parse_judgement(&reply, &paper, pages.as_deref()));
            (paper, judged)
        });
        cx.spawn(async move |this, cx| {
            let (paper, judged) = work.await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library && l.screen.run == run) else { return };
                lib.screen.reading.retain(|reading| *reading != paper.id);
                lib.screen.done += 1;
                let saved = judged.and_then(|j| {
                    let mut suggestion = Suggestion { paper_id: paper.id.clone(), verdict: j.verdict, reason: j.reason, excerpt: j.excerpt, page: j.page, basis: basis.into(), confidence: j.confidence, author, criteria, ..Default::default() };
                    lib.store.save_suggestion(&mut suggestion).map(|()| suggestion).map_err(|e| format!("{e:#}"))
                });
                match saved {
                    Ok(suggestion) => {
                        lib.screen.streak = 0;
                        lib.suggestions.insert(suggestion.paper_id.clone(), suggestion);
                        this.screen_next(run, cx);
                    }
                    Err(err) => {
                        lib.screen.failed += 1;
                        lib.screen.streak += 1;
                        if lib.screen.streak >= SCREEN_GIVE_UP {
                            lib.screen.error = format!("连续 {SCREEN_GIVE_UP} 篇没有得到可用的回答，已停止。最后一篇「{}」：{err}", paper.title);
                            lib.screen.todo.clear();
                        } else {
                            lib.screen.error = format!("「{}」没有判断成，留待下一轮：{err}", paper.title);
                            this.screen_next(run, cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- writing

    /// Start the model on the full texts: sorting those the rules sorted
    /// into parts, or reading the pages of those that gave no text.
    fn write_run_start(&mut self, reading: bool, only: Option<String>, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Structure);
        let workers = self.settings.screen_workers;
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.writing.sorting.running() { return }
        lib.writing.sorting.error.clear();
        if let Err(err) = ai::plain_request(&profile, "s", "p") {
            lib.writing.sorting.error = err;
            return cx.notify();
        }
        // Pages to look at: only a PDF has them. Parts to sort: the texts the rules had to guess at.
        let mut todo: Vec<String> = lib.writing.structures.iter().filter(|(id, _)| only.as_ref().is_none_or(|only| only == *id)).filter(|(id, info)| if reading { (info.method == "none" || info.read_in_part()) && lib.files.iter().any(|f| f.id == **id && f.format == "pdf") } else { info.worth_asking() }).map(|(id, _)| id.clone()).collect();
        if todo.is_empty() { return }
        todo.reverse();
        let sorting = &mut lib.writing.sorting;
        sorting.run += 1;
        (sorting.reading, sorting.total, sorting.done, sorting.failed, sorting.streak, sorting.todo) = (reading, todo.len(), 0, 0, 0, todo);
        sorting.cut.clear();
        sorting.model = format!("{} · {}", profile.title(), profile.model);
        let run = sorting.run;
        // A file's pages are read one after another; two files at a time is as much as a service takes kindly.
        for _ in 0..if reading { workers.min(2) } else { workers } {
            self.write_run_next(run, cx);
        }
        cx.notify();
    }

    /// Work on the next file of the run, and on the one after it once this is done.
    fn write_run_next(&mut self, run: u64, cx: &mut Context<Self>) {
        enum Made {
            Sorted(Vec<structure::Sentence>),
            /// All the pages read so far, and how many the file has.
            Read(Vec<String>, usize),
        }
        let profile = self.settings.profile_for_or_default(Task::Structure);
        let Some(lib) = self.lib.as_mut().filter(|l| l.writing.sorting.run == run) else { return };
        let Some(file_id) = lib.writing.sorting.todo.pop() else { return };
        let Some(file) = lib.files.iter().find(|f| f.id == file_id).cloned() else { return self.write_run_next(run, cx) };
        let (title, summary) = lib.paper(&file.paper_id).map(|p| (p.title.clone(), p.abstract_text.clone())).unwrap_or_default();
        let author = format!("{} · {}", profile.title(), profile.model);
        let (library, protocol, reading) = (lib.info.id.clone(), profile.protocol, lib.writing.sorting.reading);
        let path = lib.store.file_path(&file);
        let pages = lib.store.fulltext_pages(&file.id).ok().flatten().and_then(Result::ok).unwrap_or_default();
        // A file read in part goes on from the first page not read.
        let before = if reading && lib.writing.structures.get(&file.id).is_some_and(StructureInfo::read_in_part) { pages.clone() } else { vec![] };
        lib.writing.sorting.working.push(file_id.clone());
        let name = title.clone();
        let work = cx.background_executor().spawn(async move {
            if reading {
                let mut pages = before;
                let (pictures, count) = crate::pdf::page_pictures(&path, pages.len(), READ_PAGES)?;
                for picture in pictures {
                    let request = ai::read_page_request(&profile, ai::ImageData { media_type: "image/png".into(), data: ai::base64(&picture) })?;
                    pages.push(ai::page_reading(&wisp_net::complete(request, protocol)?));
                }
                if pages.iter().all(|page| page.trim().is_empty()) { return Err("模型在页面上没有读到文字。".to_string()) }
                Ok(Made::Read(pages, count))
            } else {
                let blocks = structure::blocks(&pages);
                let request = ai::plain_request(&profile, structure::OUTLINE_SYSTEM, &structure::outline_prompt(&name, &blocks))?;
                let read = structure::parse_outline(&wisp_net::complete(request, protocol)?, &blocks, &summary)?;
                // What stands under a heading that names its part stays there; the model decides the rest.
                let (labels, sure) = structure::assign_sure(&blocks, &summary);
                Ok(Made::Sorted(structure::sentences(&blocks, &structure::merge(&labels, &sure, &read))))
            }
        });
        cx.spawn(async move |this, cx| {
            let made = work.await;
            this.update(cx, |this, cx| {
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library && l.writing.sorting.run == run) else { return };
                lib.writing.sorting.working.retain(|working| *working != file_id);
                lib.writing.sorting.done += 1;
                let saved = made.and_then(|made| {
                    match made {
                        Made::Sorted(sentences) => lib.store.save_model_structure(&file_id, &author, &sentences),
                        Made::Read(pages, count) => {
                            // A file longer than one run reads says how much of it was read.
                            if pages.len() < count { lib.writing.sorting.cut.push((title.clone(), pages.len(), count)) }
                            lib.store.save_read_pages(&file_id, pages, &author, count)
                        }
                    }
                    .map_err(|e| format!("{e:#}"))
                });
                let sorting = &mut lib.writing.sorting;
                match saved {
                    Ok(()) => {
                        sorting.streak = 0;
                        // A paper whose pages were read has a text now, and says so in the list.
                        let _ = lib.reload();
                        lib.texts_changed();
                        this.write_run_next(run, cx);
                        this.write_texts_changed(cx);
                    }
                    Err(err) => {
                        sorting.failed += 1;
                        sorting.streak += 1;
                        if sorting.streak >= SCREEN_GIVE_UP {
                            sorting.error = format!("连续 {SCREEN_GIVE_UP} 篇没有成功，已停止。最后一篇「{title}」：{err}");
                            sorting.todo.clear();
                        } else {
                            sorting.error = format!("「{title}」没有整理成：{err}");
                            this.write_run_next(run, cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Look for the open full text of one paper, or of the papers of the
    /// current list that have no PDF and a number to be looked up by. A
    /// round under way takes more papers in.
    pub(crate) fn fetch_start(&mut self, only: Option<String>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.fetching.as_ref().is_some_and(|run| run.stopped && run.running()) { return self.say("正在结束当前请求，请稍后再开始新的查找。", cx) }
        let known = wisp_core::openaccess::lookable;
        let lacking = |paper: &Paper| matches!(paper.fulltext.as_str(), "missing" | "linked");
        let wanted: Vec<String> = match &only {
            Some(id) => lib.paper(id).filter(|paper| lacking(paper)).map(|paper| paper.id.clone()).into_iter().collect(),
            None => lib.shown.iter().map(|&i| &lib.papers[i]).filter(|paper| lacking(paper) && known(paper)).map(|paper| paper.id.clone()).collect(),
        };
        if let Some(paper) = only.as_deref().and_then(|id| lib.paper(id)) && !known(paper) { return self.fail("这篇文献没有 DOI、PMID 或 arXiv 编号，无从查起。先在「编辑信息与词条」里补上编号。", cx) }
        if wanted.is_empty() { return self.say("当前列表里没有可以查找的文献：要么已经有 PDF，要么没有 DOI、PMID 或 arXiv 编号。", cx) }
        let fresh = lib.fetching.as_ref().is_none_or(|run| !run.running());
        if fresh { lib.fetching = Some(FetchRun { run: lib.fetching.as_ref().map_or(0, |run| run.run) + 1, ..Default::default() }) }
        let run = lib.fetching.as_mut().unwrap();
        let mut added = 0;
        for id in wanted.into_iter().rev() {
            if run.todo.contains(&id) || run.working.contains(&id) || run.rows.iter().any(|row| row.paper_id == id) { continue }
            run.todo.insert(0, id);
            added += 1;
        }
        run.total += added;
        let id = run.run;
        for _ in run.working.len()..FETCH_WORKERS {
            self.fetch_next(id, cx);
        }
        cx.notify();
    }

    /// Ask about the next paper of the round, and about the one after it once this is answered.
    fn fetch_next(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let Some(run) = lib.fetching.as_mut().filter(|run| run.run == id) else { return };
        let Some(paper_id) = run.todo.pop() else { return };
        run.working.push(paper_id.clone());
        let title = lib.paper(&paper_id).map(|paper| paper.title.clone()).unwrap_or_default();
        self.library_tool("find_pdf", serde_json::json!({"paperId": paper_id}), cx, move |this, result, cx| {
            let Some(lib) = this.lib.as_mut() else { return };
            let Some(run) = lib.fetching.as_mut().filter(|run| run.run == id) else { return };
            run.working.retain(|working| *working != paper_id);
            let found = result.as_ref().ok().map(|value| value["papers"][0].clone()).unwrap_or_default();
            let outcome = found["outcome"].as_str().unwrap_or("failed").to_string();
            let textless = found["textReason"].is_string();
            let detail = match (&result, outcome.as_str()) {
                (Err(err), _) => err.clone(),
                (_, "saved") => {
                    let version = wisp_core::openaccess::version_label(found["version"].as_str().unwrap_or(""));
                    let from = [found["source"].as_str().unwrap_or(""), version].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
                    match found["textReason"].as_str() {
                        Some(reason) => format!("来自 {from}。没读出文字：{reason}"),
                        None => format!("来自 {from}，读出 {} 页文字", found["pages"].as_u64().unwrap_or(0)),
                    }
                }
                // Nothing found and nothing gone wrong: the outcome says it all.
                (_, "not_found") => found["reason"].as_str().filter(|reason| !reason.starts_with("没有找到开放获取的全文")).unwrap_or("").to_string(),
                _ => found["reason"].as_str().unwrap_or("").to_string(),
            };
            run.rows.push(FetchRow { paper_id: paper_id.clone(), title, outcome: outcome.clone(), detail, textless });
            if outcome == "saved" {
                // The paper has a PDF now: the list, the detail panel and the next question see it.
                let _ = lib.reload();
                lib.texts_changed();
                if lib.selected.as_deref() == Some(paper_id.as_str()) { lib.pdf = None }
                this.ensure_pdf(cx);
            }
            this.fetch_next(id, cx);
        });
    }

    /// Run a library tool off the main thread, on a connection of its own
    /// and with the network, and hand the result back if the same library
    /// is still open.
    pub fn library_tool(&mut self, name: &'static str, args: Value, cx: &mut Context<Self>, done: impl FnOnce(&mut Self, Result<Value, String>, &mut Context<Self>) + 'static) {
        let Some(lib) = self.lib.as_ref() else { return };
        let info = lib.info.clone();
        let library = info.id.clone();
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let mut store = Store::open(&info.dir).map_err(|e| format!("{e:#}"))?;
            tools::call_in(&mut store, &info, name, &args, &wisp_net::get_text, &wisp_net::download)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                if this.lib.as_ref().is_none_or(|l| l.info.id != library) { return }
                done(this, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn submit_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = |input: &Entity<TextInput>, cx: &App| input.read(cx).text().trim().to_string();
        self.modal_error.clear();
        let Some(modal) = self.modal.as_mut() else { return };
        match modal {
            Modal::Confirm { cmd, .. } => {
                let cmd = cmd.clone();
                self.run(cmd, window, cx);
            }
            Modal::ImportReport => self.run(Cmd::CloseModal, window, cx),
            Modal::Section { .. } | Modal::History { .. } => self.submit_manuscript_modal(window, cx),
            Modal::ReviewEvidence(_) => self.submit_review_evidence(window, cx),
            Modal::ReviewText { .. } => self.submit_review_text(window, cx),
            Modal::Complete(form) => {
                if form.chosen.is_some() { self.complete_apply(window, cx) } else { self.complete_find(cx) }
            }
            Modal::Zotero => self.run(Cmd::PickZotero, window, cx),
            Modal::ZoteroReport => self.run(Cmd::CloseModal, window, cx),
            Modal::FetchReport => self.run(Cmd::CloseModal, window, cx),
            Modal::Library { editing, name, topic, color } => {
                let (name, topic, color) = (text(name, cx), text(topic, cx), color.clone());
                let mut created = None;
                let result = match editing.clone() {
                    None => libraries::create(&self.root, &name, &topic, &color).map(|info| created = Some(info.id)),
                    Some(id) => {
                        let Some(mut info) = self.libraries.iter().find(|l| l.id == id).cloned() else { return };
                        (info.name, info.topic, info.color) = (name, topic, color);
                        libraries::save(&info).map(|_| {
                            if let Some(lib) = self.lib.as_mut().filter(|l| l.info.id == info.id) {
                                theme::set_accent(&info.color);
                                window.set_window_title(&format!("{} — Wisp Paper", info.name));
                                // A changed topic changes what the assistant is told.
                                lib.library_chat.forget_context();
                                lib.paper_chat.forget_context();
                                lib.info = LibraryInfo { paper_count: lib.info.paper_count, opened_at: lib.info.opened_at.clone(), ..info };
                            }
                        })
                    }
                };
                match result {
                    Ok(()) => {
                        self.libraries = libraries::list(&self.root);
                        self.run(Cmd::CloseModal, window, cx);
                        // A new library opens on its guide: where to start filling it.
                        if let Some(id) = created {
                            self.run(Cmd::OpenLibrary(id), window, cx);
                            self.run(Cmd::OpenGuide(true), window, cx);
                        }
                    }
                    Err(err) => self.modal_error = format!("{err:#}"),
                }
            }
            Modal::Tag { tag, name } => {
                let (tag, name) = (tag.clone(), text(name, cx));
                if name.is_empty() {
                    self.modal_error = "标签不能没有名字。要去掉它，在标签上点右键选「去掉这个标签」。".into();
                    return;
                }
                if name == tag || self.retag(&tag, Some(&name), cx) { self.run(Cmd::CloseModal, window, cx) }
            }
            Modal::TagPapers { ids, name } => {
                let (ids, tags) = (ids.clone(), wisp_core::model::split_list(&text(name, cx)));
                if tags.is_empty() {
                    self.modal_error = "写上要添加的标签，多个用逗号或分号隔开。".into();
                    return;
                }
                let done = self.change(cx, |lib| {
                    for id in &ids {
                        let mut paper = lib.store.get_paper(id)?;
                        for tag in &tags { if !paper.tags.contains(tag) { paper.tags.push(tag.clone()) } }
                        lib.store.save_paper(&mut paper)?;
                    }
                    Ok(())
                });
                if done {
                    self.run(Cmd::CloseModal, window, cx);
                    self.say(format!("已为 {} 篇文献添加标签。", ids.len()), cx);
                }
            }
            Modal::Note { paper_id, text: input } => {
                let (paper_id, note) = (paper_id.clone(), text(input, cx));
                if note.is_empty() {
                    self.modal_error = "没有内容可以记下。".into();
                    return;
                }
                // Added after what is there: the reader's own notes are never written over.
                let mut joined = String::new();
                let saved = self.edit_paper(&paper_id, cx, |paper| {
                    let before = paper.notes.trim_end();
                    paper.notes = if before.is_empty() { note.clone() } else { format!("{before}\n\n{note}") };
                    joined = paper.notes.clone();
                });
                if !saved { return }
                if let Some(lib) = self.lib.as_ref().filter(|lib| lib.selected.as_deref() == Some(paper_id.as_str())) { lib.notes.update(cx, |input, cx| input.set_text(&joined, cx)) }
                self.run(Cmd::CloseModal, window, cx);
                self.say("已记到这篇文献的「我的笔记」里。", cx);
            }
            Modal::AddById { input, busy } => {
                if *busy { return }
                let Some(id) = lookup::parse_identifier(&text(input, cx)) else {
                    self.modal_error = "没有认出 DOI 或 PMID。DOI 形如 10.1038/s41586-020-2649-2，PMID 是一串数字。".into();
                    return;
                };
                *busy = true;
                let library = self.lib.as_ref().map(|l| l.info.id.clone()).unwrap_or_default();
                cx.spawn_in(window, async move |this, cx| {
                    let found = cx.background_executor().spawn(async move { wisp_net::lookup_paper(&id) }).await;
                    this.update_in(cx, |this, window, cx| {
                        if !matches!(this.modal, Some(Modal::AddById { .. })) || this.lib.as_ref().is_none_or(|l| l.info.id != library) { return }
                        if let Some(Modal::AddById { busy, .. }) = this.modal.as_mut() { *busy = false }
                        let added = found.and_then(|paper| this.lib.as_mut().unwrap().store.add_paper(paper).map_err(|e| format!("{e:#}")));
                        match added {
                            Ok(paper) => {
                                this.change(cx, |_| Ok(()));
                                this.run(Cmd::CloseModal, window, cx);
                                this.select(Some(paper.id), window, cx);
                                this.say(format!("已添加：{}", paper.title), cx);
                            }
                            Err(err) => this.modal_error = err,
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Modal::Paper(form) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let mut paper = form.id.as_ref().and_then(|id| lib.paper(id)).cloned().unwrap_or_default();
                let year = text(&form.year, cx);
                paper.year = match year.parse::<i32>() {
                    Ok(year) => Some(year),
                    Err(_) if year.is_empty() => None,
                    Err(_) => {
                        self.modal_error = "年份应为数字，例如 2024；不确定可以留空。".into();
                        return;
                    }
                };
                paper.title = text(&form.title, cx);
                paper.authors = text(&form.authors, cx);
                paper.journal = text(&form.journal, cx);
                paper.doi = text(&form.doi, cx);
                paper.pmid = text(&form.pmid, cx);
                paper.url = text(&form.url, cx);
                paper.tags = split_list(&text(&form.tags, cx));
                paper.abstract_text = text(&form.abstract_text, cx);
                paper.fields.clear();
                for (id, input) in &form.fields {
                    let value = text(input, cx);
                    let kind = lib.fields.iter().find(|f| &f.id == id).map(|f| f.kind);
                    let values = if kind == Some(FieldKind::Tags) { split_list(&value) } else { clean_strings([value]) };
                    if !values.is_empty() { paper.fields.insert(id.clone(), values); }
                }
                for (id, value) in &form.selects {
                    paper.fields.insert(id.clone(), vec![value.clone()]);
                }
                let editing = form.id.is_some();
                let result = if editing { lib.store.save_paper(&mut paper).map(|_| paper.id.clone()) } else { lib.store.add_paper(paper).map(|p| p.id) };
                match result {
                    Ok(id) => {
                        self.change(cx, |_| Ok(()));
                        self.run(Cmd::CloseModal, window, cx);
                        if !editing { self.select(Some(id), window, cx) }
                        // An edited title or abstract is a new text to translate.
                        self.translate_selected(false, cx);
                        self.say(if editing { "修改已保存。" } else { "已添加。" }, cx);
                    }
                    Err(err) => self.modal_error = format!("{err:#}"),
                }
            }
            Modal::Fields { editing, name, options, kind, in_table } => {
                let mut field = FieldDef { id: editing.clone().unwrap_or_default(), name: text(name, cx), kind: *kind, options: split_list(&text(options, cx)), in_table: *in_table };
                let Some(lib) = self.lib.as_mut() else { return };
                match lib.store.save_field(&mut field) {
                    Ok(()) => {
                        self.change(cx, |_| Ok(()));
                        self.fields_dialog(None, window, cx);
                    }
                    Err(err) => self.modal_error = format!("{err:#}"),
                }
            }
            Modal::Search { editing, kind, name, query, max } => {
                let Ok(max_results) = text(max, cx).parse::<usize>() else {
                    self.modal_error = "每次取回的篇数应为数字，例如 50。".into();
                    return;
                };
                let Some(lib) = self.lib.as_mut() else { return };
                // An edited search keeps whether it is enabled.
                let enabled = editing.as_ref().and_then(|id| lib.searches.iter().find(|s| &s.id == id)).is_none_or(|s| s.enabled);
                let mut search = SavedSearch { id: editing.clone().unwrap_or_default(), kind: kind.clone(), name: text(name, cx), query: text(query, cx), max_results, enabled, ..Default::default() };
                match lib.store.save_search(&mut search) {
                    Ok(()) => {
                        // What the old query found says nothing about the new one.
                        lib.trial = None;
                        self.change(cx, |_| Ok(()));
                        self.run(Cmd::CloseModal, window, cx);
                    }
                    Err(err) => self.modal_error = format!("{err:#}"),
                }
            }
        }
        cx.notify();
    }
}

/// Run one tool call for the assistant on a connection of its own (the
/// window's stays free while a PDF's text is extracted): the call's id,
/// what the model reads, whether it failed, and the line for the window.
fn run_tool(info: &LibraryInfo, sources: &[ai::Source], call: PreparedCall, allowed: bool) -> (String, String, bool, String) {
    if call.confirm && !allowed {
        return (call.id, "用户没有同意这次操作，它没有执行。不要重试；需要时向用户说明它会做什么，由用户决定。".into(), true, "没有执行：未获同意".into());
    }
    let result = call.args.and_then(|args| {
        let mut store = Store::open(&info.dir).map_err(|e| format!("{e:#}"))?;
        let mut value = tools::call_in(&mut store, info, &call.name, &args, &wisp_net::get_text, &wisp_net::download)?;
        ai::add_labels(&mut value, sources);
        Ok(value)
    });
    let shown = tools::describe_result(&result);
    match result {
        Ok(value) => (call.id, tools::output_text(&value), false, shown),
        Err(err) => (call.id, err, true, shown),
    }
}

/// Debug builds only: `WISP_PAPER_DEV=open:0,select:1,tab:pdf` puts the
/// window into a state at start, for screenshots while developing.
#[cfg(debug_assertions)]
impl Workspace {
    pub fn dev_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(steps) = std::env::var("WISP_PAPER_DEV") else { return };
        for step in steps.split(',') {
            // `8@shot:/tmp/a.png` takes the step eight seconds after the start: what it is
            // to show (an import, an answer) needs that long to come about.
            match step.split_once('@').and_then(|(seconds, later)| Some((seconds.parse::<u64>().ok()?, later.to_string()))) {
                Some((seconds, later)) => {
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_secs(seconds)).await;
                        this.update_in(cx, |this, window, cx| this.dev_step(&later, window, cx)).ok();
                    })
                    .detach();
                }
                None => self.dev_step(step, window, cx),
            }
        }
    }

    fn dev_step(&mut self, step: &str, window: &mut Window, cx: &mut Context<Self>) {
        {
            let (name, arg) = step.split_once(':').unwrap_or((step, ""));
            let cmd = match name {
                "open" => self.libraries.get(arg.parse::<usize>().unwrap_or(0)).map(|l| Cmd::OpenLibrary(l.id.clone())),
                "select" => self.lib.as_ref().and_then(|l| l.shown.get(arg.parse::<usize>().unwrap_or(0)).map(|&i| Cmd::Select(l.papers[i].id.clone()))),
                "tab" => Some(Cmd::SetTab(match arg { "pdf" => Tab::Pdf, "ask" => Tab::Ask, _ => Tab::Info })),
                "page" => Some(Cmd::SetPage(match arg { "ask" => Page::Ask, "trash" => Page::Trash, "sources" => Page::Sources, "screen" => Page::Screen, "write" => Page::Write, _ => Page::Papers })),
                // `write:methods` shows another part of the manuscript; `draft:<text>` leaves text in it, as if typed.
                "write" => Some(Cmd::WriteSection(arg.to_string())),
                // `manuscript:new` makes another manuscript, `manuscript:<title>` gives the current one its title;
                // `section` / `section:<id>` is the dialog for a new section / one's name and kind;
                // `reading` shows the whole manuscript, `versions` the section's history.
                "manuscript" if arg == "new" => Some(Cmd::NewManuscript),
                "manuscript" => {
                    if let Some(lib) = self.lib.as_ref() { lib.writing.title.update(cx, |input, cx| input.set_text(arg, cx)) }
                    self.write_title_typed(arg.to_string(), cx);
                    None
                }
                "section" => Some(Cmd::EditSection((!arg.is_empty()).then(|| arg.to_string()))),
                "reading" => Some(Cmd::WriteReading(true)),
                "versions" => Some(Cmd::WriteHistory),
                // `find` asks for candidates as the shortcut does; `find:<words>` puts words in the find box and presses Enter.
                "find" if arg.is_empty() => Some(Cmd::WriteFind),
                "find" => {
                    if let Some(lib) = self.lib.as_ref() { lib.writing.find.update(cx, |input, cx| input.set_text(arg, cx)) }
                    self.write_find(true, cx);
                    return;
                }
                // `unfold` shows the sentences around the first candidate.
                "unfold" => self.lib.as_ref().and_then(|lib| lib.writing.search.candidates.as_ref()?.as_ref().ok()?["sentences"].get(0).map(|s| Cmd::WriteUnfold(format!("{}-{}", s["paperId"].as_str().unwrap_or(""), s["position"].as_u64().unwrap_or(0))))),
                "only" => Some(Cmd::WriteOnly(arg.strip_prefix('*').unwrap_or(arg).to_string(), arg.starts_with('*'))),
                "draft" => {
                    if let Some(lib) = self.lib.as_ref() { lib.writing.editor.update(cx, |input, cx| input.insert(arg, cx)) }
                    return;
                }
                // `import:<folder>` takes in every PDF under a folder, as if it were dropped on the window:
                // the folders below it become tags.
                "import" => {
                    self.import_paths(vec![PathBuf::from(arg)], cx);
                    return;
                }
                // `shot:<file.png>` writes the window as it is drawn to a picture, without the screen:
                // the frame is rendered off it. macOS only.
                "shot" => {
                    let path = PathBuf::from(arg);
                    window.defer(cx, move |window, cx| {
                        let Some((width, height, mut pixels)) = window.render_offscreen(cx) else { return eprintln!("shot: this platform cannot render off the screen") };
                        for pixel in pixels.as_chunks_mut::<4>().0 {
                            pixel.swap(0, 2);
                        }
                        match image::RgbaImage::from_raw(width as u32, height as u32, pixels).map(|picture| picture.save(&path)) {
                            Some(Ok(())) => eprintln!("shot: {}", path.display()),
                            other => eprintln!("shot: not written ({other:?})"),
                        }
                    });
                    return;
                }
                // The pointer, as the window hears of it: `press:x;y`, `move:x;y` (the button held), `release:x;y`,
                // `click:x;y`, `click2:x;y` (the second click of a double click), and `wheel:x;y;dy;cmd` (dy in pixels;
                // `cmd` or `shift` held, or nothing).
                // A key as the window hears it (`key:down`, `key:cmd-backspace`).
                "key" => {
                    if let Ok(stroke) = Keystroke::parse(arg) { window.defer(cx, move |window, cx| { window.dispatch_keystroke(stroke, cx); }) }
                    return;
                }
                "press" | "move" | "release" | "click" | "click2" | "rclick" | "wheel" => {
                    let mut numbers = arg.split(';');
                    let mut number = || numbers.next().and_then(|n| n.parse::<f32>().ok()).unwrap_or(0.);
                    let position = point(px(number()), px(number()));
                    let turned = number();
                    let held = arg.rsplit(';').next().unwrap_or("");
                    let modifiers = Modifiers { platform: held == "cmd", control: held == "ctrl", shift: held == "shift", ..Default::default() };
                    let count = if name == "click2" { 2 } else { 1 };
                    let events = match name {
                        "press" => vec![PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position, modifiers, click_count: 1, first_mouse: false })],
                        "move" => vec![PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(MouseButton::Left), modifiers })],
                        "release" => vec![PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers, click_count: 1 })],
                        "rclick" => vec![PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Right, position, modifiers, click_count: 1, first_mouse: false }), PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Right, position, modifiers, click_count: 1 })],
                        "wheel" => vec![PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), PlatformInput::ScrollWheel(ScrollWheelEvent { position, delta: ScrollDelta::Pixels(point(px(0.), px(turned))), modifiers, touch_phase: TouchPhase::Moved })],
                        _ => vec![
                            PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }),
                            PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position, modifiers, click_count: count, first_mouse: false }),
                            PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers, click_count: count }),
                        ],
                    };
                    window.defer(cx, move |window, cx| {
                        for event in events {
                            window.dispatch_event(event, cx);
                        }
                    });
                    return;
                }
                // `click:<command>` for what has no step of its own.
                "textless" => Some(Cmd::ShowTextless),
                // `findpdf` asks about the papers of the list on show, `findpdf:one` about the selected paper; `fetched` shows what came of it.
                "findpdf" => Some(Cmd::FindPdfs(if arg == "one" { self.lib.as_ref().and_then(|lib| lib.selected.clone()) } else { None })),
                "fetched" => Some(Cmd::ShowFetch),
                // A network-free preview of a mixed batch in an isolated demo library.
                "fetch-preview" => {
                    let Some(lib) = self.lib.as_mut() else { return };
                    let total = lib.papers.len().min(428);
                    let done = if arg == "complete" { total } else if arg == "empty" { 0 } else { total / 3 };
                    let mut run = FetchRun { total, run: lib.fetching.as_ref().map_or(1, |run| run.run + 1), ..Default::default() };
                    for (i, paper) in lib.papers.iter().take(total).enumerate() {
                        if i < done {
                            let (outcome, detail) = match i % 4 {
                                0 => ("saved", "来自 OpenAlex · 正式版，读出 12 页文字"),
                                1 => ("failed", "出版社返回 HTTP 403，拒绝程序下载。可在浏览器中下载 PDF 后拖入文献库。"),
                                _ => ("not_found", "本次检索未找到可下载的 PDF 地址。"),
                            };
                            run.rows.push(FetchRow { paper_id: paper.id.clone(), title: paper.title.clone(), outcome: outcome.into(), detail: detail.into(), textless: false });
                        } else if run.working.len() < FETCH_WORKERS && arg != "stopped" {
                            run.working.push(paper.id.clone());
                        } else {
                            run.todo.push(paper.id.clone());
                        }
                    }
                    if arg == "stopped" { run.stop() }
                    lib.fetching = Some(run);
                    cx.notify();
                    None
                }
                "view" => Some(Cmd::SetView(match arg { "nopdf" => View::NoPdf, "unread" => View::Unread, _ => View::All })),
                "import-preview" | "zotero-preview" => {
                    let Some(lib) = self.lib.as_mut() else { return };
                    let total = lib.papers.len().min(24);
                    let done = if arg == "complete" { total } else { total / 2 };
                    let mut pdfs = ImportRun { total, ..Default::default() };
                    let mut zotero = ZoteroRun { total, running: done < total, ..Default::default() };
                    for (i, paper) in lib.papers.iter().take(total).enumerate() {
                        let file = format!("{}.pdf", paper.title);
                        if i < done {
                            let failed = i % 5 == 4;
                            let skipped = i % 5 == 3;
                            let incomplete = i % 5 == 2;
                            pdfs.rows.push(ImportRow { name: file, outcome: if failed { Imported::Failed("无法读取这个文件".into()) } else if skipped { Imported::InTrash } else if i % 5 == 1 { Imported::Attached } else { Imported::Added }, paper_id: (!failed).then(|| paper.id.clone()), unrecognised: incomplete, textless: String::new() });
                            zotero.rows.push(zotero::Row { title: paper.title.clone(), paper_id: (!failed && !skipped).then(|| paper.id.clone()), added: i % 5 == 0, in_trash: skipped, files_added: usize::from(!failed && !skipped), problems: if failed { vec!["无法读取文献记录".into()] } else if incomplete { vec!["附件尚未下载到本机".into()] } else { vec![] }, ..Default::default() });
                        } else if pdfs.working.len() < IMPORT_WORKERS {
                            pdfs.working.push(file);
                            zotero.current = paper.title.clone();
                        }
                    }
                    if name == "import-preview" { lib.import = Some(pdfs) } else { lib.zotero = Some(zotero) }
                    cx.notify();
                    None
                }
                "materials" => Some(Cmd::ToggleMaterials),
                // `review:requirements|outline|section|checks` takes the last answer for that step, `review:confirm` confirms what the step waits for,
                // `review:record` shows the record or the conversation, `review:judge` says the first row of evidence is borne out, `review:manuscript` makes the manuscript.
                "review" => self.lib.as_ref().and_then(|lib| {
                    let answer = lib.library_chat.messages.iter().rev().find_map(|item| if let ChatItem::Answer(text) = item { Some(text.clone()) } else { None }).unwrap_or_default();
                    let review = lib.library_chat.review.as_ref();
                    match arg {
                        "requirements" => Some(Cmd::Review(ReviewDo::Requirements, answer)),
                        "outline" => Some(Cmd::Review(ReviewDo::Outline, answer)),
                        "section" => Some(Cmd::Review(ReviewDo::Section, answer)),
                        "checks" => Some(Cmd::Review(ReviewDo::Checks, answer)),
                        "confirm" => Some(Cmd::Review(if review.is_some_and(|review| review.step() == 1) { ReviewDo::ConfirmRequirements } else { ReviewDo::ConfirmOutline }, String::new())),
                        "complete" => Some(Cmd::Review(ReviewDo::Complete, String::new())),
                        "record" => Some(Cmd::ToggleReview),
                        "judge" => review.and_then(|review| review.evidence_rows.first()).map(|row| Cmd::JudgeReviewEvidence(row.id.clone(), wisp_core::review::EvidenceJudgment::Supports)),
                        "evidence" => Some(Cmd::EditReviewEvidence(None, String::new())),
                        "edit" => Some(Cmd::EditReviewText(ReviewText::Requirements)),
                        "manuscript" => Some(Cmd::ReviewToManuscript),
                        _ => Some(Cmd::StartReview),
                    }
                }),
                "history-paper" => Some(Cmd::ToggleHistory(true)),
                "newchat" => Some(Cmd::NewChat(arg == "paper")),
                "note" => {
                    let Some(answer) = self.lib.as_ref().and_then(|lib| lib.paper_chat.messages.iter().rev().find_map(|item| if let ChatItem::Answer(text) = item { Some(text.clone()) } else { None })) else { return };
                    Some(Cmd::AnswerToNote("（脚本）".into(), answer))
                }
                "submit" => Some(Cmd::SubmitModal),
                "reread" | "byeye" => {
                    let Some(file) = self.lib.as_ref().and_then(|lib| lib.selected.as_deref().and_then(|id| lib.pdf_file(id)).map(|file| file.id.clone())) else { return };
                    Some(if name == "reread" { Cmd::RereadFile(file) } else { Cmd::ReadPages(file) })
                }
                "width" => {
                    self.settings.detail_width = arg.parse().unwrap_or(0.);
                    cx.notify();
                    return;
                }
                // `readmode` gives the selected paper the whole window, or brings the library back; `chatwidth:400` is the conversation's width beside the PDF.
                "readmode" => Some(Cmd::ToggleReading),
                "chatwidth" => {
                    self.settings.chat_width = arg.parse().unwrap_or(0.);
                    cx.notify();
                    return;
                }
                // `sidebar` puts the sidebar away or brings it back; `sidebar:300` gives it a width.
                "sidebar" if arg.is_empty() => Some(Cmd::ToggleSidebar),
                "sidebar" => {
                    self.settings.sidebar_width = arg.parse().unwrap_or(0.);
                    cx.notify();
                    return;
                }
                // `report` shows what came of the last import.
                "report" => Some(Cmd::ShowImport),
                "zotero" => {
                    self.import_zotero(PathBuf::from(arg), cx);
                    None
                }
                "zotero-report" => Some(Cmd::ShowZotero),
                // `screen` starts the assistant on the pending papers, `screen:decided` on the decided ones.
                "screen" => Some(if arg == "decided" { Cmd::ScreenRecheck } else { Cmd::ScreenStart }),
                // `goal:<text>` writes the screening goal, as if typed.
                "goal" => {
                    if let Some(lib) = self.lib.as_mut() {
                        lib.screen.criteria.update(cx, |input, cx| input.set_text(arg, cx));
                        lib.info.criteria = arg.trim().to_string();
                        libraries::save(&lib.info).ok();
                    }
                    None
                }
                // `sort` has the model sort the full texts into parts, `read` has it read the pages of those without text.
                "sort" => Some(Cmd::WriteRun(false)),
                // `assist:rewrite|translate|polish|<own command>` asks the model about the caret's paragraph;
                // `assisted:replace|below|copy|drop` uses the result, `assiststop` stops it; `caret:<byte>` / `pick:<from>;<to>` place the caret / select.
                "assist" => Some(Cmd::WriteAssist(match arg { "rewrite" => ai::Assist::Rewrite, "translate" => ai::Assist::Translate, "polish" => ai::Assist::Polish, own => ai::Assist::Own(own.to_string()) })),
                "assiststop" => Some(Cmd::WriteAssistStop),
                "assisted" => Some(Cmd::WriteAssistUse(match arg { "replace" => crate::write_assist::Use::Replace, "below" => crate::write_assist::Use::Below, "copy" => crate::write_assist::Use::Copy, _ => crate::write_assist::Use::Drop })),
                "undo" => {
                    if let Some(lib) = self.lib.as_ref() { lib.writing.editor.update(cx, |input, cx| input.undo_last(cx)) }
                    return;
                }
                "caret" | "pick" => {
                    let (from, to) = arg.split_once(';').unwrap_or((arg, arg));
                    let (from, to) = (from.parse().unwrap_or(0), to.parse().unwrap_or(0));
                    if let Some(lib) = self.lib.as_ref() { lib.writing.editor.update(cx, |input, cx| input.select_range(from..to, cx)) }
                    return;
                }
                "read" => Some(Cmd::WriteRun(true)),
                // `guide:new` is what a new library shows; `guide:search` goes straight to composing, `guide:go` also generates.
                "guide" => {
                    self.run(Cmd::OpenGuide(arg == "new"), window, cx);
                    (arg == "go").then_some(Cmd::GuideGenerate)
                }
                // `quote:3|a sentence` opens the selected paper's PDF at page 3 with that sentence highlighted.
                "quote" => self.lib.as_ref().and_then(|l| l.selected.clone()).zip(arg.split_once('|')).map(|(id, (page, quote))| Cmd::GotoSource(id, page.parse().ok(), quote.to_string())),
                // `pdfpick:<page>;<from>;<to>` selects glyphs of a 1-based page of the PDF on show (after a `shot:` has drawn it);
                // `pdftranslate` translates the selection, `pdftranslate:<n>` with the n-th model for this once.
                "pdfpick" => {
                    let numbers: Vec<usize> = arg.split(';').filter_map(|n| n.parse().ok()).collect();
                    if let (Some(pdf), [page, from, to]) = (self.lib.as_ref().and_then(|l| l.pdf.clone()), numbers.as_slice()) && !pdf.update(cx, |pdf, cx| pdf.select_marks(page.saturating_sub(1), *from, *to, cx)) {
                        eprintln!("pdfpick: the glyphs of page {page} are not at hand yet");
                    }
                    return;
                }
                "pdftranslate" => {
                    let profile = arg.parse::<usize>().ok().and_then(|n| self.settings.profiles.get(n)).map(|p| p.id.clone());
                    if let Some(pdf) = self.lib.as_ref().and_then(|l| l.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.translate(profile, cx)) }
                    return;
                }
                // `try:0` runs the first saved search as a trial.
                "try" => self.lib.as_ref().and_then(|l| l.searches.get(arg.parse::<usize>().unwrap_or(0)).map(|s| Cmd::TrySearch(s.id.clone()))),
                "wide" => Some(Cmd::ToggleWide),
                "cut" => Some(Cmd::PdfCut),
                "beside" => Some(Cmd::BesidePdf(arg != "off")),
                // `float` / `float:off`: the paper's conversation over its PDF, or beside it; `fold` / `fold:off` folds it up or opens it.
                "float" => Some(Cmd::FloatChat(arg != "off")),
                "fold" => Some(Cmd::FoldChat(arg != "off")),
                // `zoom:150`, `zoom:page`, `zoom:width`, `zoom:in`, `zoom:out`
                "zoom" => Some(Cmd::PdfZoom(match arg {
                    "in" => ZoomTo::In,
                    "out" => ZoomTo::Out,
                    "page" => ZoomTo::Set(Zoom::FitPage),
                    "width" => ZoomTo::Set(Zoom::FitWidth),
                    percent => ZoomTo::Set(Zoom::Percent(percent.parse().unwrap_or(100.))),
                })),
                "translate" if matches!(arg, "titles" | "abstracts") => {
                    let Some(lib) = self.lib.as_ref() else { return };
                    let ids = lib.shown.iter().map(|&i| lib.papers[i].id.clone()).filter(|id| lib.marked.contains(id)).collect();
                    Some(Cmd::TranslatePapers(ids, if arg == "titles" { wisp_core::translation::Field::Title } else { wisp_core::translation::Field::Abstract }))
                }
                "translate" if arg == "stop" => Some(Cmd::StopBatchTranslation),
                "translate" => Some(Cmd::ToggleTranslation),
                // `profile:1` shows the second model service on the AI page.
                "profile" => Some(Cmd::SelectProfile(arg.parse().unwrap_or(0))),
                "menu" => {
                    let kind = match arg { "sort" => MenuKind::Sort, "year" => MenuKind::Filter(FilterKey::Year), "zoom" => MenuKind::PdfZoom, "fulltext" => MenuKind::Filter(FilterKey::Fulltext), "manuscripts" => MenuKind::Manuscripts, "export" => MenuKind::ManuscriptExport, _ => MenuKind::Add };
                    self.open_menu(kind, point(px(if matches!(arg, "zoom" | "manuscripts" | "export") { 900. } else { 20. }), px(120.)), cx);
                    return;
                }
                "search" => {
                    if let Some(lib) = self.lib.as_mut() {
                        lib.search.update(cx, |input, cx| input.set_text(arg, cx));
                        lib.filters.search = arg.to_string();
                        lib.refilter();
                    }
                    None
                }
                // Ask the library a question (no commas in it: they separate the steps).
                "ask" => {
                    if let Some(lib) = self.lib.as_ref() { lib.library_chat.input.update(cx, |input, cx| input.set_text(arg, cx)) }
                    Some(Cmd::Ask(false))
                }
                // The same of the selected paper.
                "askpaper" => {
                    if let Some(lib) = self.lib.as_ref() { lib.paper_chat.input.update(cx, |input, cx| input.set_text(arg, cx)) }
                    Some(Cmd::Ask(true))
                }
                // `type:/se` leaves text in the library's question box, as if typed (the slash commands show).
                "type" => {
                    if let Some(lib) = self.lib.as_ref() {
                        lib.library_chat.input.update(cx, |input, cx| input.set_text(arg, cx));
                        lib.library_chat.input.read(cx).focus(window);
                    }
                    self.chat_input(false, InputEvent::Changed, cx);
                    None
                }
                // `image:/path/to.png` puts a picture with the next question to the library.
                "image" => {
                    self.attach_images(false, vec![Picture::File(arg.into())], cx);
                    None
                }
                "history" => Some(Cmd::ToggleHistory(false)),
                "settings" => Some(Cmd::OpenSettings(match arg {
                    "appearance" => SettingsPage::Appearance,
                    "shortcuts" => SettingsPage::Shortcuts,
                    "ai" => SettingsPage::Ai,
                    "network" => SettingsPage::Network,
                    "about" => SettingsPage::About,
                    "commands" => SettingsPage::Commands,
                    _ => SettingsPage::General,
                })),
                "candidate" => Some(Cmd::CompleteChoose(arg.parse().ok())),
                // `command:0` opens the first of the person's own commands on 设置 › 命令.
                "command" => Some(Cmd::SelectCommand(arg.parse().unwrap_or(0))),
                // Saved like the choice in 设置 › 外观: "light", "dark", or empty for the system's.
                "theme" => Some(Cmd::SetAppearance(arg.to_string())),
                "record" => Some(Cmd::RecordShortcut(arg.to_string())),
                "update" => Some(Cmd::CheckUpdate),
                "modal" => match arg {
                    "fields" => Some(Cmd::ManageFields),
                    "library" => Some(Cmd::NewLibrary),
                    "add" => Some(Cmd::AddById),
                    "search" => Some(Cmd::NewSearch),
                    "paper" => self.lib.as_ref().and_then(|l| l.selected.clone()).map(Cmd::EditPaper),
                    // 按题目补全 for the selected paper; `submit` then looks, `candidate:0` opens the first record found.
                    "complete" => self.lib.as_ref().and_then(|l| l.selected.clone()).map(Cmd::CompletePaper),
                    _ => None,
                },
                _ => None,
            };
            if let Some(cmd) = cmd { self.run(cmd, window, cx) }
        }
    }
}

#[cfg(test)]
mod fetch_tests {
    use super::{FetchRow, FetchRun};

    #[test]
    fn stopping_keeps_the_total_and_accepts_inflight_results() {
        let mut run = FetchRun { total: 4, todo: vec!["queued-a".into(), "queued-b".into()], working: vec!["first".into(), "second".into()], run: 7, ..Default::default() };
        run.stop();
        assert_eq!((run.total, run.run, run.remaining(), run.status()), (4, 7, 2, "正在停止"));
        assert!(run.todo.is_empty());
        assert_eq!(run.working.len(), 2);
        // An already-started request may save a PDF after Stop. Keep its result visible.
        for outcome in ["saved", "failed"] {
            let id = run.working.pop().unwrap();
            run.rows.push(FetchRow { paper_id: id, title: "Paper".into(), outcome: outcome.into(), detail: String::new(), textless: false });
        }
        assert!(!run.running());
        assert_eq!((run.status(), run.total, run.remaining()), ("已停止", 4, 2));
        assert_eq!((run.count("saved"), run.count("failed")), (1, 1));
    }
}
