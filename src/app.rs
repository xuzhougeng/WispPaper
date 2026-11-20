//! `Workspace`, the window's root view: the home page of libraries, or one
//! open library. All state lives here; every click becomes a `Cmd` handled
//! by `run`, and the other modules render parts of it.

use crate::i18n::t;
use crate::input::{InputEvent, TextInput};
use crate::pdf::{PdfEvent, PdfView, Split, Zoom, ZoomTo};
use crate::settings::{AiTab, SettingsPage, SettingsView, UpdateState};
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
use wisp_core::screening::{Before, Confirming, Round};
use wisp_core::settings::{Language, Session, Settings, Task, session_file, settings_file};
use wisp_core::store::{Conversation, ConversationInfo, ImportBatch, SavedSearch, Store, StructureInfo, Suggestion, TrashEntry};
use wisp_core::manuscript::{Manuscript, Snapshot};
use wisp_core::{archive, complete, export, fulltext, import, pdftext, structure, time, tools, zotero};
pub use crate::manuscripts::ManuscriptExport;

/// What the middle of an open library shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Papers,
    /// 助手: the conversation with the library, over the current list.
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
    /// 图片库: the figures cut out of the library's PDFs, the other half of 收藏 (`src/figures.rs`).
    Figures,
    /// 提取: what the assistant proposes for the custom fields, looked over and written together (`src/extract.rs`).
    Extract,
}

impl Page {
    /// The word a page goes by where it is written down: `session.json`, a `page:` step of a scripted start.
    pub fn key(self) -> &'static str {
        match self {
            Page::Papers => "papers",
            Page::Ask => "ask",
            Page::Trash => "trash",
            Page::Screen => "screen",
            Page::Guide => "guide",
            Page::Sources => "sources",
            Page::Write => "write",
            Page::Figures => "figures",
            Page::Extract => "extract",
        }
    }

    /// The page of that word; the papers for any other.
    pub fn from_key(key: &str) -> Page {
        [Page::Ask, Page::Trash, Page::Screen, Page::Guide, Page::Sources, Page::Write, Page::Figures, Page::Extract].into_iter().find(|page| page.key() == key).unwrap_or(Page::Papers)
    }
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
    /// Between the conversation that writes and its manuscript (`write_agent.rs`).
    Manuscript,
    /// Between two columns of the paper table (`Workspace::column_drag` says which).
    Column,
}

/// The edge between two columns of the paper table, as it was when the pointer took it.
pub struct ColumnDrag {
    /// Where the pointer was.
    pub x: f32,
    /// The column at the edge's left with its width, unless that is the title, which has what the others leave.
    pub left: Option<(String, f32)>,
    /// The column at its right.
    pub right: (String, f32),
    /// How much the title can still give up.
    pub spare: f32,
}

/// Which of a library's conversations: the one with the library (助手), the one about the selected paper,
/// or the one that writes the manuscript on the writing page (助手写, `write_agent.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Talk {
    Library,
    Paper,
    Write,
}

impl Talk {
    /// A word for the ids of a conversation's elements: more than one conversation can be on screen.
    pub fn side(self) -> &'static str {
        match self {
            Talk::Library => "library",
            Talk::Paper => "paper",
            Talk::Write => "write",
        }
    }
}

/// The conversation `$talk` names, borrowed alone: the rest of the library can be read meanwhile.
macro_rules! chat_of {
    ($lib:expr, $talk:expr) => {
        match $talk {
            Talk::Library => &mut $lib.library_chat,
            Talk::Paper => &mut $lib.paper_chat,
            Talk::Write => &mut $lib.write_chat,
        }
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Info,
    Pdf,
    Ask,
}

impl Tab {
    /// As `Page::key`.
    pub fn key(self) -> &'static str {
        match self {
            Tab::Info => "info",
            Tab::Pdf => "pdf",
            Tab::Ask => "ask",
        }
    }

    pub fn from_key(key: &str) -> Tab {
        [Tab::Pdf, Tab::Ask].into_iter().find(|tab| tab.key() == key).unwrap_or(Tab::Info)
    }
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
    /// The columns of the paper table: which are on show (a right click on their names).
    Columns,
    Export,
    Reading(String),
    Screening(String),
    LibraryCard(String),
    /// The sections of the manuscript an answer can be added to: the text, or `None` for what is selected in the library's conversation.
    InsertInto(Option<String>),
    /// Which model a task uses (设置 › AI).
    TaskModel(Task),
    /// 提取: the model to read what the last run left with.
    ExtractRetry,
    /// The services a provider can be added from (设置 › AI › 模型).
    AddProvider,
    /// How hard the n-th model of the provider on show thinks.
    ModelEffort(usize),
    /// How large the PDF's pages are drawn.
    PdfZoom,
    /// Whether the PDF is shown twice, side by side or one over the other.
    PdfSplit,
    /// Which of the paper's PDFs is read: its main text or one of its attachments.
    PdfFiles,
    /// 写作: the library's manuscripts, one section's own, export, and the language.
    Manuscripts,
    Section(String),
    ManuscriptExport,
    WriteLanguage,
    /// 写作: which papers the candidates come from.
    WriteNarrow,
    /// The menus a right click opens (`src/context.rs`): on a paper of a list, a paper in the trash, a file
    /// of the selected paper, a tag of the sidebar, a past conversation (of the paper or the library), a
    /// text that can be selected (the whole of it), an answer (of which conversation, and its text).
    Paper(String),
    /// Several papers marked in the list.
    Papers(Vec<String>),
    Trashed(String),
    File(String),
    Tag(String),
    Conversation(Talk, String),
    Text(String),
    Answer(Talk, String),
    /// A figure of the figure library.
    Figure(String),
}

/// What a person does to a review's record from the conversation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReviewDo {
    Requirements,
    ConfirmRequirements,
    /// The framework drawn from the reviews already published, which the outline is built against.
    Framework,
    ConfirmFramework,
    /// On to the outline without one.
    SkipFramework,
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
    Framework,
    Outline,
    /// The free notes on the evidence.
    Notes,
    Checks,
    /// A section, by its name.
    Section(String),
}

/// The scope of the library's next conversation, as the panel over it changes it: a paper kept to
/// (dragged into the conversation, or its plus clicked), let go again, all let go, the panel's search emptied.
#[derive(Clone, Debug, PartialEq)]
pub enum ScopeDo {
    Pin(String),
    Unpin(String),
    UnpinAll,
    ClearFind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    // Home
    OpenLibrary(String),
    NewLibrary,
    EditLibrary(String),
    AskRemoveLibrary(String),
    RemoveLibrary(String),
    /// The whole library as a `.wisppaper` file, with its PDFs and other attachments or without.
    ExportLibrary(String, bool),
    /// A `.wisppaper` file from another Wisp Paper, taken in as a new library.
    ImportLibrary,
    GoHome,
    RevealFolder,
    OpenUrl(String),
    NoteOpen(Option<Talk>, bool),
    NotePick(usize),
    NoteDestination(bool),
    NotePreview,
    NoteSave(bool),
    NoteResolve(bool),
    // Settings
    OpenSettings(SettingsPage),
    CloseSettings,
    PickDataDir,
    ResetDataDir,
    RevealDataDir,
    ToggleAutoUpdate,
    ToggleAutoDownload,
    ToggleRestoreSession,
    ToggleKeepRunning,
    CheckUpdate,
    DownloadUpdate,
    InstallUpdate,
    SetAppearance(String),
    SetLanguage(Language),
    SetDensity(String),
    /// The Latin or the Chinese interface font; empty is the platform's own.
    SetFontFamily(crate::settings::FontSlot, String),
    SetFontSize(f32),
    ResetTypography,
    SetLineSpacing(wisp_core::settings::Spacing),
    SetParagraphSpacing(wisp_core::settings::Spacing),
    SetTextWidth(wisp_core::settings::TextWidth),
    ResetReadingAppearance,
    ToggleHighContrast,
    ResetAppearance,
    RecordShortcut(String),
    ClearShortcut(String),
    ResetShortcut(String),
    ResetShortcuts,
    /// 设置 › AI on one of its tabs.
    AiTab(AiTab),
    PickProtocol(Protocol),
    /// Show another provider's settings on 设置 › AI › 模型.
    SelectProvider(usize),
    /// One more provider, from a `provider::Preset` key; empty is a service set up by hand.
    AddProvider(String),
    AskRemoveProvider(usize),
    RemoveProvider(usize),
    /// One more model of the provider on show, from what its field holds.
    AddModel,
    RemoveModel(usize),
    /// How hard the n-th model of the provider on show thinks, a key of `ai::EFFORTS`.
    SetEffort(usize, String),
    /// What kind the n-th model of the provider on show is: one that answers in words (empty), draws, or judges.
    SetModelKind(usize, String),
    /// How the n-th model of the provider on show is asked for a picture, a key of `imagegen::INTERFACES`.
    SetModelInterface(usize, String),
    /// Bind a task to a profile id; an empty id follows the default.
    SetTaskModel(Task, String),
    SetScreenWorkers(usize),
    SetTranslateWorkers(usize),
    ResetTranslationPrompt,
    /// One more slash command of the person's own, on 设置 › 命令.
    AddCommand,
    /// Show or fold up the fields of the n-th.
    SelectCommand(usize),
    AskRemoveCommand(usize),
    /// 设置 › 自定义: the connectors and the skills (`custom.rs`).
    Custom(crate::custom::Do),
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
    /// Every paper in the trash back in the library.
    RestoreAll,
    /// 清空回收站: the dialog that asks for the phrase, and the deletion it leads to.
    AskEmptyTrash,
    EmptyTrash,
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
    /// Show the PDF on show twice, each half scrolled on its own, or once again.
    PdfSplit(Split),
    /// Cut a piece out of a page as a picture for a question, or stop doing so.
    PdfCut,
    /// Draw boxes or place sticky notes on the pages, or go back to selecting text.
    PdfTool(crate::pdf::Tool),
    /// Show this file of the selected paper in the viewer (its main text or an attachment).
    ReadFile(String),
    /// Add files to a paper as its main text (`fulltext`) or as attachments (`supplement`).
    AttachAs(String, &'static str),
    /// Show the paper's PDF to the right of its conversation, or put it away.
    BesidePdf(bool),
    /// The paper's PDF and its conversation change places (`Settings::pdf_first`).
    SwapPanes,
    /// The PDF's tools float at the page's right edge, or stand in its toolbar (`Settings::pdf_tools_floating`).
    FloatPdfTools(bool),
    /// Out of the layout for asking about a paper: the tab it was come to from, with the library around it.
    LeaveAsk,
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
    /// Show or put away a column of the paper table that is not a custom field's.
    ToggleColumn(String),
    /// The columns of the paper table get back the widths they come with.
    ResetColumnWidths,
    AddPreset,
    // Questions
    Ask(Talk),
    /// Ask one of the suggested questions.
    Suggest(Talk, String),
    /// Leave a slash command in the assistant's box, for its words to follow.
    StartCommand(&'static str),
    NewChat(Talk),
    /// Stop the answer being given.
    StopChat(Talk),
    /// Show the past conversations, or go back to the one in hand.
    ToggleHistory(Talk),
    /// Take up a past conversation again.
    OpenChat(Talk, String),
    AskRemoveChat(Talk, String),
    RemoveChat(Talk, String),
    /// Take the n-th of the slash commands on offer.
    PickSlash(Talk, usize),
    /// Choose pictures to go with the next question.
    AttachImages(Talk),
    RemoveAttachment(Talk, usize),
    /// Take a passage quoted for the next question away again.
    RemoveQuote(Talk, usize),
    /// The person's answer to the assistant wanting to change the library.
    AnswerTools(Talk, Consent),
    /// Unfold or fold the tool calls that start at this place in the conversation.
    ToggleTools(Talk, usize),
    CopyText(String),
    /// 撤回: the question at this place in the conversation and all after it are taken back; its words and pictures return to the box.
    Retract(Talk, usize),
    /// 重新回答: the answer that ends at this place is asked for again, from the question before it.
    Regenerate(Talk, usize),
    /// 从这里分支: a new conversation of what was said up to the end of the answer at this place; the one it came from stays in the history.
    Fork(Talk, usize),
    /// Put back what the last 撤回 took away.
    Unretract(Talk),
    /// Save an answer and its source labels to a new manuscript, at the person's request.
    AnswerToManuscript(String, String),
    /// Add this text (an answer, or what is selected in one) at the end of a section of the manuscript.
    InsertInto(String, String),
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
    PickText(Talk, usize, crate::markdown::Pointer, crate::markdown::Place),
    /// Copy what is selected in an answer; or put it into the question box as a quotation.
    CopyPicked(Talk),
    QuotePicked(Talk),
    /// Make a note of the selected paper out of an answer about it: the question, and the answer.
    AnswerToNote(String, String),
    /// Show or put away the panel of the papers the library's questions are put to.
    ToggleMaterials,
    /// What that panel does to the scope of the next conversation (`ScopeDo`).
    Scope(ScopeDo),
    /// Show every source of an answer (by its place in the conversation), or the first few again.
    ToggleSources(Talk, usize),
    /// Open a paper; with a page, its PDF there; with a quoted sentence too, that sentence highlighted.
    GotoSource(String, Option<usize>, String),
    /// The same in one file of the paper: an attachment by its id, or (`None`) the main text.
    GotoFile(String, Option<String>, Option<usize>, String),
    /// The same for a citation as an answer writes it: the label of a conversation, an attachment's number, a page, a quotation.
    GotoCited(Talk, String, Option<usize>, Option<usize>, String),
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
    /// The assistant asked again about the papers already decided, against the goal as it is now;
    /// with `true`, only about the included ones (继续筛: the next, tighter round of the funnel).
    ScreenRecheck(bool),
    /// Move the decided papers that are not included to the trash, after asking.
    AskTrashUnincluded,
    ScreenStop,
    /// The person's decision on a paper, made on the screening page.
    ScreenDecide(String, String),
    /// Show only the queue's papers with this suggestion (a key of `SCREENINGS`), those without one (`none`), or all (empty).
    ScreenFilter(String),
    /// How the queue is worked through: with the assistant's run over it (批量), one paper at a time (逐篇), or its suggestions confirmed together (确认).
    ScreenMode(ScreenMode),
    /// 逐篇: show the paper at this place of the round; one past the last is the round's summary.
    ScreenGo(usize),
    /// 逐篇: take the round's last decision back.
    ScreenUndo,
    /// Ask the assistant about this one paper.
    ScreenAskOne(String),
    /// The paper being screened beside the page: its PDF when it has one, else its record.
    ScreenBeside,
    /// 确认: count a paper in or leave it out; give it another status; the suggested verdict on show; all of
    /// those on show in or out; the least sure first; the paper the keys act on.
    ConfirmTick(String),
    ConfirmChoose(String, String),
    ConfirmPart(String),
    ConfirmTickAll(bool),
    ConfirmSort,
    ConfirmCursor(String),
    /// Write what is ticked; take that back.
    ConfirmApply,
    ConfirmUndo,
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
    /// Cut the section being written at the caret's paragraph: that paragraph its heading (true), or the first of a new section.
    SplitSection(bool),
    /// Put a section back into the one before it.
    MergeSection(String),
    /// The whole manuscript to read (true), or one section to write.
    WriteReading(bool),
    /// The top-left arrow of a manuscript: out of 写作, back to the page it was opened from.
    LeaveWrite,
    ExportManuscript(ManuscriptExport),
    /// How a manuscript exported to Word cites (`cite::Style::key`).
    CiteStyle(String),
    /// 写作: a `.csl` file to cite by in Word files.
    AddCiteStyle,
    /// The section's earlier versions.
    WriteHistory,
    HistoryPick(usize),
    /// Cite a paper (by id) at the caret: its key, `[@Zhang2024]` (`src/cite.rs`).
    WriteCite(String),
    /// 引用 on a sentence of a paper: cite the paper and keep the sentence as what the citation rests on.
    WriteCiteFrom(String, wisp_core::passage::Candidate),
    /// The passages citations rest on (write_source.rs).
    Source(crate::write_source::Do),
    /// The reference list of the whole manuscript (true) or of the part being written.
    WriteCiteScope(bool),
    /// Put citations in the part being written right: (start, end, what is there, what replaces it), last first.
    WriteCiteFix(Vec<(usize, usize, String, String)>),
    WriteCopyReferences,
    /// The page of the panel beside the manuscript: the candidates, the comments, or the reference list.
    WriteSide(Side),
    /// The reference list of the whole manuscript: beside it, or, while it is read through, at its end.
    WriteReferences,
    /// The cited papers as BibTeX.
    WriteExportCited,
    /// Have the model sort the full texts into parts (false), or read the pages of the files that gave no text (true).
    WriteRun(bool),
    WriteStop,
    /// Look for candidates: the selected text, or the paragraph at the caret.
    WriteFind,
    /// Show (true) or hide the card under the citation the pointer is on (`Settings::citation_cards_hidden`).
    CitationCards(bool),
    /// Only the papers with this tag (empty: any), and only the starred ones.
    WriteOnly(String, bool),
    /// Show or hide the sentences around a candidate, by its card's id.
    WriteUnfold(String),
    /// 批注, and manuscripts that start from a file (`comments.rs`).
    Comment(crate::comments::Do),
    /// Have the model work on the selected text or the caret's paragraph; stop it; use what it gave.
    WriteAssist(ai::Assist),
    WriteAssistStop,
    WriteAssistUse(crate::write_assist::Use),
    /// 配引用's card: tick a paper, show more, write the ticked citations (write_cite.rs).
    WriteCited(crate::write_cite::Do),
    /// 改稿: the box under the manuscript where the assistant is told what to change (`revise.rs`).
    Revise(crate::revise::Do),
    /// 写作's other mode, where the assistant writes (`write_agent.rs`).
    Agent(crate::write_agent::Do),
    /// Looking for words in the manuscript (`write_seek.rs`).
    Seek(crate::write_seek::Do),
    // The figure library (`src/figures.rs`)
    /// Only the figures with this tag; the tag chosen already, or none, shows all of them.
    FigureTag(String),
    /// Look at a figure and change what is written about it: the dialog.
    OpenFigure(String),
    /// Open the PDF a figure was cut from, at its place.
    FigureSource(String),
    /// Ask about a figure's paper with the figure as a picture.
    AskFigure(String),
    CopyFigure(String),
    /// Save a figure's picture somewhere else.
    SaveFigure(String),
    AskRemoveFigure(String),
    RemoveFigure(String),
    /// Add one of the tags on offer to the dialog's tags.
    FigureAddTag(String),
    /// Have a model that sees pictures write a field of the dialog: the legend, tags, or a reading for the notes.
    FigureInfer(wisp_core::figure::Ask),
    /// 生图: the dialog a picture is drawn in from a description, to keep as a figure.
    DrawFigure,
    /// Have the model that draws make the dialog's picture from its description; while it draws, stop it.
    FigureDraw,
    /// The shape of the picture the dialog asks for.
    FigureShape(wisp_core::imagegen::Shape),
    /// 提取: everything the page, its dialog and the sparkles by a paper's fields do (`src/extract.rs`).
    Extract(crate::extract::Do),
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
    /// A filter's choices: the card of all the filters stays open under it (`render_papers`).
    pub filter: bool,
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
    pub filter: String,
    pub error: String,
    /// Bumped when a run starts or stops, so answers to an earlier one are dropped.
    run: u64,
    /// How the queue is worked through.
    pub mode: ScreenMode,
    /// 逐篇: the papers of this round and the one on show (`screen_single.rs`).
    pub round: Round,
    /// 确认: what the person changed in the list of suggestions (`screen_confirm.rs`).
    pub confirm: Confirming,
    /// What the last confirmation wrote, until the page is left: shown in place of the list, and how it is taken back.
    pub confirmed: Option<Confirmed>,
    pub rail_scroll: UniformListScrollHandle,
    pub confirm_scroll: UniformListScrollHandle,
    pub paper_scroll: ScrollHandle,
}

/// The three faces of 筛选.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenMode {
    /// The queue as a list, with the assistant's run over it.
    #[default]
    Batch,
    /// One paper at a time, decided with a key.
    Single,
    /// The assistant's suggestions looked over and written together.
    Confirm,
}

/// What a confirmation wrote.
pub struct Confirmed {
    /// The papers as they were, to take it back.
    pub before: Vec<Before>,
    /// How many papers got each status, in the order of `SCREENINGS`.
    pub counts: [usize; 5],
}

impl Screen {
    fn new(criteria: &str, cx: &mut App) -> Screen {
        let criteria = cx.new(|cx| TextInput::new("例如：只收以人类样本做单细胞测序的原始研究；综述、方法学文章不收。留空则按文献库的主题判断。", cx).multiline(3).with_text(criteria));
        Screen { criteria, todo: vec![], reading: vec![], total: 0, done: 0, failed: 0, streak: 0, model: String::new(), filter: String::new(), error: String::new(), run: 0, mode: ScreenMode::Batch, round: Round::default(), confirm: Confirming::default(), confirmed: None, rail_scroll: UniformListScrollHandle::new(), confirm_scroll: UniformListScrollHandle::new(), paper_scroll: ScrollHandle::new() }
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
    /// The assistant's changes to the section, asked for in the box under the manuscript (`revise.rs`).
    pub revise: crate::revise::Revising,
    /// The comments on the manuscript, and the assistant's review (`comments.rs`).
    pub comments: crate::comments::Commenting,
    /// Where the manuscript's editor is scrolled to.
    pub editor_scroll: ScrollHandle,
    /// Which page the panel beside the manuscript shows.
    pub side: Side,
    /// Whether that panel is open where it stands under the manuscript, in a page too narrow for both side by side: folded to its tabs until asked for.
    pub side_open: bool,
    /// The other mode, where the assistant writes: which manuscript its conversation is about, what it changed, the versions to step through (`write_agent.rs`).
    pub agent: crate::write_agent::Agent,
    /// Looking for words in the manuscript (`write_seek.rs`).
    pub seek: crate::write_seek::Seek,
}

/// The pages of the panel beside the manuscript: what there is to draw on, what was said about the text, what it cites.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Candidates,
    Comments,
    References,
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
    /// What it would change, a line per paper (`tools::preview_call`), for the card that asks.
    pub details: Vec<String>,
    /// Not a tool of the library: a connector's, or a skill being read.
    pub outside: Option<crate::custom::Outside>,
}

/// The calls of a turn that wait for the person's consent, and where their lines stand in the conversation.
pub struct PendingTools {
    pub calls: Vec<PreparedCall>,
    first: usize,
}

/// The PDFs of an import read at the same time.
const IMPORT_WORKERS: usize = 3;
/// Full texts read again at the same time (`extract_missing`), at most one per core.
const READ_WORKERS: usize = 4;

/// What became of one PDF of an import.
#[derive(Clone, Debug, PartialEq)]
pub enum Imported {
    Added,
    /// The library already held its paper: the file was attached to that.
    Attached,
    /// Its paper is in the trash: nothing was imported.
    InTrash,
    /// An identifier of a paper the library already holds: nothing to do.
    Held,
    Failed(String),
}

pub struct ImportRow {
    /// The file's name, or the identifier looked up.
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

/// The PDFs dropped or picked in one go, or the identifiers pasted into 通过 DOI / PMID 添加 (more of either
/// meanwhile join it): how far the reading is, then what came of each, until it is put away.
#[derive(Default)]
pub struct ImportRun {
    pub total: usize,
    /// How many of `total` are identifiers looked up rather than files read.
    pub ids: usize,
    /// Names of the files currently being read; duplicate names are separate jobs.
    pub working: Vec<String>,
    pub rows: Vec<ImportRow>,
    /// Words pasted with the identifiers that are neither a DOI nor a PMID: left out, said once in the report.
    pub unknown: Vec<String>,
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
        if self.running { return tf!("正在导入 Zotero · {}/{} · {}", "Importing Zotero · {}/{} · {}", self.rows.len(), self.total, self.current) }
        tf!("新增 {} 篇 · 已有 {} 篇 · 回收站 {} 篇 · 失败 {} 篇 · 新增 PDF {} 个 · 待处理 {} 项", "{} added · {} existing · {} in trash · {} failed · {} PDFs added · {} pending",
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

    /// The job's name, as what it imports: PDFs, identifiers, or both.
    pub fn title(&self) -> &'static str {
        match (self.ids, self.running()) {
            (0, true) => t("导入 PDF"),
            (0, false) => t("PDF 导入完成"),
            (ids, true) if ids == self.total => t("按 DOI / PMID 导入"),
            (ids, false) if ids == self.total => t("DOI / PMID 导入完成"),
            (_, true) => t("正在导入"),
            (_, false) => t("导入完成"),
        }
    }

    /// How many files came to this.
    pub fn count(&self, of: impl Fn(&ImportRow) -> bool) -> usize {
        self.rows.iter().filter(|row| of(row)).count()
    }

    /// What came of the files, in a line: 「新增 15 · 附到已有文献 3」.
    pub fn summary(&self) -> String {
        let mut facts = vec![];
        for (n, text) in [
            (self.count(|row| row.outcome == Imported::Added), t("新增")),
            (self.count(|row| row.outcome == Imported::Attached), t("附到已有文献")),
            (self.count(|row| row.outcome == Imported::InTrash), t("在回收站里、未导入")),
            (self.count(|row| row.outcome == Imported::Held), t("已有、跳过")),
            (self.count(|row| matches!(row.outcome, Imported::Failed(_))), t("没有成功")),
            (self.unknown.len(), t("没认出、跳过")),
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
            (true, true) => t("正在停止"),
            (true, false) => t("已停止"),
            (false, true) => t("正在查找"),
            (false, false) => t("查找完成"),
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
    /// The file it was selected in: the paper's main text or an attachment.
    pub file_id: String,
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
    /// How many messages there were when the review last took a step: an answer from before it is not offered to the next step.
    pub review_mark: usize,
    /// What is selected in an answer: the answer's place among the messages, the two ends in order of selecting, and whether the pointer is still held.
    pub picked: Option<(usize, crate::markdown::Place, crate::markdown::Place, bool)>,
    /// Where that selection was last drawn, and how big the bar of actions beside it came out (`chat::Spot`).
    pub spot: Option<crate::chat::Spot>,
    pub bar_size: Option<Size<Pixels>>,
    /// The quotations of the answers looked up in the papers they name, by label, page and quotation.
    pub checks: HashMap<(String, Option<usize>, Option<usize>, String), ai::Quoted>,
    /// When the question being answered was asked.
    pub started: Option<Instant>,
    /// What the last 撤回 took away, until the next question: it can be put back (`Cmd::Unretract`).
    pub retracted: Option<Retracted>,
}

/// What 撤回 took out of a conversation, and what the box held before.
pub struct Retracted {
    /// The questions and answers taken away, in order.
    tail: Vec<ChatItem>,
    /// The box's text and pictures before the question came back into it.
    draft: String,
    attachments: Vec<ChatImage>,
}

impl Retracted {
    pub fn questions(&self) -> usize {
        self.tail.iter().filter(|item| matches!(item, ChatItem::User { .. })).count()
    }
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
    pub volume: Entity<TextInput>,
    pub issue: Entity<TextInput>,
    pub pages: Entity<TextInput>,
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

/// 通过 DOI / PMID 添加 at work on what was typed: how far it is, and what came of each identifier.
#[derive(Default)]
pub struct IdRun {
    pub total: usize,
    pub done: usize,
    /// Ids of the papers added.
    added: Vec<String>,
    /// A sentence for each paper the library (or its trash) already held.
    skipped: Vec<String>,
    /// The identifiers nothing came of, each with the reason.
    failed: Vec<(String, String)>,
}

pub enum Modal {
    ExportNote(Box<crate::notes::Form>),
    Library { editing: Option<String>, name: Entity<TextInput>, topic: Entity<TextInput>, color: String },
    /// `run` while the identifiers are looked up.
    AddById { input: Entity<TextInput>, run: Option<IdRun> },
    Paper(Box<PaperForm>),
    Complete(Box<CompleteForm>),
    Fields { editing: Option<String>, name: Entity<TextInput>, options: Entity<TextInput>, hint: Entity<TextInput>, kind: FieldKind, in_table: bool },
    Search { editing: Option<String>, kind: String, name: Entity<TextInput>, query: Entity<TextInput>, max: Entity<TextInput> },
    Confirm { title: String, message: String, action: String, cmd: Cmd },
    /// Another name for a tag, on every paper that has it.
    Tag { tag: String, name: Entity<TextInput> },
    /// Tags for several papers at once.
    TagPapers { ids: Vec<String>, name: Entity<TextInput> },
    /// 清空回收站: the papers in the trash deleted from disk, once the phrase is typed.
    EmptyTrash { phrase: Entity<TextInput> },
    /// What came of each PDF of the last import (`Library::import`).
    ImportReport,
    /// `open`: Zotero holds its database right now, as far as the usual place of it tells.
    Zotero { open: bool },
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
    /// A figure of the figure library, or a piece of a PDF page about to become one.
    Figure(Box<crate::figures::FigureForm>),
    /// 提取: which fields to ask the assistant about, for which papers.
    Extract(Box<crate::extract::Form>),
    /// 写作: which paragraphs of a Word file without heading styles start a section.
    Headings(Box<crate::comments::HeadingsForm>),
}

pub struct Library {
    pub info: LibraryInfo,
    pub store: Store,
    pub papers: Vec<Paper>,
    pub fields: Vec<FieldDef>,
    pub files: Vec<PaperFile>,
    pub trash: Vec<TrashEntry>,
    /// 筛选's rounds, oldest first (`Store::begin_round`).
    pub rounds: Vec<wisp_core::store::ScreeningRound>,
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
    /// The papers the library's next conversation is kept to: picked in the list, or dragged into the
    /// conversation from the panel over it. None means the list on show, narrowed by that panel's search.
    pub asked: Vec<String>,
    /// The search box of the panel over the library's conversation, and what it holds.
    pub scope_find: Entity<TextInput>,
    pub scope_words: String,
    /// The papers of the library that hold those words; `None` while nothing is typed. Kept, since a
    /// frame cannot search every abstract: `scope_refilter` makes it anew when the words or the papers change.
    pub scope_hits: Option<HashSet<String>>,
    pub tab: Tab,
    /// The detail panel takes most of the window (reading a PDF).
    pub wide: bool,
    /// Reading mode: the sidebar and the list are put away and the paper has the whole window.
    pub reading: bool,
    /// The paper's PDF is wanted to the right of its conversation. While the paper is asked about, the
    /// window is laid out for the two (`Workspace::asking`).
    pub split: bool,
    /// The tab the asking was come to from, which leaving it goes back to.
    pub ask_from: Tab,
    /// The page 写作 was opened from, which leaving a manuscript goes back to (`Library::enter_write`).
    pub write_from: Page,
    /// Whether the window was last drawn laid out for asking: the list finds its paper again when that changes.
    pub ask_drawn: bool,
    /// The floating conversation's window or button while the pointer holds it (floating.rs).
    pub float_drag: Option<crate::floating::Drag>,
    /// The size of the area the floating conversation keeps within, as last drawn.
    pub float_area: (f32, f32),
    pub notes: Entity<TextInput>,
    pub note_links: BTreeMap<String, String>,
    pub pdf: Option<Entity<PdfView>>,
    pdf_events: Option<Subscription>,
    /// The fit PDF pages are drawn at (`Zoom::carried`): it stays from paper to paper, a percentage does not.
    pub zoom: Zoom,
    pub library_chat: Chat,
    pub paper_chat: Chat,
    /// The conversation that writes the manuscript on the writing page (`write_agent.rs`).
    pub write_chat: Chat,
    pub table_scroll: UniformListScrollHandle,
    pub detail_scroll: ScrollHandle,
    /// Files whose text is being extracted.
    pub extracting: HashSet<String>,
    /// The file of each paper chosen to read, when it is not the main text (`Cmd::ReadFile`).
    pub read_file: HashMap<String, String>,
    /// Where each file was read at when another took its place (`PdfView::place`).
    pub file_places: HashMap<String, (usize, f32)>,
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
    /// 图片库: the figures cut out of the library's PDFs.
    pub gallery: crate::figures::Gallery,
    /// 提取: the assistant's proposals for the custom fields, its run over the papers, and their confirmation.
    pub extract: crate::extract::Extract,
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
    /// 连接器: where the connection of each one tried stands (`custom.rs`).
    pub connections: Vec<(wisp_core::connector::Connector, crate::custom::Connection)>,
    /// 技能: what the skills' folder held when it was last read.
    pub skills: Vec<wisp_core::skill::Skill>,
    pub update: UpdateState,
    /// The version the automatic check last announced, so it says it once.
    pub update_told: Option<String>,
    /// Where the window is, as last written to `session.json` (`keep_session`).
    session: Session,
    pub modal: Option<Modal>,
    pub modal_error: String,
    pub modal_subscriptions: Vec<Subscription>,
    pub menu: Option<Menu>,
    /// The command palette, while it is open (`palette.rs`).
    pub palette: Option<crate::palette::Palette>,
    pub toast: Option<Toast>,
    pub home_scroll: ScrollHandle,
    /// The edge being dragged: the sidebar's, the detail panel's, or the one between a paper's conversation and its PDF.
    pub resizing: Option<Edge>,
    pub column_drag: Option<ColumnDrag>,
    /// What is selected in a text of the window that is not an answer or a text field.
    pub picked: Option<crate::select::Picked>,
    /// A paper of the scope panel is held and on its way into the conversation (`chat::PaperDrag`).
    pub pinning: bool,
    /// The pointer is over the filters of a list too narrow for them, or over the card that shows them all.
    pub filters_over: [bool; 2],
    /// Those filters were drawn in this frame; a frame without them forgets the pointer.
    pub filters_drawn: bool,
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
            review_mark: 0,
            picked: None,
            spot: None,
            bar_size: None,
            checks: HashMap::new(),
            started: None,
            retracted: None,
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

    /// What was said from this place on is gone. The model's turns in
    /// `wire` belong to the conversation as it was: it is written again
    /// from what is left (as `ask` replays) with the next question.
    fn cut(&mut self, at: usize) -> Vec<ChatItem> {
        let tail = self.messages.split_off(at.min(self.messages.len()));
        self.sources_open.retain(|&i| i < at);
        self.tools_open.retain(|&i| i < at);
        self.review_mark = self.review_mark.min(at);
        self.picked = None;
        self.spot = None;
        self.error.clear();
        self.note.clear();
        self.history = None;
        self.wire.clear();
        self.protocol = None;
        tail
    }

    /// The question has been answered, or given up on.
    fn end_run(&mut self) {
        self.busy = false;
        self.started = None;
        self.pending = None;
        self.running.clear();
        self.drop_empty_answer();
        if self.note.is_empty() && self.error.is_empty() && !matches!(self.messages.last(), Some(ChatItem::Answer(_))) { self.note = t("模型没有返回内容。").into() }
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

    pub(crate) fn reset(&mut self, cx: &mut App) {
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
        self.review_mark = 0;
        self.picked = None;
        self.spot = None;
        self.checks.clear();
        self.started = None;
        self.retracted = None;
        self.input.update(cx, |input, cx| {
            input.choosing = false;
            input.set_text("", cx);
        });
    }
}

impl Library {
    /// 写作 becomes the page, and the one it was opened from is kept for the way back (`Cmd::LeaveWrite`).
    pub(crate) fn enter_write(&mut self) {
        if self.page != Page::Write { self.write_from = self.page }
        self.page = Page::Write;
    }

    pub fn chat(&self, talk: Talk) -> &Chat {
        match talk {
            Talk::Library => &self.library_chat,
            Talk::Paper => &self.paper_chat,
            Talk::Write => &self.write_chat,
        }
    }

    pub(crate) fn chat_mut(&mut self, talk: Talk) -> &mut Chat {
        chat_of!(self, talk)
    }

    /// Ends the selection made in an answer, in whichever conversation it is; says whether there was one.
    /// One the pointer still holds is being made, and stays.
    pub(crate) fn unpick(&mut self) -> bool {
        let mut ended = false;
        for talk in [Talk::Library, Talk::Paper, Talk::Write] {
            let chat = self.chat_mut(talk);
            if chat.picked.is_some_and(|(.., held)| !held) {
                chat.picked = None;
                ended = true;
            }
        }
        ended
    }

    /// What a conversation of this kind is about, which is what it is kept under in the library's history
    /// (`conversations.paper_id`): nothing for the library's, the selected paper, the manuscript being written.
    /// None while there is no such paper or manuscript.
    pub fn talk_about(&self, talk: Talk) -> Option<String> {
        match talk {
            Talk::Library => Some(String::new()),
            Talk::Paper => self.selected.clone(),
            Talk::Write => Some(self.writing.agent.about.clone()).filter(|id| !id.is_empty()),
        }
    }

    pub fn paper(&self, id: &str) -> Option<&Paper> {
        self.papers.iter().find(|p| p.id == id)
    }

    /// The papers a question to the library is put to: those it is kept to, else what the panel over the conversation lists.
    pub fn scope_papers(&self) -> Vec<&Paper> {
        if self.asked.is_empty() { return self.scope_listed() }
        let asked: HashSet<&str> = self.asked.iter().map(String::as_str).collect();
        self.papers.iter().filter(|p| asked.contains(p.id.as_str())).collect()
    }

    /// The list on show, narrowed by the search of the panel over the library's conversation.
    pub fn scope_listed(&self) -> Vec<&Paper> {
        self.shown.iter().map(|&i| &self.papers[i]).filter(|p| self.scope_has(&p.id)).collect()
    }

    /// Whether a paper holds the words of that panel's search.
    pub fn scope_has(&self, id: &str) -> bool {
        self.scope_hits.as_ref().is_none_or(|hits| hits.contains(id))
    }

    /// Look the panel's words up in the papers again.
    pub fn scope_refilter(&mut self) {
        let words = self.scope_words.trim();
        self.scope_hits = (!words.is_empty()).then(|| self.papers.iter().filter(|p| filter::says(p, words)).map(|p| p.id.clone()).collect());
    }

    /// A new conversation with the library is about the list on show again: nothing kept to, nothing searched for.
    pub fn scope_reset(&mut self, cx: &mut App) {
        self.asked.clear();
        self.scope_words.clear();
        self.scope_hits = None;
        self.scope_find.update(cx, |input, cx| input.set_text("", cx));
    }

    /// The panel of the papers in scope is on show over the library's conversation.
    pub fn materials_shown(&self) -> bool {
        let chat = &self.library_chat;
        self.page == Page::Ask && chat.materials_open && chat.history.is_none() && (chat.ctx.is_some() || chat.messages.is_empty())
    }

    pub fn selected_paper(&self) -> Option<&Paper> {
        self.selected.as_deref().and_then(|id| self.paper(id))
    }

    pub fn paper_files(&self, id: &str) -> Vec<&PaperFile> {
        self.files.iter().filter(|f| f.paper_id == id).collect()
    }

    /// The stored PDF to preview and read: the one chosen (`read_file`), else the first full-text PDF, else any PDF.
    pub fn pdf_file(&self, id: &str) -> Option<&PaperFile> {
        let files = self.paper_files(id);
        let chosen = self.read_file.get(id).and_then(|chosen| files.iter().find(|f| &f.id == chosen && f.format == "pdf"));
        chosen.or(files.iter().find(|f| f.kind == "fulltext" && f.format == "pdf")).or(files.iter().find(|f| f.format == "pdf")).copied()
    }

    /// The paper's main-text PDF, whatever is read now.
    pub fn main_pdf(&self, id: &str) -> Option<&PaperFile> {
        let files = self.paper_files(id);
        files.iter().find(|f| f.kind == "fulltext" && f.format == "pdf").or(files.iter().find(|f| f.format == "pdf")).copied()
    }

    /// The paper's PDF attachments, in the order `S1`, `S2`, … count them (`Store::attachment_pages`).
    pub fn attachments(&self, id: &str) -> Vec<&PaperFile> {
        self.paper_files(id).into_iter().filter(|f| f.kind != "fulltext" && f.format == "pdf").collect()
    }

    /// The number an attachment is cited by (`S2` is 2); `None` for the main text.
    pub fn attachment_number(&self, paper: &str, file: &str) -> Option<usize> {
        self.attachments(paper).iter().position(|f| f.id == file).map(|i| i + 1)
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

    /// The decided papers that are not included (相关, 证据不足, 不相关):
    /// what 把未纳入的移到回收站 takes.
    pub fn unincluded(&self) -> Vec<String> {
        self.papers.iter().filter(|p| !matches!(p.screening.as_str(), "pending" | "included")).map(|p| p.id.clone()).collect()
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
        for chat in [&mut self.paper_chat, &mut self.library_chat, &mut self.write_chat] {
            if !chat.busy { chat.forget_context() }
        }
    }

    /// Look up, in the papers they name, the quotations of the answers
    /// that have not been looked up yet. An answer still being written is
    /// left until it is whole.
    pub fn check_quotes(&mut self, talk: Talk) {
        let chat = self.chat(talk);
        let finished = if chat.busy { chat.messages.len().saturating_sub(1) } else { chat.messages.len() };
        let mut wanted: Vec<(ai::Citation, String)> = vec![];
        for item in &chat.messages[..finished] {
            let ChatItem::Answer(text) = item else { continue };
            for citation in ai::citations(text) {
                if citation.quote.is_empty() || chat.checks.contains_key(&(citation.label.clone(), citation.file, citation.page, citation.quote.clone())) { continue }
                let Some(source) = chat.sources().iter().find(|source| source.label == citation.label) else { continue };
                wanted.push((citation, source.paper_id.clone()));
            }
        }
        if wanted.is_empty() { return }
        // Each paper's pages are read once, however often it is quoted.
        let mut texts: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let mut attachment_texts: HashMap<(String, usize), Option<Vec<String>>> = HashMap::new();
        let mut checks = vec![];
        for (citation, paper_id) in wanted {
            let Some(paper) = self.papers.iter().find(|paper| paper.id == paper_id) else { continue };
            // A quotation from an attachment is looked up in that attachment's pages.
            let pages = match citation.file {
                Some(n) => attachment_texts.entry((paper_id.clone(), n)).or_insert_with(|| self.store.attachment_pages(&paper_id).ok().and_then(|files| files.into_iter().nth(n - 1)).map(|(_, pages)| pages).filter(|pages| !pages.is_empty())),
                None => texts.entry(paper_id.clone()).or_insert_with(|| self.store.paper_pages(&paper_id).ok().flatten()),
            };
            let found = ai::check_quote(&citation.quote, citation.page, paper, pages.as_deref());
            checks.push(((citation.label, citation.file, citation.page, citation.quote), found));
        }
        self.chat_mut(talk).checks.extend(checks);
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
        self.scope_refilter();
    }

    /// Read everything again after a change.
    pub fn reload(&mut self) -> anyhow::Result<()> {
        self.note_links = self.store.note_links()?;
        self.papers = self.store.list_papers()?;
        self.fields = self.store.list_fields()?;
        self.files = self.store.list_files()?;
        self.trash = self.store.list_trash()?;
        self.rounds = self.store.screening_rounds()?;
        self.searches = self.store.list_searches()?;
        self.suggestions = self.store.list_suggestions()?.into_iter().map(|s| (s.paper_id.clone(), s)).collect();
        self.batches = self.store.list_batches(30)?;
        self.writing.structures = self.store.structures()?;
        self.origins = self.store.file_origins()?;
        self.gallery.items = self.store.list_figures()?;
        self.extract.proposals = wisp_core::extract::Proposals::new(self.store.list_proposals()?);
        self.extract_settle();
        // Filters on a field that no longer exists would hide everything.
        let known: HashSet<&str> = self.fields.iter().map(|f| f.id.as_str()).collect();
        self.filters.fields.retain(|id, _| known.contains(id.as_str()));
        if self.selected.as_ref().is_some_and(|id| self.paper(id).is_none()) {
            self.selected = None;
            self.pdf = None;
        }
        let held: HashSet<&str> = self.papers.iter().map(|p| p.id.as_str()).collect();
        self.screen.round.keep(|id| held.contains(id));
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
            toast = Some(Toast { message: tf!("{err:#}；本次使用默认设置。", "{err:#}; using default settings for this session.").into(), error: true, at: Instant::now() });
            Settings::default()
        });
        crate::i18n::set_language(settings.language);
        wisp_net::set_proxy(&settings.proxy);
        let root = settings.data_root(&default_root());
        let _ = std::fs::create_dir_all(&root);
        let focus = cx.focus_handle();
        window.focus(&focus);
        theme::set_accent("blue");
        shortcuts::rebuild(&settings.shortcuts, cx);
        let libraries = libraries::list(&root);
        let mut this = Workspace { focus, settings, settings_path, root, libraries, lib: None, settings_view: None, connections: vec![], skills: vec![], update: UpdateState::Idle, update_told: None, session: Session::default(), modal: None, modal_error: String::new(), modal_subscriptions: vec![], menu: None, palette: None, toast, home_scroll: ScrollHandle::new(), resizing: None, column_drag: None, picked: None, pinning: false, filters_over: [false; 2], filters_drawn: false };
        this.apply_appearance(window);
        this.apply_typography(window, cx);
        cx.observe_window_appearance(window, |this, window, _| this.apply_appearance(window)).detach();
        this.watch_updates(cx);
        this.watch_searches(cx);
        this.load_skills();
        this.connect_all(cx);
        this
    }

    /// Once the window is there: go back to where it was last (回到上次的界面), and from now on keep where it is.
    pub fn start_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A scripted debug start says itself where it begins (its `restore` step is this).
        if self.settings.restore_session && std::env::var_os("WISP_PAPER_DEV").is_none() { self.restore_session(window, cx) }
        cx.observe_self(|this, _| this.keep_session()).detach();
    }

    /// Write down where the window is whenever that changed, so the next start (after quitting,
    /// an update or a crash) can be the same place: the library, its page, the paper on show.
    // ponytail: the window's size, the scroll positions and the PDF's page are not kept; fields of `Session` when they are missed.
    fn keep_session(&mut self) {
        if !self.settings.restore_session { return }
        let now = self.lib.as_ref().map(|lib| Session { library: lib.info.id.clone(), page: lib.page.key().into(), paper: lib.selected.clone().unwrap_or_default(), tab: lib.tab.key().into(), wide: lib.wide, reading: lib.reading }).unwrap_or_default();
        if now != self.session {
            let _ = now.save(&session_file());
            self.session = now;
        }
    }

    /// Go back to where `session.json` says the window was. A library or a paper that is gone since is simply not opened.
    fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = Session::load(&session_file());
        if session.library.is_empty() { return }
        self.open_library(&session.library, window, cx);
        if self.lib.as_ref().is_none_or(|lib| lib.info.id != session.library) { return }
        self.run(Cmd::SetPage(Page::from_key(&session.page)), window, cx);
        if self.lib.as_ref().is_some_and(|lib| lib.paper(&session.paper).is_some()) {
            self.select(Some(session.paper.clone()), window, cx);
            self.run(Cmd::SetTab(Tab::from_key(&session.tab)), window, cx);
            if let Some(lib) = self.lib.as_mut() { (lib.wide, lib.reading) = (session.wide, session.reading) }
        }
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

    /// The rows of a menu of models: each provider's models that `fits`, in its group; one without a key says so beside it.
    fn model_items(&self, fits: impl Fn(&wisp_core::provider::Model) -> bool, chosen: &str, cmd: impl Fn(String) -> Cmd, items: &mut Vec<MenuItem>, breaks: &mut Vec<usize>) {
        let fits = |m: &wisp_core::provider::Model| !m.model.trim().is_empty() && fits(m);
        let mut providers = self.settings.providers.iter().filter(|p| p.models.iter().any(fits)).peekable();
        while let Some(provider) = providers.next() {
            let note: SharedString = if provider.key().is_empty() { tf!("{} · 缺 API Key", "{} · no API key", provider.title()).into() } else { provider.title().into() };
            for model in provider.models.iter().filter(|m| fits(m)) {
                // A model that judges is no model like the others in the list: it says so.
                let note = if model.judges() { tf!("{note} · 判断模型", "{note} · judging model").into() } else { note.clone() };
                items.push(MenuItem { label: model.model.trim().to_string().into(), checked: chosen == model.id, note: Some(note), cmd: cmd(model.id.clone()) });
            }
            if providers.peek().is_some() { breaks.push(items.len()) }
        }
    }

    pub(crate) fn open_menu(&mut self, kind: MenuKind, at: Point<Pixels>, cx: &mut Context<Self>) {
        let item = |label: &str, cmd: Cmd| MenuItem { label: label.to_string().into(), checked: false, note: None, cmd };
        let mut items = vec![];
        let mut breaks = vec![];
        let paper = match &kind {
            MenuKind::Paper(id) => Some(id.clone()),
            _ => None,
        };
        let filter = matches!(kind, MenuKind::Filter(_));
        match kind {
            MenuKind::Paper(_) | MenuKind::Papers(_) | MenuKind::Trashed(_) | MenuKind::File(_) | MenuKind::Tag(_) | MenuKind::Conversation(..) | MenuKind::Text(_) | MenuKind::Answer(..) => (items, breaks) = self.context_menu(&kind),
            MenuKind::Figure(id) => (items, breaks) = self.figure_menu(&id),
            MenuKind::Add => {
                items.push(item(t("通过 DOI / PMID 添加…"), Cmd::AddById));
                items.push(item(t("导入 PDF 文件…"), Cmd::ImportPdfs));
                items.push(item(t("导入文件夹…"), Cmd::ImportFolder));
                items.push(item(t("导入 BibTeX / RIS…"), Cmd::ImportReferences));
                items.push(item(t("从 Zotero 导入…"), Cmd::ImportZotero));
                items.push(item(t("手动录入…"), Cmd::AddManual));
            }
            MenuKind::Manuscripts | MenuKind::Section(_) | MenuKind::ManuscriptExport | MenuKind::WriteLanguage => (items, breaks) = self.manuscript_menu(&kind),
            MenuKind::WriteNarrow => items = self.narrowing_menu(),
            MenuKind::Export => {
                items.push(item("BibTeX（.bib）", Cmd::Export(ExportKind::Bibtex)));
                items.push(item(t("表格（.csv）"), Cmd::Export(ExportKind::Csv)));
                if let Some(id) = self.lib.as_ref().map(|l| l.info.id.clone()) {
                    items.push(item(t("整个文献库，含 PDF（.wisppaper）…"), Cmd::ExportLibrary(id.clone(), true)));
                    items.push(item(t("整个文献库，不含 PDF（.wisppaper）…"), Cmd::ExportLibrary(id, false)));
                }
            }
            MenuKind::LibraryCard(id) => {
                items.push(item(t("打开"), Cmd::OpenLibrary(id.clone())));
                items.push(item(t("编辑名称与主题…"), Cmd::EditLibrary(id.clone())));
                items.push(item(t("导出文献库，含 PDF…"), Cmd::ExportLibrary(id.clone(), true)));
                items.push(item(t("导出文献库，不含 PDF…"), Cmd::ExportLibrary(id.clone(), false)));
                items.push(item(t("移除文献库…"), Cmd::AskRemoveLibrary(id)));
            }
            MenuKind::PdfZoom => {
                let Some(current) = self.lib.as_ref().and_then(|lib| lib.pdf.as_ref()).map(|pdf| pdf.read(cx).zoom()) else { return };
                let fits = [(t("适应宽度"), Zoom::FitWidth), (t("适应整页"), Zoom::FitPage)].map(|(label, zoom)| (label.to_string(), zoom));
                let sizes = [50., 75., 100., 125., 150., 200., 300., 400.].map(|percent| (if percent == 100. { t("100%（实际大小）").to_string() } else { format!("{percent}%") }, Zoom::Percent(percent)));
                let key = crate::shortcuts::key_of("zoom-fit", &self.settings.shortcuts);
                for (label, zoom) in fits.into_iter().chain(sizes) {
                    let note = (zoom == Zoom::FitWidth && !key.is_empty()).then(|| crate::shortcuts::pretty(&key).into());
                    items.push(MenuItem { label: label.into(), checked: current == zoom, note, cmd: Cmd::PdfZoom(ZoomTo::Set(zoom)) });
                }
            }
            MenuKind::PdfSplit => {
                let Some(current) = self.lib.as_ref().and_then(|lib| lib.pdf.as_ref()).map(|pdf| pdf.read(cx).split()) else { return };
                for (label, split) in [("不分屏", Split::Off), ("左右分屏", Split::Side), ("上下分屏", Split::Stacked)] {
                    items.push(MenuItem { label: t(label).into(), checked: current == split, note: None, cmd: Cmd::PdfSplit(split) });
                }
            }
            MenuKind::PdfFiles => {
                // The paper's main text and each PDF attachment, the one read ticked.
                let Some(lib) = &self.lib else { return };
                let Some(paper) = lib.selected.clone() else { return };
                let shown = lib.pdf_file(&paper).map(|f| f.id.clone());
                let main = lib.main_pdf(&paper).map(|f| (f.id.clone(), tf!("正文 · {}", "Main text · {}", f.title)));
                let attachments = lib.attachments(&paper).into_iter().enumerate().map(|(i, f)| (f.id.clone(), tf!("附件 S{} · {}", "Attachment S{} · {}", i + 1, f.title)));
                for (id, label) in main.into_iter().chain(attachments) {
                    items.push(MenuItem { label: label.into(), checked: shown.as_ref() == Some(&id), note: None, cmd: Cmd::ReadFile(id) });
                }
            }
            MenuKind::Columns => {
                let Some(lib) = &self.lib else { return };
                for (key, name) in crate::library::COLUMNS {
                    items.push(MenuItem { label: t(name).into(), checked: !self.settings.columns_hidden.iter().any(|k| k == key), note: None, cmd: Cmd::ToggleColumn(key.into()) });
                }
                for field in &lib.fields {
                    items.push(MenuItem { label: field.name.clone().into(), checked: field.in_table, note: None, cmd: Cmd::ToggleFieldInTable(field.id.clone()) });
                }
                if !self.settings.column_widths.is_empty() {
                    breaks.push(items.len());
                    items.push(item(t("恢复默认列宽"), Cmd::ResetColumnWidths));
                }
            }
            MenuKind::Sort => {
                let Some(lib) = &self.lib else { return };
                let current = if lib.filters.sort.is_empty() { "newest" } else { lib.filters.sort.as_str() };
                for (value, label) in [("newest", t("最近添加")), ("year-desc", t("发表年份 ↓")), ("year-asc", t("发表年份 ↑")), ("title-asc", t("题目 A→Z")), ("title-desc", t("题目 Z→A")), ("journal-asc", t("期刊 A→Z")), ("authors-asc", t("作者 A→Z"))] {
                    items.push(MenuItem { label: label.into(), checked: current == value, note: None, cmd: Cmd::SetSort(value.into()) });
                }
            }
            MenuKind::Reading(id) => {
                let Some(current) = self.lib.as_ref().and_then(|l| l.paper(&id)).map(|p| p.reading.clone()) else { return };
                for (value, label) in READINGS {
                    items.push(MenuItem { label: t(label).into(), checked: current == value, note: None, cmd: Cmd::SetReading(id.clone(), value.into()) });
                }
            }
            MenuKind::InsertInto(text) => {
                let Some(lib) = self.lib.as_ref() else { return };
                let Some(text) = text.or_else(|| lib.library_chat.picked_text()) else { return };
                let Some(manuscript) = lib.writing.current().or(lib.writing.manuscripts.first()) else { return };
                for section in &manuscript.sections {
                    let length = section.body.chars().count();
                    items.push(MenuItem { label: section.name.clone().into(), checked: false, note: Some(if length == 0 { t("空").into() } else { tf!("{length} 字", "{length} chars").into() }), cmd: Cmd::InsertInto(section.id.clone(), text.clone()) });
                }
            }
            MenuKind::AddProvider => {
                for preset in &wisp_core::provider::PRESETS {
                    let note = self.settings.providers.iter().any(|p| p.preset == preset.key).then(|| SharedString::from(t("已添加")));
                    items.push(MenuItem { label: preset.name.into(), checked: false, note, cmd: Cmd::AddProvider(preset.key.into()) });
                }
                breaks.push(items.len());
                items.push(MenuItem { label: t("其他 OpenAI 兼容服务").into(), checked: false, note: None, cmd: Cmd::AddProvider(String::new()) });
            }
            MenuKind::ModelEffort(index) => {
                let Some(view) = self.settings_view.as_ref() else { return };
                let Some(model) = self.settings.providers.get(view.provider()).and_then(|p| p.models.get(index)) else { return };
                if model.judges() {
                    // A model that judges has nothing to set: it is asked one way and does not think aloud.
                } else if model.draws() {
                    // A model that draws does not think: what is chosen is how it is asked for a picture.
                    let guessed = wisp_core::imagegen::guess(&self.settings.providers[view.provider()].base_url, &model.model);
                    for (key, name) in wisp_core::imagegen::INTERFACES {
                        let note = key.is_empty().then(|| SharedString::from(t(guessed.name())));
                        items.push(MenuItem { label: t(name).into(), checked: model.interface == key, note, cmd: Cmd::SetModelInterface(index, key.to_string()) });
                    }
                } else {
                    for (key, name) in ai::EFFORTS {
                        items.push(MenuItem { label: t(name).into(), checked: model.effort == key, note: None, cmd: Cmd::SetEffort(index, key.to_string()) });
                    }
                }
                if !items.is_empty() { breaks.push(items.len()) }
                // What kind of model it is: the name tells when it is added, and here it can be said otherwise.
                for (kind, name) in [("", t("对话模型")), (wisp_core::imagegen::KIND, t("生图模型")), (wisp_core::judge::KIND, t("判断模型"))] {
                    items.push(MenuItem { label: name.into(), checked: model.kind == kind, note: None, cmd: Cmd::SetModelKind(index, kind.to_string()) });
                }
            }
            MenuKind::TaskModel(task) => {
                let bound = self.settings.tasks.get(task).to_string();
                // Questions always have a model: the first one until another is chosen.
                let chosen = if task == Task::Chat || task.draws() { self.settings.profile_for(task).map(|p| p.id).unwrap_or_default() } else { bound };
                if task != Task::Chat && !task.draws() {
                    items.push(MenuItem { label: t("与助手相同").into(), checked: chosen.is_empty(), note: None, cmd: Cmd::SetTaskModel(task, String::new()) });
                    breaks.push(items.len());
                }
                self.model_items(|m| task.takes(m), &chosen, |id| Cmd::SetTaskModel(task, id), &mut items, &mut breaks);
            }
            MenuKind::ExtractRetry => {
                let Some(lib) = self.lib.as_ref() else { return };
                // 提取 writes values out: a model that only judges cannot.
                self.model_items(|m| m.speaks(), &self.extract_profile(&lib.extract.with).id, |id| Cmd::Extract(crate::extract::Do::Retry(id)), &mut items, &mut breaks);
            }
            MenuKind::Screening(id) => {
                let Some(current) = self.lib.as_ref().and_then(|l| l.paper(&id)).map(|p| p.screening.clone()) else { return };
                let hints = [("pending", t("还没有判断")), ("included", t("符合主题，重点阅读")), ("excluded", t("可能有用，保留备查")), ("uncertain", t("暂时判断不了")), ("irrelevant", t("与主题无关，默认隐藏"))];
                for (value, label) in SCREENINGS {
                    let hint = hints.iter().find(|(v, _)| *v == value).map(|(_, h)| SharedString::from(*h));
                    items.push(MenuItem { label: t(label).into(), checked: current == value, note: hint, cmd: Cmd::SetScreening(id.clone(), value.into()) });
                }
            }
            MenuKind::Filter(key) => {
                let Some(lib) = &self.lib else { return };
                let f = &lib.filters;
                let fixed = |table: &[(&str, &str)]| table.iter().map(|(v, l)| (v.to_string(), t(l).to_string(), None)).collect::<Vec<_>>();
                let counted = |list: Vec<(String, usize)>| list.into_iter().take(40).map(|(v, n)| (v.clone(), v, Some(n))).collect::<Vec<_>>();
                let (current, choices): (String, Vec<(String, String, Option<usize>)>) = match &key {
                    FilterKey::Tag => (f.tag.clone(), counted(filter::tag_counts(&lib.papers))),
                    FilterKey::Year => (f.year.clone(), counted(filter::years(&lib.papers))),
                    FilterKey::Source => (f.source.clone(), counted(filter::source_counts(&lib.papers))),
                    FilterKey::Reading => (f.reading.clone(), fixed(&READINGS)),
                    FilterKey::Screening => (f.screening.clone(), fixed(&SCREENINGS)),
                    FilterKey::Fulltext => (f.fulltext.clone(), fixed(&FULLTEXTS)),
                    FilterKey::Preprint => (f.preprint.clone(), fixed(&[("yes", t("只看预印本")), ("no", t("不含预印本"))])),
                    FilterKey::Field(id) => (f.fields.get(id).cloned().unwrap_or_default(), counted(filter::field_counts(&lib.papers, id))),
                };
                items.push(MenuItem { label: t("全部").into(), checked: current.is_empty(), note: None, cmd: Cmd::SetFilter(key.clone(), String::new()) });
                for (value, label, count) in choices {
                    items.push(MenuItem { label: label.into(), checked: current == value, note: count.map(|n| n.to_string().into()), cmd: Cmd::SetFilter(key.clone(), value) });
                }
            }
        }
        if items.is_empty() { return }
        self.menu = Some(Menu { at, items, breaks, active: None, paper, filter });
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
                    title: tf!("移除文献库「{}」？", "Remove library “{}”?", info.name),
                    message: tf!("文献库会从首页消失。它的文件夹（{} 篇文献、PDF 和笔记）会移到数据目录下的 removed 文件夹，不会被删除，需要时可以手动移回。", "The library will disappear from home. Its folder ({} papers, PDFs and notes) moves to removed in the data directory. You can move it back manually.", info.paper_count),
                    action: t("移除").into(),
                    cmd: Cmd::RemoveLibrary(id),
                });
            }
            Cmd::RemoveLibrary(id) => {
                self.modal = None;
                // The database must be closed before its folder can move.
                if self.lib.as_ref().is_some_and(|l| l.info.id == id) { self.lib = None }
                let Some(info) = self.libraries.iter().find(|l| l.id == id).cloned() else { return };
                match libraries::remove(&self.root, &info) {
                    Ok(target) => self.say(tf!("已移到 {}", "Moved to {}", target.display()), cx),
                    Err(err) => self.fail(format!("{err:#}"), cx),
                }
                self.libraries = libraries::list(&self.root);
            }
            Cmd::ExportLibrary(id, attachments) => self.export_library(&id, attachments, cx),
            Cmd::ImportLibrary => self.pick_library_file(cx),
            Cmd::RevealFolder => {
                let path = self.lib.as_ref().map_or(self.root.clone(), |l| l.info.dir.clone());
                cx.open_with_system(&path);
            }
            Cmd::OpenUrl(url) => cx.open_url(&url),
            Cmd::NoteOpen(talk, notion) => self.open_note(talk, notion, window, cx),
            Cmd::NotePick(at) => self.pick_note(at, cx),
            Cmd::NoteDestination(notion) => self.note_destination(notion, cx),
            Cmd::NotePreview => self.preview_note(cx),
            Cmd::NoteSave(notion) => self.save_note(notion, cx),
            Cmd::NoteResolve(saved) => self.resolve_note(saved, cx),

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
            Cmd::ToggleAutoDownload => {
                self.settings.auto_download = !self.settings.auto_download;
                self.save_settings(cx);
                // A release already found is fetched now rather than at the next check.
                if self.settings.auto_download && let UpdateState::Available(release, Some(target)) = self.update.clone() { self.download_update(release, target, false, cx) }
            }
            Cmd::ToggleRestoreSession => {
                self.settings.restore_session = !self.settings.restore_session;
                self.save_settings(cx);
                // Off: nothing is kept. On: `keep_session` writes where the window is now.
                let _ = std::fs::remove_file(session_file());
                self.session = Session::default();
            }
            Cmd::ToggleKeepRunning => {
                self.settings.keep_running = !self.settings.keep_running;
                self.save_settings(cx);
            }
            Cmd::CheckUpdate => self.check_update(true, cx),
            Cmd::DownloadUpdate => {
                if let UpdateState::Available(release, Some(target)) = self.update.clone() { self.download_update(release, target, true, cx) }
            }
            Cmd::InstallUpdate => self.install_update(cx),
            Cmd::SetAppearance(value) => {
                self.settings.appearance = value;
                self.save_settings(cx);
                self.apply_appearance(window);
            }
            Cmd::SetFontFamily(slot, value) => self.set_font_family(slot, value, window, cx),
            Cmd::SetFontSize(value) => {
                self.settings.font_size = wisp_core::settings::font_size(value);
                self.save_settings(cx);
                self.apply_typography(window, cx);
            }
            Cmd::ResetTypography => {
                self.settings.font_family.clear();
                self.settings.cjk_font_family.clear();
                self.settings.font_size = wisp_core::settings::DEFAULT_FONT_SIZE;
                self.save_settings(cx);
                self.apply_typography(window, cx);
            }
            Cmd::SetLineSpacing(value) => {
                self.settings.reading_appearance.line_spacing = value;
                self.save_settings(cx);
                self.apply_typography(window, cx);
            }
            Cmd::SetParagraphSpacing(value) => {
                self.settings.reading_appearance.paragraph_spacing = value;
                self.save_settings(cx);
                self.apply_typography(window, cx);
            }
            Cmd::SetTextWidth(value) => {
                self.settings.reading_appearance.width = value;
                self.save_settings(cx);
                self.apply_typography(window, cx);
            }
            Cmd::ResetReadingAppearance => {
                self.settings.reading_appearance = Default::default();
                self.save_settings(cx);
                self.apply_typography(window, cx);
            }
            Cmd::ToggleHighContrast => {
                self.settings.high_contrast = !self.settings.high_contrast;
                self.save_settings(cx);
                self.apply_appearance(window);
            }
            Cmd::ResetAppearance => {
                self.settings.reset_appearance();
                self.dismiss_font_picker(window, cx);
                self.save_settings(cx);
                self.apply_typography(window, cx);
                self.apply_appearance(window);
            }
            Cmd::SetDensity(value) => {
                self.settings.density = value;
                self.save_settings(cx);
                window.refresh();
            }
            Cmd::SetLanguage(language) => {
                self.settings.language = language;
                crate::i18n::set_language(language);
                crate::background::set_menus(cx);
                self.save_settings(cx);
                cx.refresh_windows();
            }
            Cmd::RecordShortcut(id) => self.start_recording(id, window, cx),
            Cmd::ClearShortcut(id) => self.set_shortcut(&id, Some(String::new()), cx),
            Cmd::ResetShortcut(id) => self.set_shortcut(&id, None, cx),
            Cmd::ResetShortcuts => self.reset_shortcuts(cx),
            Cmd::PickProtocol(value) => self.pick_protocol(value, cx),
            Cmd::AiTab(tab) => self.open_ai_tab(tab, window, cx),
            Cmd::SelectProvider(index) => self.select_provider(index, cx),
            Cmd::AddProvider(preset) => self.add_provider(&preset, cx),
            Cmd::AskRemoveProvider(index) => {
                let Some(provider) = self.settings.providers.get(index) else { return };
                let serves: Vec<&str> = Task::ALL.into_iter().filter(|task| provider.models.iter().any(|m| m.id == self.settings.tasks.get(*task))).map(|task| t(task.label())).collect();
                let after = if serves.is_empty() { String::new() } else { tf!("它的模型现在用于{}，删除后这些任务改用默认的模型。", "Its models currently serve {}. These tasks will use the default model after removal.", serves.join("、")) };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("删除服务「{}」？", "Delete provider “{}”?", provider.title()), message: tf!("它的地址、API Key 和 {} 个模型会从 settings.json 里删除。{after}", "Its URL, API key and {} models will be removed from settings.json. {after}", provider.models.len()), action: t("删除").into(), cmd: Cmd::RemoveProvider(index) });
            }
            Cmd::RemoveProvider(index) => {
                self.run(Cmd::CloseModal, window, cx);
                self.remove_provider(index, cx);
            }
            Cmd::AddModel => self.add_model(cx),
            Cmd::RemoveModel(index) => self.remove_model(index, cx),
            Cmd::SetEffort(index, value) => self.set_effort(index, value, cx),
            Cmd::SetModelKind(index, kind) => {
                let Some(provider) = self.settings_view.as_ref().map(|view| view.provider()) else { return };
                self.settings.set_model_kind(provider, index, &kind);
                self.save_settings(cx);
            }
            Cmd::SetModelInterface(index, value) => {
                let Some(provider) = self.settings_view.as_ref().map(|view| view.provider()) else { return };
                let Some(model) = self.settings.providers.get_mut(provider).and_then(|p| p.models.get_mut(index)) else { return };
                model.interface = value;
                self.save_settings(cx);
            }
            Cmd::SetTaskModel(task, id) => {
                self.settings.tasks.set(task, &id);
                self.save_settings(cx);
            }
            Cmd::SetScreenWorkers(count) => {
                self.settings.screen_workers = count.clamp(1, wisp_core::settings::MAX_SCREEN_WORKERS);
                self.save_settings(cx);
            }
            Cmd::SetTranslateWorkers(count) => {
                self.settings.translate_workers = count.clamp(1, wisp_core::settings::MAX_SCREEN_WORKERS);
                self.save_settings(cx);
            }
            Cmd::ResetTranslationPrompt => self.reset_translation_prompt(cx),
            Cmd::AddCommand => self.add_command(cx),
            Cmd::Custom(what) => self.run_custom(what, window, cx),
            Cmd::SelectCommand(index) => self.select_command(index, cx),
            Cmd::AskRemoveCommand(index) => {
                let Some(command) = self.settings.commands.get(index) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("删除命令 /{}？", "Delete command /{}?", command.name), message: t("它的模板会从 settings.json 里删除。用过它的对话不受影响：对话里保存着当时发出的内容。").into(), action: t("删除").into(), cmd: Cmd::RemoveCommand(index) });
            }
            Cmd::RemoveCommand(index) => {
                self.run(Cmd::CloseModal, window, cx);
                self.remove_command(index, cx);
            }

            Cmd::AddById => {
                let input = self.modal_input("DOI、PMID 或它们的链接；多个时一行一个，或用空格分开", "", true, window, cx);
                // Enter looks them up, as when the field held one line; a pasted list brings its own line breaks.
                input.update(cx, |input, _| input.submit_on_enter = true);
                let submit = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| if *event == InputEvent::Submit { this.run(Cmd::SubmitModal, window, cx) });
                self.modal_subscriptions.push(submit);
                input.read(cx).focus(window);
                self.modal_error.clear();
                self.modal = Some(Modal::AddById { input, run: None });
            }
            Cmd::AddManual => self.paper_dialog(None, window, cx),
            Cmd::EditPaper(id) => self.paper_dialog(Some(id), window, cx),
            Cmd::ImportReferences => self.pick_files(t("导入"), false, cx),
            Cmd::ImportZotero => {
                if self.lib.is_none() { return self.fail(t("请先打开一个文献库。"), cx) }
                self.modal_error.clear();
                self.modal = Some(if self.lib.as_ref().and_then(|l| l.zotero.as_ref()).is_some_and(|r| r.running) { Modal::ZoteroReport } else { Modal::Zotero { open: zotero::default_database().is_some_and(|path| zotero::in_use(&path)) } });
            }
            Cmd::PickZotero => self.pick_zotero(cx),
            Cmd::ShowZotero => {
                self.modal_error.clear();
                self.modal = Some(Modal::ZoteroReport);
            }
            Cmd::ImportPdfs => self.pick_files(t("导入"), true, cx),
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
                    lib.filters = Filters { fulltext: "textless".into(), sort: lib.info.sort.clone(), ..Default::default() };
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
                // What 提取 last wrote is on show, with the way back, until the page is left.
                if let Some(lib) = self.lib.as_mut().filter(|l| l.page == Page::Extract && page != Page::Extract) { lib.extract.last = None }
                // The same for what 确认 last wrote: taken back later, it would undo decisions made since.
                if let Some(lib) = self.lib.as_mut().filter(|l| l.page == Page::Screen && page != Page::Screen) { lib.screen.confirmed = None }
                if page == Page::Write {
                    self.write_ensure(cx);
                    self.agent_attend(cx);
                    self.agent_versions(cx);
                }
                if let Some(lib) = self.lib.as_mut() {
                    if page == Page::Write { lib.enter_write() } else { lib.page = page }
                    lib.reading = false;
                    if page == Page::Ask { lib.library_chat.input.read(cx).focus(window) }
                    if page == Page::Write {
                        lib.writing.editor.read(cx).focus(window);
                        lib.writing.structures = lib.store.structures().unwrap_or_default();
                        self.write_texts_changed(cx);
                    }
                }
            }
            Cmd::WriteSection(_) | Cmd::WriteManuscript(_) | Cmd::NewManuscript | Cmd::AskRemoveManuscript(_) | Cmd::RemoveManuscript(_) | Cmd::WriteLanguage(_) | Cmd::EditSection(_) | Cmd::PickSectionKind(_) | Cmd::MoveSection(..) | Cmd::AskRemoveSection(_) | Cmd::RemoveSection(_) | Cmd::SplitSection(_) | Cmd::MergeSection(_) | Cmd::WriteReading(_) | Cmd::LeaveWrite | Cmd::ExportManuscript(_) | Cmd::WriteHistory | Cmd::HistoryPick(_) => self.run_manuscript(cmd, window, cx),
            Cmd::FigureTag(_) | Cmd::OpenFigure(_) | Cmd::FigureSource(_) | Cmd::AskFigure(_) | Cmd::CopyFigure(_) | Cmd::SaveFigure(_) | Cmd::AskRemoveFigure(_) | Cmd::RemoveFigure(_) | Cmd::FigureAddTag(_) | Cmd::FigureInfer(_) | Cmd::DrawFigure | Cmd::FigureDraw | Cmd::FigureShape(_) => self.run_figures(cmd, window, cx),
            Cmd::WriteCiteFrom(id, from) => {
                self.cite_paper(&id, Some(from), cx);
                if let Some(lib) = self.lib.as_ref() { lib.writing.editor.read(cx).focus(window) }
            }
            Cmd::Source(what) => self.run_source(what, cx),
            Cmd::WriteCite(id) => {
                self.cite_paper(&id, None, cx);
                if let Some(lib) = self.lib.as_ref() { lib.writing.editor.read(cx).focus(window) }
            }
            Cmd::WriteCiteScope(whole) => {
                if let Some(lib) = self.lib.as_mut() { lib.citing.whole = whole }
            }
            Cmd::WriteCiteFix(pieces) => self.cite_fix(pieces, cx),
            Cmd::CiteStyle(style) => {
                self.settings.cite_style = style;
                self.save_settings(cx);
                self.say(t("导出 Word 时按这种格式写引用，文末列出参考文献；稿件里仍是引用键"), cx);
            }
            Cmd::AddCiteStyle => self.add_cite_style(cx),
            Cmd::WriteCopyReferences => self.copy_references(cx),
            Cmd::WriteSide(side) => {
                // The page on show asked for again folds the panel under a narrow manuscript, or opens it; another page opens it.
                if let Some(lib) = self.lib.as_mut() {
                    lib.writing.side_open = lib.writing.side != side || !lib.writing.side_open;
                    lib.writing.side = side;
                }
            }
            Cmd::WriteReferences => {
                // Beside the conversation the list is a column of its own: the row opens and closes it.
                let agent = self.settings.agent_writing;
                if let Some(lib) = self.lib.as_mut() && agent && !lib.writing.reading && lib.writing.side == Side::References && lib.citing.whole {
                    lib.writing.side = Side::Candidates;
                } else if let Some(lib) = self.lib.as_mut() {
                    (lib.writing.side, lib.writing.side_open, lib.citing.whole) = (Side::References, true, true);
                    // Read through, the manuscript ends in its references: there is no panel beside it.
                    if lib.writing.reading { lib.citing.read_scroll.scroll_to_bottom() }
                }
            }
            Cmd::WriteExportCited => self.export_cited(cx),
            Cmd::WriteRun(reading) => self.write_run_start(reading, None, cx),
            Cmd::WriteFind => self.write_find(false, cx),
            Cmd::CitationCards(on) => {
                self.settings.citation_cards_hidden = !on;
                self.save_settings(cx);
            }
            Cmd::WriteOnly(tag, starred) => self.write_narrow(tag, starred, cx),
            Cmd::WriteUnfold(card) => {
                if let Some(unfolded) = self.lib.as_mut().map(|lib| &mut lib.writing.search.unfolded) && !unfolded.remove(&card) { unfolded.insert(card); }
            }
            Cmd::Comment(what) => self.run_comment(what, window, cx),
            Cmd::WriteAssist(assist) => self.assist_start(assist, cx),
            Cmd::WriteAssistStop => self.assist_stop(cx),
            Cmd::WriteAssistUse(how) => self.assist_use(how, window, cx),
            Cmd::WriteCited(what) => self.cite_do(what, window, cx),
            Cmd::Revise(what) => self.run_revise(what, cx),
            Cmd::Seek(what) => self.run_seek(what, window, cx),
            Cmd::Agent(what) => self.run_agent(what, window, cx),
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
                let Some(lib) = self.lib.as_mut() else { return };
                lib.filters.sort = sort.clone();
                lib.refilter();
                lib.info.sort = sort;
                if let Err(err) = libraries::save(&lib.info) { self.fail(format!("{err:#}"), cx) }
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
                    self.say(t("已移到回收站。"), cx)
                }
            }
            Cmd::RestorePaper(id) => {
                if self.change(cx, |lib| lib.store.restore_paper(&id)) { self.say(t("已恢复。"), cx) }
            }
            Cmd::AskDeletePaper(id) => {
                let title = self.lib.as_ref().and_then(|l| l.trash.iter().find(|t| t.paper.id == id)).map(|t| t.paper.title.clone()).unwrap_or_default();
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: t("彻底删除这篇文献？").into(), message: tf!("「{title}」连同它的笔记、词条、标注、PDF 和图片，会从磁盘上完全删除，无法恢复。删除之后它不再阻止重复导入。", "“{title}”, with its notes, fields, annotations, PDFs and pictures, will be deleted from disk completely and cannot be recovered. It will no longer block re-import."), action: t("彻底删除").into(), cmd: Cmd::DeletePaper(id) });
            }
            Cmd::DeletePaper(id) => {
                self.modal = None;
                self.change(cx, |lib| lib.store.purge_trashed(&id));
            }
            Cmd::RestoreAll => {
                let ids: Vec<String> = self.lib.as_ref().map(|lib| lib.trash.iter().map(|entry| entry.paper.id.clone()).collect()).unwrap_or_default();
                let done = self.change(cx, |lib| {
                    for id in &ids { lib.store.restore_paper(id)? }
                    Ok(())
                });
                if done { self.say(tf!("已恢复 {} 篇。", "Restored {} papers.", ids.len()), cx) }
            }
            Cmd::AskEmptyTrash => {
                if self.lib.as_ref().is_none_or(|lib| lib.trash.is_empty()) { return }
                self.modal_subscriptions.clear();
                self.modal_error.clear();
                let phrase = self.modal_input("", "", false, window, cx);
                // The confirm button lights up as the phrase is typed.
                let typed = cx.subscribe_in(&phrase, window, |_, _, event: &InputEvent, _, cx| if *event == InputEvent::Changed { cx.notify() });
                self.modal_subscriptions.push(typed);
                phrase.read(cx).focus(window);
                self.modal = Some(Modal::EmptyTrash { phrase });
            }
            Cmd::EmptyTrash => {
                self.modal = None;
                self.modal_subscriptions.clear();
                let mut count = 0;
                if self.change(cx, |lib| lib.store.empty_trash().map(|n| count = n)) { self.say(tf!("已彻底删除 {count} 篇。", "Deleted {count} papers permanently."), cx) }
                window.focus(&self.focus);
            }
            Cmd::OpenLink(id) => {
                if let Some(link) = self.lib.as_ref().and_then(|l| l.paper(&id)).map(|p| p.link()).filter(|l| !l.is_empty()) {
                    cx.open_url(&link);
                }
            }
            Cmd::AttachPdf(id) => self.pick_attachment(id, None, cx),
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
                    // Another PDF of the paper, if it has one, takes the viewer.
                    self.ensure_pdf(cx);
                    self.say(t("附件已移除（文件移到了 removed 文件夹）。"), cx);
                }
            }
            Cmd::RereadFile(id) => self.reread_file(id, cx),
            Cmd::ReadPages(id) => self.write_run_start(true, Some(id), cx),
            Cmd::SetTab(tab) => {
                // Without a model the question box is not drawn: the keyboard stays with the window, so Esc is still heard.
                let asking = !self.settings.profile_for_or_default(Task::Chat).key().is_empty();
                if let Some(lib) = self.lib.as_mut() {
                    if tab == Tab::Ask && lib.tab != Tab::Ask { lib.ask_from = lib.tab }
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
                // Asking about a paper put the sidebar away for the while: wanting it back is leaving that.
                if self.asking(window) {
                    self.run(Cmd::LeaveAsk, window, cx);
                    if !self.settings.sidebar_hidden { return }
                }
                self.settings.sidebar_hidden = !self.settings.sidebar_hidden;
                self.save_settings(cx);
            }
            Cmd::LeaveAsk => {
                if let Some(lib) = self.lib.as_mut().filter(|lib| lib.tab == Tab::Ask) { lib.tab = lib.ask_from }
                self.ensure_pdf(cx);
                window.focus(&self.focus);
            }
            Cmd::PdfCut => {
                if let Some(pdf) = self.lib.as_ref().and_then(|lib| lib.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.toggle_cutting(cx)) }
            }
            Cmd::PdfTool(tool) => {
                if let Some(pdf) = self.lib.as_ref().and_then(|lib| lib.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.set_tool(tool, cx)) }
            }
            Cmd::ReadFile(file) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let Some(paper) = lib.files.iter().find(|f| f.id == file).map(|f| f.paper_id.clone()) else { return };
                if lib.selected.as_deref() != Some(paper.as_str()) { self.select(Some(paper.clone()), window, cx) }
                let Some(lib) = self.lib.as_mut() else { return };
                // Read where there is room: beside the paper's conversation when it is open, else on the PDF tab.
                if !(lib.tab == Tab::Ask && lib.split) { lib.tab = Tab::Pdf }
                let main = lib.main_pdf(&paper).map(|f| f.id.clone());
                self.show_file(&paper, (main.as_deref() != Some(file.as_str())).then_some(file), cx);
                cx.notify();
            }
            Cmd::AttachAs(paper, kind) => self.pick_attachment(paper, Some(kind), cx),
            Cmd::PdfSplit(split) => {
                // It is for this paper: the next one opens whole.
                if let Some(pdf) = self.lib.as_ref().and_then(|lib| lib.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.set_split(split, cx)) }
            }
            Cmd::BesidePdf(show) => {
                // Beside the conversation the window is laid out for the two (`asking`); put away, the library is back around it.
                if let Some(lib) = self.lib.as_mut() { lib.split = show }
                self.ensure_pdf(cx);
            }
            Cmd::SwapPanes => {
                // Only the order is drawn anew: the PDF, the conversation and the question being typed stay as they are.
                self.settings.pdf_first = !self.settings.pdf_first;
                self.save_settings(cx);
            }
            Cmd::FloatPdfTools(on) => {
                // Only where the tools are drawn changes; a tool in use stays in use.
                self.settings.pdf_tools_floating = on;
                self.save_settings(cx);
            }
            Cmd::FloatChat(on) => self.float_chat(on, window, cx),
            Cmd::FoldChat(fold) => self.fold_chat(fold, window, cx),
            Cmd::PdfZoom(to) => {
                if let Some(lib) = self.lib.as_mut().filter(|lib| lib.tab == Tab::Pdf || lib.split) && let Some(pdf) = lib.pdf.clone() {
                    // A fit chosen is the one the next paper opens at; a percentage stays with this paper.
                    lib.zoom = pdf.update(cx, |pdf, cx| {
                        pdf.zoom_to(to, cx);
                        pdf.zoom()
                    })
                    .carried(lib.zoom);
                }
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

            Cmd::Extract(what) => self.run_extract(what, window, cx),
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
            Cmd::ToggleColumn(key) => {
                let hidden = &mut self.settings.columns_hidden;
                match hidden.iter().position(|k| *k == key) {
                    Some(i) => drop(hidden.remove(i)),
                    None => hidden.push(key),
                }
                self.save_settings(cx);
            }
            Cmd::ResetColumnWidths => {
                self.settings.column_widths.clear();
                self.save_settings(cx);
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
                    lib.chat(paper).input.update(cx, |input, cx| input.set_text(&question, cx));
                }
                self.ask(paper, cx);
            }
            Cmd::StartCommand(name) => {
                if let Some(lib) = self.lib.as_ref() {
                    lib.library_chat.input.update(cx, |input, cx| input.set_text(&format!("/{name} "), cx));
                    lib.library_chat.input.read(cx).focus(window);
                }
                self.chat_input(Talk::Library, InputEvent::Changed, cx);
            }
            Cmd::NewChat(paper) => {
                // An answer still arriving is stopped first, so that what there is of it is kept.
                self.stop_chat(paper, cx);
                // A conversation whose last save failed is tried once more; if that fails too, it says so itself.
                if self.lib.as_ref().is_some_and(|lib| !lib.chat(paper).kept) { self.save_chat(paper, cx) }
                let mut kept = false;
                if let Some(lib) = self.lib.as_mut() {
                    let chat = chat_of!(lib, paper);
                    kept = chat.kept;
                    chat.reset(cx);
                    chat.input.read(cx).focus(window);
                    // A new conversation with the library is about the list on show again.
                    if paper == Talk::Library { lib.scope_reset(cx) }
                }
                if kept { self.say(t("上一段对话已保存，在「历史」里可以找到。"), cx) }
            }
            Cmd::StopChat(paper) => self.stop_chat(paper, cx),
            Cmd::ToggleHistory(paper) => {
                if let Some(lib) = self.lib.as_mut() {
                    let about = lib.talk_about(paper).unwrap_or_default();
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
                let warning = if reviewed { t("\n\n这次对话的综述记录（写作要求、大纲、正文和证据）会一起删除；已经存为稿件的内容不受影响。") } else { "" };
                self.modal = Some(Modal::Confirm { title: t("删除这次对话？").into(), message: tf!("「{title}」连同其中的图片会从这个文献库的历史对话里删除，不能恢复。文献库里的文献不受影响。{warning}", "“{title}” and its images will be permanently removed from this library's chat history. Papers are kept.{warning}"), action: t("删除").into(), cmd: Cmd::RemoveChat(paper, id) });
            }
            Cmd::RemoveChat(paper, id) => {
                if let Some(lib) = self.lib.as_mut() {
                    let about = lib.talk_about(paper).unwrap_or_default();
                    let result = lib.store.remove_conversation(&id).and_then(|()| lib.store.list_conversations(&about));
                    // The conversation in hand, once removed, would only be saved again with the next answer;
                    // the papers it was kept to go with it, as for any new conversation.
                    let in_hand = lib.chat(paper).id == id;
                    if in_hand && paper == Talk::Library { lib.scope_reset(cx) }
                    let chat = lib.chat_mut(paper);
                    if in_hand {
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
                let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(t("添加图片").into()), extensions: Some(extensions) });
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
            Cmd::InsertInto(section, text) => self.insert_into_section(section, text, cx),
            Cmd::StartReview => self.start_review(cx),
            Cmd::ReviewToManuscript => self.review_to_manuscript(window, cx),
            Cmd::ToggleReview => {
                if let Some(lib) = self.lib.as_mut() { lib.library_chat.review_open = !lib.library_chat.review_open && lib.library_chat.review.is_some() }
            }
            Cmd::Review(what, text) => self.review_do(what, text, cx),
            Cmd::EditReviewText(what) => self.edit_review_text(what, window, cx),
            Cmd::AskRemoveReviewSection(name) => {
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("删除综述记录中的「{name}」？", "Delete “{name}” from the review record?"), message: t("这一节会从综述记录中移除，它的引用带来的、你还没有判断过的证据也一起移除。已创建的稿件不受影响。").into(), action: t("删除").into(), cmd: Cmd::RemoveReviewSection(name) });
            }
            Cmd::RemoveReviewSection(name) => {
                self.review_apply(wisp_core::review::ReviewAction::RemoveSection { name }, t("这一节已从综述记录中移除"), cx);
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
                self.modal = Some(Modal::Confirm { title: t("删除这条证据记录？").into(), message: t("这条论断与来源的关联会从综述记录中移除，文献和正文不受影响。").into(), action: t("删除").into(), cmd: Cmd::RemoveReviewEvidence(id) });
            }
            Cmd::RemoveReviewEvidence(id) => {
                self.review_apply(wisp_core::review::ReviewAction::RemoveEvidence { id }, t("证据记录已删除"), cx);
                self.run(Cmd::CloseModal, window, cx);
            }
            Cmd::CopyText(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.say(t("已复制"), cx);
            }
            Cmd::Retract(talk, at) => self.retract(talk, at, window, cx),
            Cmd::Regenerate(talk, at) => self.regenerate(talk, at, cx),
            Cmd::Fork(talk, at) => self.fork_chat(talk, at, cx),
            Cmd::Unretract(talk) => self.unretract(talk, cx),
            Cmd::CopySelected => {
                if let Some(text) = self.picked.as_ref().map(|picked| picked.chosen().to_string()).filter(|text| !text.is_empty()) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.say(t("已复制"), cx);
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
                self.modal = Some(Modal::Confirm { title: tf!("去掉标签「{tag}」？", "Remove tag “{tag}”?"), message: tf!("它会从 {count} 篇文献上去掉。文献本身不动。", "It will be removed from {count} papers. The papers are kept."), action: t("去掉标签").into(), cmd: Cmd::RemoveTag(tag) });
            }
            Cmd::RemoveTag(tag) => {
                self.modal = None;
                if self.retag(&tag, None, cx) { self.say(t("标签已去掉。"), cx) }
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
                if matches!(pointer, Pointer::Down | Pointer::Word) {
                    window.focus(&self.focus);
                    self.picked = None;
                }
                // One selection at a time: starting one in a conversation ends the other's.
                if matches!(pointer, Pointer::Down | Pointer::Word) {
                    for other in [Talk::Library, Talk::Paper, Talk::Write].into_iter().filter(|other| *other != paper) { lib.chat_mut(other).picked = None }
                }
                let chat = lib.chat_mut(paper);
                match (pointer, chat.picked.as_mut()) {
                    (Pointer::Down, _) => (chat.picked, chat.spot) = (Some((at, place, place, true)), None),
                    // By the rule of every text read (`select::word_at`). Done at once and not held: the hand's slip
                    // between the press and the release would else be a drag that cuts the word short.
                    (Pointer::Word, _) => {
                        let Some(ChatItem::Answer(answer)) = chat.messages.get(at) else { return };
                        let word = crate::markdown::texts(answer).get(place.0).map(|piece| crate::select::word_at(piece, place.1)).filter(|word| !word.is_empty() && crate::markdown::text_between(answer, (place.0, word.start), (place.0, word.end)).trim() != "");
                        (chat.picked, chat.spot) = (word.map(|word| (at, (place.0, word.start), (place.0, word.end), false)), None);
                    }
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
                    self.say(t("已复制"), cx);
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
                if let Some(lib) = self.lib.as_mut() {
                    lib.library_chat.materials_open = !lib.library_chat.materials_open;
                    // The panel is a place to search: its box has the keyboard while it is open, the question's after.
                    if lib.library_chat.materials_open { lib.scope_find.read(cx).focus(window) } else { lib.library_chat.input.read(cx).focus(window) }
                }
            }
            Cmd::Scope(what) => self.scope(what, cx),
            Cmd::ToggleSources(paper, at) => {
                if let Some(lib) = self.lib.as_mut() {
                    let chat = lib.chat_mut(paper);
                    if !chat.sources_open.remove(&at) { chat.sources_open.insert(at); }
                }
            }
            Cmd::GotoCited(paper, label, file, page, quote) => {
                let Some(lib) = self.lib.as_ref() else { return };
                let chat = lib.chat(paper);
                let Some(id) = chat.sources().iter().find(|source| source.label == label).map(|source| source.paper_id.clone()) else { return self.fail(tf!("这次对话里没有 {label} 这篇文献。", "Paper {label} is not in this conversation."), cx) };
                // Where the sentence was found, when it is not where the answer says.
                let page = match chat.checks.get(&(label, file, page, quote.clone())) {
                    Some(ai::Quoted::Elsewhere(at) | ai::Quoted::Begins(at, _)) => Some(*at),
                    _ => page,
                };
                let file = file.and_then(|n| lib.attachments(&id).get(n - 1).map(|f| f.id.clone()));
                self.run(Cmd::GotoFile(id, file, page, quote), window, cx);
            }
            Cmd::GotoSource(id, page, quote) => self.run(Cmd::GotoFile(id, None, page, quote), window, cx),
            Cmd::GotoFile(id, file, page, quote) => {
                // The file cited is the one shown: the main text, unless an attachment is named.
                if page.is_some() { self.show_file(&id, file, cx) }
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
                        pdf.update(cx, |pdf, cx| pdf.find(page, &quote, cx));
                    }
                }
            }

            Cmd::NewSearch => self.search_dialog(None, window, cx),
            Cmd::EditSearch(id) => self.search_dialog(Some(id), window, cx),
            Cmd::AskRemoveSearch(id) => {
                let Some(search) = self.lib.as_ref().and_then(|l| l.searches.iter().find(|s| s.id == id)) else { return };
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("删除检索式「{}」？", "Delete search “{}”?", search.name), message: t("它导入过的文献和运行记录都会保留，只是不再运行这个检索式。").into(), action: t("删除").into(), cmd: Cmd::RemoveSearch(id) });
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
                        Ok(run) if run["stopped"].is_string() => this.fail(tf!("{}：新增 {} 篇。{}", "{}: {} added. {}", run["search"].as_str().unwrap_or(t("检索")), run["added"], run["stopped"].as_str().unwrap_or_default()), cx),
                        Ok(run) => this.say(tf!("{}：新增 {} 篇，跳过已有的 {} 篇", "{}: {} added, {} existing skipped", run["search"].as_str().unwrap_or(t("检索")), run["added"], run["skipped"]), cx),
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
                    title: t("撤销这次导入？").into(),
                    message: tf!("{}\n\n这次导入新增的 {} 篇文献里，还没有处理过的会移出文献库，记录另存在文献库文件夹的 removed 里，可以再导入；已经做过收录判断、读过、收藏、写过笔记、加过标签或附件、提过问、被稿件引用的会保留。", "{}\n\nOf {} papers added by this import, untouched papers leave the library; their records are set aside in the library folder under removed and can be imported again. Papers already screened, read, starred, annotated, tagged, given attachments, asked about or cited in a manuscript are kept.", batch.label, batch.added),
                    action: t("撤销导入").into(),
                    cmd: Cmd::UndoImport(id),
                });
            }
            Cmd::UndoImport(id) => {
                let mut undone = None;
                if self.change(cx, |lib| lib.store.undo_batch(&id).map(|done| undone = Some(done)))
                    && let Some(done) = undone
                {
                    let aside = if done.set_aside.is_empty() { String::new() } else { tf!("，记录另存在 {}", "; records set aside in {}", done.set_aside) };
                    self.say(if done.kept.is_empty() { tf!("已撤销：移出了 {} 篇{aside}", "Undone: {} papers removed{aside}", done.removed) } else { tf!("已撤销：移出了 {} 篇{aside}；保留了处理过的 {} 篇", "Undone: {} removed{aside}; {} edited papers kept", done.removed, done.kept.len()) }, cx);
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
            Cmd::ScreenStart => self.screen_start("pending", None, cx),
            Cmd::ScreenRecheck(included) => self.screen_start(if included { "included" } else { "decided" }, None, cx),
            Cmd::AskTrashUnincluded => {
                let Some(lib) = self.lib.as_ref() else { return };
                let ids = lib.unincluded();
                if ids.is_empty() { return }
                let counts = wisp_core::screening::tally(ids.iter().filter_map(|id| lib.paper(id)).map(|p| p.screening.as_str()));
                let each: Vec<String> = SCREENINGS.iter().zip(counts).filter(|(_, n)| *n > 0).map(|((key, _), n)| format!("{} {n}", crate::i18n::label(&SCREENINGS, key))).collect();
                self.modal_error.clear();
                self.modal = Some(Modal::Confirm { title: tf!("把未纳入的 {} 篇移到回收站？", "Move {} papers not included to trash?", ids.len()), message: tf!("其中{}。它们的笔记、词条和 PDF 都留着，可以在回收站里恢复。", "Of them: {}. Their notes, fields and PDFs are kept, and they can be restored from the trash.", each.join(" · ")), action: t("移到回收站").into(), cmd: Cmd::TrashPapers(ids) });
            }
            Cmd::ScreenStop => {
                if let Some(lib) = self.lib.as_mut() {
                    // Answers still on their way belong to a run that is over.
                    lib.screen.run += 1;
                    lib.screen.todo.clear();
                    lib.screen.reading.clear();
                }
            }
            Cmd::ScreenFilter(filter) => {
                if let Some(lib) = self.lib.as_mut() { lib.screen.filter = filter }
            }
            Cmd::ScreenDecide(..) | Cmd::ScreenMode(_) | Cmd::ScreenGo(_) | Cmd::ScreenUndo | Cmd::ScreenAskOne(_) | Cmd::ScreenBeside => self.run_screen(cmd, window, cx),
            Cmd::ConfirmTick(_) | Cmd::ConfirmChoose(..) | Cmd::ConfirmPart(_) | Cmd::ConfirmTickAll(_) | Cmd::ConfirmSort | Cmd::ConfirmCursor(_) | Cmd::ConfirmApply | Cmd::ConfirmUndo => self.run_confirm(cmd, window, cx),

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
                    let mut search = SavedSearch { kind: entry.idea.source.clone(), name: entry.idea.why.chars().take(40).collect(), query: entry.idea.query.clone(), max_results: wisp_core::sources::broad_limit(&entry.idea.source), enabled: true, ..Default::default() };
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
                // A file not imported: the next one waiting is asked about.
                if matches!(self.modal, Some(Modal::Headings(_))) { self.headings_done(false, cx) }
                if matches!(self.modal, Some(Modal::Headings(_))) { return cx.notify() }
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
        } else if self.palette.is_some() {
            self.close_palette(window, cx);
        } else if self.modal.is_some() {
            self.run(Cmd::CloseModal, window, cx);
        } else if self.dismiss_font_picker(window, cx) {
        } else if self.settings_view.is_some() {
            self.close_settings(window, cx);
        } else if self.lib.as_ref().is_some_and(|l| l.page == Page::Write && l.writing.seek.open) {
            // The search over the manuscript, from its own field or the manuscript: before a paper beside it is closed.
            self.run(Cmd::Seek(crate::write_seek::Do::Close), window, cx);
        } else if self.picked.take().is_some() || self.lib.as_mut().is_some_and(Library::unpick) {
        } else if self.lib.as_ref().is_some_and(|l| l.materials_shown()) {
            // The panel floating over the conversation is put away first; what it searched for stays.
            if let Some(lib) = self.lib.as_mut() {
                lib.library_chat.materials_open = false;
                lib.library_chat.input.read(cx).focus(window);
            }
        } else if self.lib.as_ref().is_some_and(|l| !l.marked.is_empty()) {
            if let Some(lib) = self.lib.as_mut() { lib.marked.clear() }
        } else if self.lib.as_ref().is_some_and(|l| l.reading && l.selected.is_some()) {
            // Out of reading mode first: the paper stays open where it was read.
            if let Some(lib) = self.lib.as_mut() { lib.reading = false }
        } else if self.asking(window) {
            // Out of asking first, back to the library with the paper still open.
            self.run(Cmd::LeaveAsk, window, cx);
        } else if self.lib.as_ref().is_some_and(|l| l.selected.is_some()) {
            self.select(None, window, cx);
        } else if self.lib.as_ref().is_some_and(|l| l.page == Page::Screen && l.screen.mode != ScreenMode::Batch) {
            // Out of a round or the confirmation, back to the queue.
            self.run(Cmd::ScreenMode(ScreenMode::Batch), window, cx);
        }
        cx.notify();
    }

    /// A keyboard shortcut (shortcuts.rs) was pressed.
    pub fn shortcut(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A dialog keeps the keyboard to itself.
        if self.modal.is_some() { return }
        if id == "palette" { return self.toggle_palette(window, cx) }
        // So does the palette: its own key puts it away, the others wait.
        if self.palette.is_some() { return }
        let in_settings = self.settings_view.is_some();
        let cmd = match id {
            "settings" if !in_settings => Cmd::OpenSettings(SettingsPage::General),
            "home" => Cmd::GoHome,
            "new-library" if !in_settings && self.lib.is_none() => Cmd::NewLibrary,
            // The rest act on the open library, when it is the one in view.
            _ if in_settings || self.lib.is_none() => return,
            // On the writing page the words are looked for in the manuscript, not among the papers (a PDF with the keyboard looks in itself, pdf.rs).
            "search" if self.lib.as_ref().is_some_and(|l| l.page == Page::Write && !l.reading) => return self.seek_open(window, cx),
            "search" => {
                if let Some(lib) = self.lib.as_mut() {
                    lib.page = Page::Papers;
                    lib.reading = false;
                }
                // A window too narrow for the list beside a paper asked about: the search box is with the full list.
                if self.asking(window) && self.ask_list_width(window) <= 0. { self.run(Cmd::LeaveAsk, window, cx) }
                if let Some(lib) = self.lib.as_ref() {
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
            // On the writing page with no paper beside it, the manuscript's own reading view: 预览.
            "reading" if self.lib.as_ref().is_some_and(|l| l.page == Page::Write && !l.reading) && self.detail_width(window) <= 0. => Cmd::WriteReading(!self.lib.as_ref().is_some_and(|l| l.writing.reading)),
            "reading" if self.lib.as_ref().is_some_and(|l| l.selected.is_some()) => Cmd::ToggleReading,
            // The PDF's size, while a PDF is on show.
            "zoom-in" => Cmd::PdfZoom(ZoomTo::In),
            "zoom-out" => Cmd::PdfZoom(ZoomTo::Out),
            "zoom-fit" => Cmd::PdfZoom(ZoomTo::Set(Zoom::FitWidth)),
            // The candidates stand beside the manuscript the person writes; beside the conversation that writes there are none to show.
            "write-find" if !self.settings.agent_writing && self.lib.as_ref().is_some_and(|l| l.page == Page::Write) => Cmd::WriteFind,
            // What is selected in an answer: the paper's conversation first, when both have something.
            "copy-picked" if self.picked.is_some() => Cmd::CopySelected,
            "copy-picked" => match self.lib.as_ref().and_then(|lib| [Talk::Paper, Talk::Library, Talk::Write].into_iter().find(|talk| lib.chat(*talk).picked.is_some())) {
                Some(talk) => Cmd::CopyPicked(talk),
                None => return,
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
        let scope_find = cx.new(|cx| TextInput::new("在这些文献里找：题目、作者、标签、摘要…", cx));
        let subscriptions = vec![
            // The panel over the conversation searches the papers in scope as it is typed into.
            cx.subscribe(&scope_find, |this, input, event: &InputEvent, cx| {
                if *event == InputEvent::Changed { this.scope_typed(input.read(cx).text().to_string(), cx) }
            }),
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
        let library_chat = Chat::new("交代一件事或提一个问题，输入 / 选择命令，可以粘贴图片", cx);
        let paper_chat = Chat::new("就这篇文献提问，输入 / 选择命令", cx);
        let write_chat = Chat::new("告诉助手写什么、改什么", cx);
        let mut subscriptions = subscriptions;
        for (chat, talk) in [(&library_chat, Talk::Library), (&paper_chat, Talk::Paper), (&write_chat, Talk::Write)] {
            subscriptions.push(cx.subscribe(&chat.input, move |this, _, event: &InputEvent, cx| this.chat_input(talk, *event, cx)));
        }
        let guide = Guide::new(&info.topic, cx);
        let screen = Screen::new(&info.criteria, cx);
        // The goal is the library's, kept with it as it is typed; suggestions made against another one stop counting.
        subscriptions.push(cx.subscribe(&screen.criteria, |this, input, event: &InputEvent, cx| {
            if *event != InputEvent::Changed { return }
            let goal = input.read(cx).text().trim().to_string();
            let Some(lib) = this.lib.as_mut().filter(|l| l.info.criteria != goal) else { return };
            lib.info.criteria = goal;
            if let Err(err) = libraries::save(&lib.info) { this.fail(tf!("筛选目标没有保存：{err:#}", "Screening criteria not saved: {err:#}"), cx) }
        }));
        let manuscripts = store.manuscripts().unwrap_or_default();
        let (manuscript, section_id) = Writing::latest(&manuscripts);
        let opened = manuscripts.iter().find(|m| m.id == manuscript);
        let draft = opened.and_then(|m| m.section(&section_id)).map(|s| s.body.clone()).unwrap_or_default();
        let title = cx.new(|cx| TextInput::new(wisp_core::manuscript::UNTITLED, cx).with_text(&opened.map(|m| m.title.clone()).unwrap_or_default()));
        subscriptions.push(cx.subscribe(&title, |this, input, event: &InputEvent, cx| {
            if *event == InputEvent::Changed { this.write_title_typed(input.read(cx).text().to_string(), cx) }
        }));
        let editor = cx.new(|cx| TextInput::new(crate::write::PLACEHOLDER, cx).multiline(14).markdown().with_text(&draft));
        let find = cx.new(|cx| TextInput::new("找句子：输入词或短语，例如 remains unclear", cx));
        subscriptions.push(cx.subscribe(&editor, |this, input, event: &InputEvent, cx| {
            if let InputEvent::MarkClicked(n) = *event { return this.comment_mark_clicked(n, cx) }
            if *event != InputEvent::Changed { return }
            let text = input.read(cx).text().to_string();
            this.write_typed(text, cx);
        }));
        // A citation pointed at says what it rests on; ⌘ or Ctrl with a click opens that sentence in its paper (write_source.rs).
        subscriptions.push(cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, window, cx| match *event {
            InputEvent::LinkHovered => cx.notify(),
            InputEvent::LinkClicked(n, at) => this.citation_follow(n, at, window, cx),
            _ => {}
        }));
        // Candidates are asked for: the find box on Enter, the manuscript by the shortcut.
        subscriptions.push(cx.subscribe(&find, |this, _, event: &InputEvent, cx| if *event == InputEvent::Submit { this.write_find(true, cx) }));
        let (comments, heard) = crate::comments::Commenting::new(store.comments(&manuscript).unwrap_or_default(), store.manuscript_source(&manuscript).ok().flatten(), cx);
        subscriptions.extend(heard);
        let revise = crate::revise::Revising::new(&mut subscriptions, cx);
        let seek = crate::write_seek::Seek::new(&mut subscriptions, cx);
        let writing = Writing { manuscripts, manuscript, section_id, reading: false, title, typed: 0, editor, find, search: Default::default(), structures: BTreeMap::new(), sorting: Sorting::default(), scroll: ScrollHandle::new(), assist: Default::default(), revise, comments, editor_scroll: ScrollHandle::new(), side: Side::default(), side_open: false, agent: Default::default(), seek };
        let citing = crate::cite::Citing::new(&store, &writing.manuscript, &mut subscriptions, cx);
        let gallery = crate::figures::Gallery::new(&mut subscriptions, cx);
        let filters = Filters { sort: info.sort.clone(), ..Default::default() };
        let mut lib = Library {
            info,
            store,
            papers: vec![],
            fields: vec![],
            files: vec![],
            trash: vec![],
            rounds: vec![],
            filters,
            shown: vec![],
            search,
            page: Page::Papers,
            selected: None,
            tab: Tab::Info,
            wide: false,
            reading: false,
            split: true,
            ask_from: Tab::Info,
            write_from: Page::Papers,
            ask_drawn: false,
            float_drag: None,
            float_area: (0., 0.),
            notes,
            pdf: None,
            pdf_events: None,
            zoom: Zoom::FitWidth,
            library_chat,
            paper_chat,
            write_chat,
            marked: vec![],
            anchor: None,
            cursor: None,
            asked: vec![],
            scope_find,
            scope_words: String::new(),
            scope_hits: None,
            table_scroll: UniformListScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            extracting: HashSet::new(),
            read_file: HashMap::new(),
            file_places: HashMap::new(),
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
            note_links: BTreeMap::new(),
            writing,
            citing,
            gallery,
            extract: crate::extract::Extract::new(),
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
        self.stop_chat(Talk::Paper, cx);
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
                (lib.tab, lib.ask_from) = (Tab::Info, Tab::Info);
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
        let chosen = profile.as_deref().and_then(|id| self.settings.profile(id));
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
        let models: Vec<(String, String)> = self.settings.profiles().into_iter().filter(|p| !p.model.trim().is_empty()).map(|p| (p.id.clone(), p.labelled())).collect();
        let usual = self.settings.profile_for_or_default(Task::Translate);
        let Some(lib) = self.lib.as_ref() else { return };
        let Some(pdf) = lib.pdf.clone() else { return };
        let Some((text, profile)) = pdf.read(cx).translation_asked() else { return };
        let state = lib.translations.get(&wisp_core::store::translation_key(&text, &prompt)).cloned().unwrap_or(Translated::Busy);
        let model = profile.and_then(|id| self.settings.profile(&id)).unwrap_or(usual);
        let model = (model.id.clone(), model.labelled());
        pdf.update(cx, |pdf, cx| pdf.show_translation(state, model, models, cx));
    }

    /// Open the PDF viewer when its tab is showing and the paper has a PDF.
    fn ensure_pdf(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        if !(lib.tab == Tab::Pdf || (lib.tab == Tab::Ask && lib.split)) || lib.pdf.is_some() { return }
        let Some(file) = lib.selected.as_deref().and_then(|id| lib.pdf_file(id)).cloned() else { return };
        let path = lib.store.file_path(&file);
        let annotations = match lib.store.pdf_annotations(&file.paper_id, &file.id) {
            Ok(annotations) => annotations,
            Err(error) => return self.fail(format!("{error:#}"), cx),
        };
        let zoom = lib.zoom;
        let place = lib.file_places.get(&file.id).copied();
        let pdf = cx.new(|cx| {
            let mut pdf = PdfView::new(&path, zoom, &file, annotations, cx);
            // A file read before, of this paper, opens where it was left.
            if let Some(place) = place { pdf.resume(place, cx) }
            pdf
        });
        lib.pdf_events = Some(cx.subscribe(&pdf, |this, _, event: &PdfEvent, cx| this.pdf_event(event, cx)));
        lib.pdf = Some(pdf);
    }

    /// Show a file of a paper in the viewer: an attachment by its id, or (`None`) the main text. The file on
    /// show keeps its place for when it is come back to, and each file has its own marks.
    fn show_file(&mut self, paper: &str, file: Option<String>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        match file {
            Some(file) => lib.read_file.insert(paper.to_string(), file),
            None => lib.read_file.remove(paper),
        };
        let wanted = lib.pdf_file(paper).map(|f| f.id.clone());
        if let Some(pdf) = lib.pdf.clone() {
            let (shown, place) = { let view = pdf.read(cx); (view.resource_id().to_string(), view.place()) };
            if Some(&shown) != wanted.as_ref() {
                lib.file_places.insert(shown, place);
                (lib.pdf, lib.pdf_events) = (None, None);
            }
        }
        self.ensure_pdf(cx);
        cx.notify();
    }

    /// What the PDF viewer reports: its selection copied, or handed to a conversation.
    fn pdf_event(&mut self, event: &PdfEvent, cx: &mut Context<Self>) {
        match event {
            PdfEvent::Copied => self.say(t("已复制"), cx),
            PdfEvent::Translate(text, profile) => self.translate_passage(text.clone(), profile.clone(), cx),
            PdfEvent::Figure(piece) => self.collect_piece(piece, cx),
            PdfEvent::Annotate { paper, resource, page, kind, quote, boxes } => self.change_pdf_annotation(resource, |store| store.add_pdf_annotation(paper, resource, *page, *kind, quote, boxes).map(|_| ()), cx),
            PdfEvent::AnnotationNote { resource, id, note } => self.change_pdf_annotation(resource, |store| store.update_pdf_annotation_note(resource, id, note), cx),
            PdfEvent::RecolorAnnotation { resource, id, color } => self.change_pdf_annotation(resource, |store| store.recolor_pdf_annotation(resource, id, color), cx),
            PdfEvent::RemoveAnnotation { resource, id } => self.change_pdf_annotation(resource, |store| store.remove_pdf_annotation(resource, id), cx),
            PdfEvent::AddShape { paper, resource, page, kind, rect, note } => self.change_pdf_annotation(resource, |store| store.add_pdf_shape(paper, resource, *page, *kind, *rect, note).map(|_| ()), cx),
            PdfEvent::MoveShape { resource, id, rect } => self.change_pdf_annotation(resource, |store| store.move_pdf_shape(resource, id, *rect), cx),
            PdfEvent::Picture(png, to_library) => {
                self.attach_images(if *to_library { Talk::Library } else { Talk::Paper }, vec![Picture::Bytes(png.clone())], cx);
                if *to_library { self.say(t("截图已加到文献库对话的输入框上方。"), cx) } else { self.show_paper_chat(cx) }
            }
            PdfEvent::Quote(page, text, to_library) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let Some(paper_id) = lib.selected.clone() else { return };
                let file_id = lib.pdf.as_ref().map(|pdf| pdf.read(cx).resource_id().to_string()).unwrap_or_default();
                let quote = ChatQuote { paper_id, file_id, page: *page, text: text.clone() };
                let chat = lib.chat_mut(if *to_library { Talk::Library } else { Talk::Paper });
                if chat.quotes.len() >= MAX_QUOTES { return self.fail(tf!("一个问题最多带 {MAX_QUOTES} 段原文。先发出去，或者去掉一段。", "A question can include at most {MAX_QUOTES} passages. Send it first or remove one."), cx) }
                if !chat.quotes.contains(&quote) { chat.quotes.push(quote) }
                chat.history = None;
                if *to_library {
                    self.say(t("已加到文献库对话的输入框上方。"), cx);
                } else {
                    self.show_paper_chat(cx);
                }
                cx.notify();
            }
        }
    }

    fn change_pdf_annotation(&mut self, resource: &str, change: impl FnOnce(&mut Store) -> anyhow::Result<()>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        // An event queued by an old viewer cannot edit the newly selected PDF.
        if lib.selected.as_deref().and_then(|id| lib.pdf_file(id)).is_none_or(|file| file.id != resource) { return }
        match change(&mut lib.store).and_then(|()| lib.store.pdf_annotations("", resource)) {
            Ok(annotations) => {
                if let Some(pdf) = lib.pdf.clone() { pdf.update(cx, |pdf, cx| pdf.annotations_saved(annotations, cx)) }
                self.say(t("PDF 标注已保存"), cx);
            }
            Err(error) => self.fail(format!("{error:#}"), cx),
        }
    }

    /// Something from the PDF went to the paper's own conversation: the
    /// conversation comes up with the PDF to its right, the window laid
    /// out for the two.
    fn show_paper_chat(&mut self, cx: &mut Context<Self>) {
        self.unfold_chat(cx);
        let Some(lib) = self.lib.as_mut() else { return };
        if !(lib.split && lib.tab == Tab::Ask) {
            if lib.tab != Tab::Ask { lib.ask_from = lib.tab }
            (lib.split, lib.tab) = (true, Tab::Ask);
            self.say(t("已加到这篇文献的提问里。"), cx);
        }
        cx.notify();
    }

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

    /// Change one paper and save it.
    pub(crate) fn edit_paper(&mut self, id: &str, cx: &mut Context<Self>, edit: impl FnOnce(&mut Paper)) -> bool {
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
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(t("选择 zotero.sqlite").into()), extensions: Some(vec!["sqlite".into()]) });
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            this.update(cx, |this, cx| {
                if this.lib.as_ref().is_none_or(|l| l.info.id != library) { return }
                match result {
                    Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                        Some(path) => this.import_zotero(path, cx),
                        None => this.modal_error = t("没有得到所选文件的路径，请重新选择 zotero.sqlite。").into(),
                    },
                    Ok(Ok(None)) => {}
                    Ok(Err(err)) => { this.modal_error = tf!("无法打开文件选择器：{err:#}", "Cannot open file picker: {err:#}"); }
                    Err(err) => { this.modal_error = tf!("文件选择器未能返回结果：{err}", "File picker returned no result: {err}"); }
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
        lib.zotero = Some(ZoteroRun { id: run_id.clone(), running: true, current: t("正在读取数据库…").into(), ..Default::default() });
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
        self.import_picked(picked, cx);
    }

    /// What a file picker gave goes to the import; a picker that failed says so instead of doing nothing.
    fn import_picked<E: std::fmt::Display + 'static>(&mut self, picked: impl Future<Output = Result<anyhow::Result<Option<Vec<PathBuf>>>, E>> + 'static, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            this.update(cx, |this, cx| match result {
                Ok(Ok(Some(paths))) => this.import_paths(paths, cx),
                Ok(Ok(None)) => {}
                Ok(Err(err)) => this.fail(tf!("无法打开文件选择器：{err:#}", "Cannot open file picker: {err:#}"), cx),
                Err(err) => this.fail(tf!("文件选择器未能返回结果：{err}", "File picker returned no result: {err}"), cx),
            }).ok();
        })
        .detach();
    }

    /// Folders picked: every PDF under them, tagged with the folders it lies in.
    fn pick_folders(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some(t("导入").into()), extensions: None });
        self.import_picked(picked, cx);
    }

    /// Files picked for a paper: as `kind` (`fulltext` or `supplement`), or, without one, the first PDF its main text
    /// when it has none yet and the rest attachments.
    fn pick_attachment(&mut self, paper_id: String, kind: Option<&'static str>, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(t("添加").into()), extensions: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| {
                    let mut added = 0;
                    this.change(cx, |lib| {
                        let mut has_fulltext = lib.files.iter().any(|f| f.paper_id == paper_id && f.kind == "fulltext");
                        for path in &paths {
                            let is_pdf = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
                            // The first PDF of a paper is its full text; the rest are supplements.
                            let kind = kind.unwrap_or(if is_pdf && !has_fulltext { "fulltext" } else { "supplement" });
                            has_fulltext |= is_pdf;
                            lib.store.import_file(&paper_id, path, kind)?;
                            added += 1;
                        }
                        Ok(())
                    });
                    if added > 0 {
                        if let Some(lib) = this.lib.as_mut() { lib.pdf = None }
                        this.ensure_pdf(cx);
                        this.extract_missing(cx);
                        this.say(tf!("已添加 {added} 个附件。", "Added {added} attachments."), cx);
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
        // A library from another Wisp Paper becomes a library of its own, wherever it is dropped.
        let (archives, paths): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|path| path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case(archive::EXTENSION)));
        for path in archives { self.import_library(path, cx) }
        if paths.is_empty() { return }
        if self.lib.is_none() { return self.fail(t("请先打开一个文献库，再拖入文件。"), cx) }
        // A Word file is a manuscript to go on with, wherever it is dropped; Markdown and plain text are, on the writing page.
        let writing = self.lib.as_ref().is_some_and(|lib| lib.page == Page::Write);
        let (drafts, paths): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|path| path.is_file() && (path.extension().is_some_and(|e| e.eq_ignore_ascii_case("docx")) || (writing && (crate::comments::is_manuscript_file(path) || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt"))))));
        if !drafts.is_empty() { self.import_manuscript_files(drafts, cx) }
        if paths.is_empty() { return }
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
            return self.fail(tf!("文件夹「{}」里没有 PDF。", "No PDFs in folder “{}”.", empty_folders.join("」「")), cx);
        }
        let mut added = vec![];
        let mut skipped = 0;
        let mut errors = vec![];
        for path in others {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let parsed = std::fs::read(&path).map_err(|e| tf!("无法读取 {name}：{e}", "Cannot read {name}: {e}")).and_then(|bytes| import::parse_references(&String::from_utf8_lossy(&bytes), &tf!("导入 · {name}", "Import · {name}")).map_err(|e| format!("{name}：{e}")));
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
            self.say(tf!("导入 {} 篇{}。", "Imported {} papers{}.", added.len(), if skipped > 0 { tf!("，跳过 {skipped} 篇已有的", ", skipped {skipped} existing") } else { String::new() }), cx);
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
        let stem = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| t("未命名文献").into());
        let recognised = found.is_some();
        let mut paper = found.unwrap_or_else(|| Paper { title: stem.clone(), ..Default::default() });
        paper.source = t("PDF 导入").into();
        paper.tags = clean_strings(paper.tags.iter().chain(tags));
        let mut row = ImportRow { name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| stem.clone()), outcome: Imported::Added, paper_id: None, unrecognised: false, textless: String::new() };
        let stored = (|| -> anyhow::Result<()> {
            let known = lib.store.existing_paper(&{
                let mut probe = paper.clone();
                probe.normalize();
                probe
            })?;
            // The same file under another name is the paper that holds it: without a DOI nothing else says so.
            let known = match known {
                Some(known) => Some(known),
                None => lib.store.paper_holding(path)?,
            };
            let target = match known {
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
            Imported::Added if row.unrecognised => tf!("已添加「{stem}」。没有识别出 DOI：可以在文献详情里「按题目补全」。", "Added “{stem}”. DOI not detected; use “Look up by title” in paper details."),
            Imported::Added => tf!("已添加：{title}", "Added: {title}"),
            Imported::Attached => tf!("「{stem}」已在文献库中，PDF 已附到原条目。", "“{stem}” is already in this library. PDF attached to existing record."),
            Imported::InTrash => tf!("「{stem}」对应的文献在回收站中，未导入。", "“{stem}” is in trash and was not imported."),
            Imported::Held => tf!("「{stem}」已在文献库中。", "“{stem}” is already in this library."),
            Imported::Failed(err) => tf!("「{stem}」没有导入：{err}", "“{stem}” not imported: {err}"),
        };
        let said = if row.textless.is_empty() { said } else { tf!("{said} 但没有读出文字（{}），全文暂时不能用于提问。", "{said} But no text was extracted ({}); full text is unavailable for chat.", t(fulltext::failure_brief(&row.textless))) };
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

    /// The identifiers pasted into 通过 DOI / PMID 添加, more than one, looked up in the background as an import of
    /// their own (a PDF import going on takes them in): the dialog closes at once, the progress and the report are
    /// the import's. Words that are no identifier are left out and said in the report.
    fn import_ids(&mut self, ids: Vec<Identifier>, unknown: Vec<String>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let run = match lib.import.as_mut().filter(|run| run.running()) {
            Some(run) => run,
            None => lib.import.insert(ImportRun::default()),
        };
        run.total += ids.len();
        run.ids += ids.len();
        run.unknown.extend(unknown);
        let library = lib.info.id.clone();
        cx.notify();
        let queue = std::rc::Rc::new(std::cell::RefCell::new(ids.into_iter().rev().collect::<Vec<_>>()));
        for _ in 0..IMPORT_WORKERS.min(queue.borrow().len()) {
            let (queue, library) = (queue.clone(), library.clone());
            cx.spawn(async move |this, cx| {
                loop {
                    let Some(id) = queue.borrow_mut().pop() else { break };
                    let (Identifier::Doi(name) | Identifier::Pmid(name)) = id.clone();
                    let known = this.update(cx, |this, cx| {
                        let Some(run) = this.lib.as_mut().filter(|l| l.info.id == library).and_then(|l| l.import.as_mut()) else { return false };
                        run.working.push(name.clone());
                        cx.notify();
                        true
                    });
                    if !matches!(known, Ok(true)) { break }
                    let found = cx.background_executor().spawn(async move { wisp_net::lookup_paper(&id) }).await;
                    let done = this.update(cx, |this, cx| {
                        let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library) else { return false };
                        let stored = found.and_then(|paper| lib.store.import_papers(vec![paper]).and_then(|result| lib.reload().map(|()| result)).map_err(|e| format!("{e:#}")));
                        let mut row = ImportRow { name: name.clone(), outcome: Imported::Added, paper_id: None, unrecognised: false, textless: String::new() };
                        match stored.map(|result| (result.added.into_iter().next(), result.skipped.into_iter().next())) {
                            Ok((Some(paper), _)) => row.paper_id = Some(paper.id),
                            Ok((None, Some(held))) => {
                                row.outcome = if held.in_trash { Imported::InTrash } else { Imported::Held };
                                row.paper_id = Some(held.existing_id);
                            }
                            Ok((None, None)) => row.outcome = Imported::Failed(t("没有查到这篇文献。").into()),
                            Err(err) => row.outcome = Imported::Failed(err),
                        }
                        let Some(run) = lib.import.as_mut() else { return false };
                        if let Some(at) = run.working.iter().position(|working| working == &name) { run.working.remove(at); }
                        run.rows.push(row);
                        if !run.running() { run.working.clear() }
                        cx.notify();
                        true
                    });
                    if !matches!(done, Ok(true)) { break }
                }
            })
            .detach();
        }
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
                    Ok(pages) => Ok(tf!("读出了 {} 页文字，现在可以用于提问。", "Read {} pages of text, now available for chat.", pages.len())),
                    Err(reason) => Err(tf!("还是没有读出文字：{reason}", "Still no readable text: {reason}")),
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
    /// sorted into parts yet, a few side by side: new files, and those an
    /// earlier build read without telling headings from paragraphs.
    pub fn extract_missing(&mut self, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let _ = lib.store.restructure_outdated();
        let mut todo = vec![];
        for file in &lib.files {
            if lib.extracting.contains(&file.id) { continue }
            // A PDF attachment is read once, for the paper's conversation; a main text also whenever it is to be sorted again.
            let unread = file.kind != "fulltext" && file.format == "pdf" && matches!(lib.store.fulltext_pages(&file.id), Ok(None));
            if (file.kind == "fulltext" && matches!(lib.store.needs_structure(&file.id), Ok(true))) || unread {
                todo.push((file.id.clone(), lib.store.file_path(file), lib.store.fulltext_pages(&file.id).ok().flatten()));
            }
        }
        if todo.is_empty() { return }
        lib.extracting.extend(todo.iter().map(|(id, ..)| id.clone()));
        let library = lib.info.id.clone();
        todo.reverse();
        let queue = std::rc::Rc::new(std::cell::RefCell::new(todo));
        // Reading is the processor's work: one file per core, up to a few.
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, READ_WORKERS).min(queue.borrow().len());
        for _ in 0..workers {
            let (queue, library) = (queue.clone(), library.clone());
            cx.spawn(async move |this, cx| {
                loop {
                    let Some((id, path, known)) = queue.borrow_mut().pop() else { break };
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
    }

    /// The whole library in one `.wisppaper` file, for another Wisp Paper.
    fn export_library(&mut self, id: &str, attachments: bool, cx: &mut Context<Self>) {
        // The open library's own description is the newest.
        let Some(info) = self.lib.as_ref().map(|l| &l.info).filter(|i| i.id == id).or_else(|| self.libraries.iter().find(|l| l.id == id)).cloned() else { return };
        let directory = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(|| self.root.clone());
        let picked = cx.prompt_for_new_path(&directory, Some(&archive::file_name(&info.name)));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut path))) = picked.await else { return };
            if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case(archive::EXTENSION)) {
                let mut named = path.into_os_string();
                named.push(format!(".{}", archive::EXTENSION));
                path = PathBuf::from(named);
            }
            this.update(cx, |this, cx| this.say(tf!("正在导出「{}」…", "Exporting “{}”…", info.name), cx)).ok();
            let target = path.clone();
            let result = cx.background_executor().spawn(async move { archive::export(&info, &target, attachments) }).await;
            this.update(cx, |this, cx| match result {
                Ok(manifest) if attachments => this.say(tf!("已导出 {} 篇文献和 {} 个文件到 {}", "Exported {} papers and {} files to {}", manifest.papers, manifest.files, path.display()), cx),
                Ok(manifest) => this.say(tf!("已导出 {} 篇文献（不含 PDF）到 {}", "Exported {} papers (without PDFs) to {}", manifest.papers, path.display()), cx),
                Err(err) => this.fail(format!("{err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn pick_library_file(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(t("导入").into()), extensions: Some(vec![archive::EXTENSION.into()]) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            this.update(cx, |this, cx| for path in paths { this.import_library(path, cx) }).ok();
        })
        .detach();
    }

    /// A `.wisppaper` file taken in as a new library; the home page shows it once it is whole.
    pub fn import_library(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.say(tf!("正在导入 {file}…", "Importing {file}…"), cx);
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { archive::import(&root, &path) }).await;
            this.update(cx, |this, cx| {
                this.libraries = libraries::list(&this.root);
                match result {
                    Ok(info) => this.say(tf!("已导入文献库「{}」，{} 篇文献", "Imported library “{}” with {} papers", info.name, info.paper_count), cx),
                    Err(err) => this.fail(tf!("无法导入 {file}：{}", "Cannot import {file}: {}", format!("{err:#}")), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn export(&mut self, kind: ExportKind, cx: &mut Context<Self>) {
        let Some(lib) = &self.lib else { return };
        let papers: Vec<&Paper> = lib.shown.iter().map(|&i| &lib.papers[i]).collect();
        if papers.is_empty() { return self.fail(t("当前列表是空的，没有可导出的文献。"), cx) }
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
                    Ok(()) => this.say(tf!("已导出 {count} 篇到 {}", "Exported {count} papers to {}", path.display()), cx),
                    Err(err) => this.fail(tf!("无法写入 {}：{err}", "Cannot write {}: {err}", path.display()), cx),
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- questions

    /// A key or a paste in a conversation's box.
    fn chat_input(&mut self, talk: Talk, event: InputEvent, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = lib.chat_mut(talk);
        let (offered, picked) = (chat.slash_list(&self.settings.commands, cx).len(), chat.slash);
        match event {
            // Enter on a command in the list takes it; otherwise the message goes.
            InputEvent::Submit if offered > 0 => return self.pick_slash(talk, picked, cx),
            InputEvent::Submit => return self.ask(talk, cx),
            InputEvent::Tab if offered > 0 => return self.pick_slash(talk, picked, cx),
            InputEvent::Changed => {
                chat.slash_closed = false;
                chat.slash = 0;
            }
            InputEvent::Up if offered > 0 => chat.slash = (chat.slash + offered - 1) % offered,
            InputEvent::Down if offered > 0 => chat.slash = (chat.slash + 1) % offered,
            InputEvent::Cancel => chat.slash_closed = true,
            InputEvent::Images => {
                let pictures = chat.input.update(cx, |input, _| input.take_images()).into_iter().map(|image| Picture::Bytes(image.bytes)).collect();
                return self.attach_images(talk, pictures, cx);
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
    fn pick_slash(&mut self, talk: Talk, at: usize, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = lib.chat_mut(talk);
        let Some(command) = chat.slash_list(&self.settings.commands, cx).into_iter().nth(at) else { return };
        let alone = command.runs;
        chat.slash = 0;
        chat.input.update(cx, |input, cx| {
            input.choosing = false;
            input.set_text(&if alone { format!("/{}", command.name) } else { format!("/{} ", command.name) }, cx);
        });
        if alone { self.ask(talk, cx) }
        cx.notify();
    }

    /// Read pictures in and put them with the next question. Each is made
    /// one a model takes (`picture::prepare`) off the main thread.
    pub(crate) fn attach_images(&mut self, talk: Talk, mut pictures: Vec<Picture>, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let (library, selected) = (lib.info.id.clone(), lib.selected.clone());
        let chat = lib.chat_mut(talk);
        let room = MAX_ATTACHMENTS.saturating_sub(chat.attachments.len() + chat.attaching);
        let too_many = pictures.len() > room;
        pictures.truncate(room);
        chat.attaching += pictures.len();
        if too_many { self.fail(tf!("一次提问最多带 {MAX_ATTACHMENTS} 张图片。", "A question can include at most {MAX_ATTACHMENTS} images."), cx) }
        if pictures.is_empty() { return }
        let work = cx.background_executor().spawn(async move {
            let read = |picture: Picture| match picture {
                Picture::Bytes(bytes) => Ok(bytes),
                Picture::File(path) => std::fs::read(&path).map_err(|err| tf!("无法读取 {}：{err}", "Cannot read {}: {err}", path.display())),
            };
            pictures.into_iter().map(|picture| read(picture).and_then(crate::picture::prepare)).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let prepared = work.await;
            this.update(cx, |this, cx| {
                // Another library, or another paper's conversation by now: the pictures were not meant for it.
                let Some(lib) = this.lib.as_mut().filter(|l| l.info.id == library && (talk != Talk::Paper || l.selected == selected)) else { return };
                let chat = lib.chat_mut(talk);
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

    fn ask(&mut self, talk: Talk, cx: &mut Context<Self>) {
        self.send_question(talk, None, cx);
    }

    /// Ask what is in the box; or, with `again`, a question asked before
    /// (its words and pictures), leaving the box alone. True when it was sent.
    fn send_question(&mut self, talk: Talk, again: Option<(String, Vec<ChatImage>)>, cx: &mut Context<Self>) -> bool {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        // The conversation that writes says of its manuscript what it is and where it stands.
        let composing = if talk == Talk::Write {
            let Some(note) = self.agent_note(cx) else {
                self.fail(t("先在写作里打开一份稿件。"), cx);
                return false;
            };
            Some(note)
        } else {
            None
        };
        let Some(lib) = self.lib.as_mut() else { return false };
        let paper_id = lib.selected.clone();
        let chat = chat_of!(lib, talk);
        let (question, again) = match again {
            Some((text, images)) => (text, Some(images)),
            None => (chat.input.read(cx).text().trim().to_string(), None),
        };
        let asked_again = again.is_some();
        // A review is begun once, by the question that first asked for it.
        let begin_review = !asked_again && talk == Talk::Library && question.strip_prefix("/review").is_some_and(|rest| rest.starts_with(char::is_whitespace));
        let nothing = match &again { Some(images) => question.is_empty() && images.is_empty(), None => question.is_empty() && chat.attachments.is_empty() && chat.quotes.is_empty() };
        if nothing || chat.busy { return false }
        chat.error.clear();
        chat.note.clear();
        if !asked_again && chat.attaching > 0 {
            chat.error = t("图片还在读取，稍等一下再发送。").into();
            cx.notify();
            return false;
        }
        // `/search …` and the like stand for a longer request; the bubble shows what was typed.
        let prompt = match ai::read_input(if question.is_empty() && !asked_again && !chat.quotes.is_empty() { t("这段原文说了什么？在这篇文献里起什么作用？") } else { &question }, &self.settings.commands) {
            Ok(ai::Input::Ask(prompt)) => prompt,
            Ok(ai::Input::NewChat) => {
                chat.reset(cx);
                if talk == Talk::Library { lib.scope_reset(cx) }
                cx.notify();
                return false;
            }
            Err(err) => {
                chat.error = err;
                cx.notify();
                return false;
            }
        };
        chat.history = None;
        // The panel of papers floats over the place the answer comes to.
        if talk != Talk::Paper { chat.materials_open = false }

        // A changed context or protocol starts the wire again from the text of what was said:
        // the model's own turns are only valid in the conversation that produced them.
        let replay = chat.ctx.is_none() || chat.protocol != Some(profile.protocol);
        if chat.ctx.is_none() {
            let scope = match (talk, &paper_id) {
                (Talk::Paper, Some(id)) => Scope::Paper(id.clone()),
                (Talk::Paper, None) => return false,
                _ => Scope::Library,
            };
            let fulltext = match lib.store.all_fulltext() {
                Ok(fulltext) => fulltext,
                Err(err) => {
                    chat.error = format!("{err:#}");
                    cx.notify();
                    return false;
                }
            };
            // A question about one paper reads that paper; one about the library reads the current list.
            let papers: Vec<&Paper> = match &scope {
                Scope::Paper(_) => lib.papers.iter().collect(),
                // The assistant that writes draws on the whole library, whatever the list shows.
                Scope::Library if talk == Talk::Write => lib.papers.iter().collect(),
                // As `Library::scope_papers`, field by field: the conversation is held meanwhile.
                Scope::Library if !lib.asked.is_empty() => lib.papers.iter().filter(|p| lib.asked.contains(&p.id)).collect(),
                Scope::Library => lib.shown.iter().map(|&i| &lib.papers[i]).filter(|p| lib.scope_hits.as_ref().is_none_or(|hits| hits.contains(&p.id))).collect(),
            };
            if papers.is_empty() && talk != Talk::Write {
                chat.error = t("当前列表里没有文献。先添加文献，或清空筛选条件。").into();
                cx.notify();
                return false;
            }
            // A conversation taken up from the history keeps the labels its answers already use.
            // A paper is read with its attachments: supplementary figures, tables and methods are part of it.
            let attachments = match &scope {
                Scope::Paper(id) => lib.store.attachment_pages(id).unwrap_or_default(),
                Scope::Library => vec![],
            };
            let mut ctx = ai::build_context(&lib.info.name, &lib.info.topic, &papers, &lib.fields, &fulltext, &attachments, &scope, &prompt, &chat.known);
            // The tools reach every paper of the library, so every paper can be cited.
            ctx.add_sources(&lib.papers);
            chat.ctx = Some(ctx);
            // The panel over the conversation lists what was handed over from here on: all of it, not what its search had narrowed the list to.
            if talk == Talk::Library && lib.scope_hits.take().is_some() {
                lib.scope_words.clear();
                lib.scope_find.update(cx, |input, cx| input.set_text("", cx));
            }
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
        // A question asked again has them in its words already; the box's stay for the box.
        let quotes = if asked_again { vec![] } else { std::mem::take(&mut chat.quotes) };
        // A passage of an attachment is written with its number, as the answers cite it.
        let numbers: Vec<Option<usize>> = quotes.iter().map(|quote| lib.attachment_number(&quote.paper_id, &quote.file_id)).collect();
        let chat = chat_of!(lib, talk);
        let quoted: Vec<String> = quotes.iter().zip(numbers).filter_map(|(quote, file)| Some(ai::quoted_passage(&chat.sources().iter().find(|source| source.paper_id == quote.paper_id)?.label, file, quote.page, &quote.text))).collect();
        let (prompt, question) = if quoted.is_empty() {
            (prompt, question)
        } else {
            (format!("我在文献的 PDF 里选中了下面的原文，问题是就它提的；引用它时沿用这里的出处写法。\n{}\n\n{prompt}", quoted.join("\n")), [quoted.join("\n"), question].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n"))
        };
        // The record, not what the model says of itself, is where the review stands: each question of a review's conversation says so.
        // Only what steers the writing goes along (the sections by name: sending their text with every question would send it many times over),
        // except with the question that asks for the saved sections to be checked, which is about that text.
        let prompt = match chat.review.as_ref().filter(|_| talk == Talk::Library) {
            Some(review) if crate::chat::verifies_sections(&question) && review.sections.iter().any(|section| !section.body.trim().is_empty()) => format!("{prompt}\n\n{}\n\n{}", crate::chat::review_note(review), crate::chat::review_sections(review)),
            Some(review) => format!("{prompt}\n\n{}", crate::chat::review_note(review)),
            None => prompt,
        };
        // The same for the conversation that writes a manuscript: where the manuscript stands, what is on show and selected in it.
        let prompt = match composing {
            Some(note) => format!("{prompt}\n\n{note}"),
            None => prompt,
        };
        let images = match again {
            Some(images) => images,
            None => std::mem::take(&mut chat.attachments),
        };
        chat.wire.push(ai::user_message_with(profile.protocol, &prompt, &wire_images(&images)));
        // Kept with the question, so the conversation taken up later asks what was asked now, whatever the command says by then.
        let sent = if prompt == question { String::new() } else { prompt };
        chat.messages.push(ChatItem::User { text: question, images, prompt: sent });
        chat.steps = 0;
        chat.started = Some(Instant::now());
        // What the last question changed in the manuscript has been seen: the next changes are this one's.
        if talk == Talk::Write {
            let asked = match chat.messages.last() { Some(ChatItem::User { text, .. }) => text.clone(), _ => String::new() };
            self.agent_asked(&asked);
        }
        let sent = self.chat_step(talk, cx);
        if let Some(lib) = self.lib.as_mut() {
            let chat = chat_of!(lib, talk);
            if sent {
                // What 撤回 took away is not put back after a question that followed it.
                chat.retracted = None;
                if !asked_again { chat.input.update(cx, |input, cx| input.set_text("", cx)) }
            } else {
                // Nothing was sent: the question stays in the box, its pictures beside it, to be asked again (one asked again goes back to its place, `regenerate`).
                chat.wire.pop();
                let popped = chat.messages.pop();
                if !asked_again {
                    if let Some(ChatItem::User { images, .. }) = popped { chat.attachments = images }
                    chat.quotes = quotes;
                }
                if chat.messages.is_empty() { chat.ctx = None }
            }
        }
        // Kept from the question on, so that closing the window mid-answer does not lose it.
        if sent {
            self.save_chat(talk, cx);
            // The transcript is saved first so the review and its conversation
            // can be associated atomically, including across application restarts.
            if begin_review { self.start_review(cx) }
        }
        cx.notify();
        sent
    }

    /// 撤回: the question at `at` and all that followed are taken out of the
    /// conversation, and its words and pictures come back to the box, to be
    /// changed and sent again. Until the next question it can be put back
    /// (`unretract`). What a tool already did to the library stays done.
    fn retract(&mut self, talk: Talk, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = chat_of!(lib, talk);
        if chat.busy || !matches!(chat.messages.get(at), Some(ChatItem::User { .. })) { return }
        let mut tail = chat.cut(at);
        // Nothing left: the material is chosen again with the next first question.
        if chat.messages.is_empty() { chat.forget_context() }
        let Some(ChatItem::User { text, images, .. }) = tail.first() else { return };
        let (text, images) = (text.clone(), images.clone());
        // A second 撤回 further up takes the first's along: both are put back together, with the box as it was before the first.
        let (draft, attachments) = match chat.retracted.take() {
            Some(earlier) => {
                tail.extend(earlier.tail);
                (earlier.draft, earlier.attachments)
            }
            None => (chat.input.read(cx).text().to_string(), chat.attachments.clone()),
        };
        // What was being typed meanwhile is not lost: it follows the words taken back.
        let shown = if draft.trim().is_empty() { text } else { format!("{text}\n\n{}", draft.trim()) };
        chat.input.update(cx, |input, cx| input.set_text(&shown, cx));
        chat.attachments = images.into_iter().chain(attachments.iter().cloned()).collect();
        chat.retracted = Some(Retracted { tail, draft, attachments });
        chat.input.read(cx).focus(window);
        self.save_chat(talk, cx);
        cx.notify();
    }

    /// Put back what the last 撤回 took away, and the box as it was before.
    fn unretract(&mut self, talk: Talk, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = chat_of!(lib, talk);
        if chat.busy { return }
        let Some(retracted) = chat.retracted.take() else { return };
        chat.messages.extend(retracted.tail);
        // Written again from the text with the next question, as after the cut.
        chat.wire.clear();
        chat.protocol = None;
        chat.error.clear();
        chat.note.clear();
        chat.input.update(cx, |input, cx| input.set_text(&retracted.draft, cx));
        chat.attachments = retracted.attachments;
        chat.scroll.scroll_to_bottom();
        self.save_chat(talk, cx);
        cx.notify();
    }

    /// 重新回答: the answer that ends at `at` (the conversation's last) is
    /// asked for again with the question before it, under the material
    /// the conversation already has.
    fn regenerate(&mut self, talk: Talk, at: usize, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = chat_of!(lib, talk);
        if chat.busy || at + 1 != chat.messages.len() { return }
        let Some(asked) = chat.messages[..at].iter().rposition(|item| matches!(item, ChatItem::User { .. })) else { return };
        let mut tail = chat.cut(asked);
        let question = tail.remove(0);
        let ChatItem::User { text, images, .. } = &question else { return };
        let again = (text.clone(), images.clone());
        chat.retracted = None;
        if !self.send_question(talk, Some(again), cx) && let Some(lib) = self.lib.as_mut() {
            // Not sent (no model, no key): the conversation is as it was, and says why.
            let chat = chat_of!(lib, talk);
            chat.messages.push(question);
            chat.messages.extend(tail);
            cx.notify();
        }
    }

    /// 从这里分支: what was said up to the end of the answer at `at`
    /// becomes a conversation of its own, the one on show from now on; the
    /// whole of the one it came from stays in the history.
    fn fork_chat(&mut self, talk: Talk, at: usize, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_ref() else { return };
        let chat = lib.chat(talk);
        if chat.busy || chat.review.is_some() || at >= chat.messages.len() { return }
        self.save_chat(talk, cx);
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = chat_of!(lib, talk);
        chat.cut(at + 1);
        chat.retracted = None;
        // Kept anew under an id of its own, its pictures with it; a yes to every change was said to the other conversation.
        chat.id.clear();
        chat.created_at.clear();
        chat.kept = false;
        chat.stored_images.clear();
        chat.allow_changes = false;
        self.save_chat(talk, cx);
        self.say(t("已分支为新对话，原对话仍在「历史」里"), cx);
        cx.notify();
    }

    /// Send the conversation to the model and follow its turn: the text as
    /// it arrives, then either the end of the answer or the tools it asked
    /// for. False when the request could not be made.
    fn chat_step(&mut self, talk: Talk, cx: &mut Context<Self>) -> bool {
        let profile = self.settings.profile_for_or_default(Task::Chat);
        // Beside the library's tools: the connectors' and the one that reads a skill.
        let outside: Vec<_> = self.outside_tools().into_iter().map(|(tool, _)| tool).collect();
        let Some(lib) = self.lib.as_mut() else { return false };
        let chat = chat_of!(lib, talk);
        let Some(ctx) = chat.ctx.as_ref() else { return false };
        let request = match ai::chat_request_with(&profile, ctx, &chat.wire, &tools::library_catalog(), &outside) {
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
                    let chat = chat_of!(lib, talk);
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
                            chat.note = tf!("模型服务暂时没有回应（{why}），正在重试，第 {attempt} 次…", "The model service has not responded ({why}). Retrying, attempt {attempt}…");
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
                                    (t("已达到单次提问的步数上限，工具没有执行。"), t("已达到单次提问的步数上限。可以继续提问，让它接着做。"))
                                } else {
                                    (t("回答被截断，工具没有执行。"), ai::stop_note(&turn.stop).unwrap_or(t("回答没有正常结束。")))
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
                    if let Some(calls) = calls { this.run_tools(talk, calls, cx) }
                    if ended { this.save_chat(talk, cx) }
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
    fn run_tools(&mut self, talk: Talk, calls: Vec<ToolCall>, cx: &mut Context<Self>) {
        // A connector's tool or a skill: looked up before the library is borrowed.
        let mut outside: Vec<_> = calls.iter().map(|call| self.outside_call(&call.name, &call.args)).collect();
        outside.reverse();
        let Some(lib) = self.lib.as_mut() else { return };
        let store = &lib.store;
        // The manuscript the conversation that writes is about.
        let about = Some(lib.writing.agent.about.clone()).filter(|id| talk == Talk::Write && !id.is_empty());
        let chat = chat_of!(lib, talk);
        let first = chat.messages.len();
        let catalog = tools::catalog();
        let mut prepared = vec![];
        for call in calls {
            if let Some((outside, confirm, line, details)) = outside.pop().flatten() {
                chat.messages.push(ChatItem::Tool { call: line.clone(), result: None });
                prepared.push(PreparedCall { id: call.id, name: call.name, args: call.args, writes: false, confirm, line, details, outside: Some(outside) });
                continue;
            }
            let tool = catalog.iter().find(|t| t.name == call.name);
            let line = tools::describe_call(&call.name, call.args.as_ref().unwrap_or(&Value::Null));
            chat.messages.push(ChatItem::Tool { call: line.clone(), result: None });
            // The model may name a paper by its label.
            let composes = tools::MANUSCRIPT.contains(&call.name.as_str()) || ["read_manuscript"].contains(&call.name.as_str());
            let args = call.args.map(|mut args| {
                ai::resolve_labels(&mut args, chat.sources());
                // Said of no manuscript, a call of that conversation is about its own.
                if composes && let Some(about) = &about && args["manuscript"].as_str().is_none_or(|named| named.trim().is_empty()) && args.is_object() { args["manuscript"] = Value::String(about.clone()) }
                args
            });
            // In the conversation that writes a manuscript the pen was handed over: writing that manuscript is not asked about again.
            let handed = about.as_ref().is_some_and(|about| tools::MANUSCRIPT.contains(&call.name.as_str()) && args.as_ref().is_ok_and(|args| args["manuscript"].as_str() == Some(about)));
            // A call that cannot run as written has nothing to ask about: it fails by itself.
            let asks = args.is_ok() && tool.is_some_and(|t| t.confirm) && !handed;
            // What it would change, for the card that asks; one that would be refused anyway is not asked about.
            let details = match &args {
                Ok(args) if asks => tools::preview_call(store, &call.name, args).ok(),
                _ => Some(vec![]),
            };
            let (confirm, details) = (asks && details.is_some(), details.unwrap_or_default());
            prepared.push(PreparedCall { id: call.id, name: call.name, args, writes: tool.is_some_and(|t| t.writes), confirm, line, details, outside: None });
        }
        chat.scroll.scroll_to_bottom();
        if prepared.iter().any(|call| call.confirm) && !chat.allow_changes {
            chat.pending = Some(PendingTools { calls: prepared, first });
            return cx.notify();
        }
        self.execute_tools(talk, prepared, first, true, cx);
    }

    /// Run a turn's tools off the main thread, then hand the results to
    /// the model with the next step. Calls that only read run side by
    /// side; a turn that changes the library runs in the order it was
    /// given. Without the person's consent (`allowed`) the calls that asked
    /// for it are answered with a refusal and the others still run.
    fn execute_tools(&mut self, talk: Talk, calls: Vec<PreparedCall>, first: usize, allowed: bool, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let info = lib.info.clone();
        let chat = lib.chat_mut(talk);
        let Some(protocol) = chat.protocol else { return };
        let sources: Arc<Vec<ai::Source>> = Arc::new(chat.sources().to_vec());
        let generation = chat.generation;
        chat.running = calls.iter().map(|call| call.id.clone()).collect();
        let wrote = calls.iter().any(|call| call.writes && (allowed || !call.confirm));
        let composed = calls.iter().any(|call| tools::MANUSCRIPT.contains(&call.name.as_str()) && (allowed || !call.confirm));
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
                // And the manuscripts, when one was written: the editor follows, and what changed is shown.
                if composed { this.manuscripts_synced(cx) }
                let Some(lib) = this.lib.as_mut() else { return };
                let chat = lib.chat_mut(talk);
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
                    if !this.chat_step(talk, cx) { this.save_chat(talk, cx) }
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
    pub(crate) fn stop_chat(&mut self, talk: Talk, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let chat = lib.chat_mut(talk);
        if !chat.busy { return }
        chat.generation += 1;
        let unanswered: Vec<String> = match chat.pending.take() {
            Some(pending) => pending.calls.into_iter().map(|call| call.id).collect(),
            None => std::mem::take(&mut chat.running),
        };
        if let Some(protocol) = chat.protocol {
            if !unanswered.is_empty() {
                let results: Vec<(String, String, bool)> = unanswered.into_iter().map(|id| (id, t("用户中止了这次提问，这个工具的结果没有交回。").to_string(), true)).collect();
                chat.wire.extend(ai::tool_results(protocol, &results));
            } else if let Some(ChatItem::Answer(text)) = chat.messages.last() && !text.is_empty() {
                chat.wire.push(ai::assistant_message(text));
            }
        }
        for item in chat.messages.iter_mut() {
            if let ChatItem::Tool { result, .. } = item && result.is_none() { *result = Some((t("已中止").into(), true)) }
        }
        chat.note = t("已停止。").into();
        chat.end_run();
        self.save_chat(talk, cx);
        cx.notify();
    }

    /// Keep the conversation in the library's history, as it stands.
    pub(crate) fn save_chat(&mut self, talk: Talk, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        // What the conversation is about is what it is kept under: nothing for the library's, the paper, the manuscript.
        let Some(paper_id) = lib.talk_about(talk) else { return };
        let chat = chat_of!(lib, talk);
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
        if let Err(err) = result { self.fail(tf!("这次对话没能保存：{err:#}", "Conversation not saved: {err:#}"), cx) }
    }

    /// Take up a conversation from the history: what was said is shown
    /// again, and the next question continues it. The model is given the
    /// library as it is now, under the labels the old answers use.
    pub(crate) fn open_chat(&mut self, talk: Talk, id: &str, cx: &mut Context<Self>) {
        self.open_chat_said(talk, id, cx);
        if talk == Talk::Library && let Some(Err(err)) = self.lib.as_mut().map(|lib| lib.load_review()) { self.fail(tf!("无法读取综述记录：{err:#}", "Cannot read review record: {err:#}"), cx) }
    }

    /// The search box of the panel over the library's conversation was typed into.
    pub fn scope_typed(&mut self, words: String, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        lib.scope_words = words;
        lib.scope_refilter();
        cx.notify();
    }

    /// The panel over the library's conversation changes what the first question is put to.
    fn scope(&mut self, what: ScopeDo, cx: &mut Context<Self>) {
        self.pinning = false;
        let Some(lib) = self.lib.as_mut() else { return };
        // The material is handed over with the first question; after that the scope is the conversation's.
        let open = lib.library_chat.ctx.is_none() && lib.library_chat.messages.is_empty();
        match what {
            ScopeDo::ClearFind => {
                lib.scope_find.update(cx, |input, cx| input.set_text("", cx));
                return self.scope_typed(String::new(), cx);
            }
            _ if !open => return self.say(t("这次对话的材料已经交给模型。在「新对话」里可以重新限定文献。"), cx),
            ScopeDo::Pin(id) => {
                if lib.paper(&id).is_some() && !lib.asked.contains(&id) { lib.asked.push(id) }
            }
            ScopeDo::Unpin(id) => lib.asked.retain(|asked| *asked != id),
            ScopeDo::UnpinAll => lib.asked.clear(),
        }
        cx.notify();
    }

    fn open_chat_said(&mut self, talk: Talk, id: &str, cx: &mut Context<Self>) {
        let Some(lib) = self.lib.as_mut() else { return };
        let conversation = match lib.store.conversation(id) {
            Ok(Some(conversation)) => conversation,
            Ok(None) => return self.fail(t("这次对话已经不在了。"), cx),
            Err(err) => return self.fail(format!("{err:#}"), cx),
        };
        if talk == Talk::Library { lib.scope_reset(cx) }
        let chat = chat_of!(lib, talk);
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
            self.modal_error = t("写上题目再查找。").into();
            return;
        }
        let year = match text(&form.year) {
            year if year.is_empty() => None,
            year => match year.parse::<i32>().ok().filter(|y| (1800..=2200).contains(y)) {
                Some(year) => Some(year),
                None => {
                    self.modal_error = t("年份应为数字，例如 2019；不确定可以留空。").into();
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
                if found.asked.iter().all(|asked| !asked.error.is_empty()) { this.modal_error = t("几个来源都没有查成，见下面的原因。检查网络或「设置 › 网络」里的代理后再试。").into() }
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
            Some(t("文献库里已经有这条记录了，不能再写进这一篇。"))
        } else if fields.is_empty() {
            Some(t("没有勾选要写入的字段。"))
        } else if self.lib.as_ref().and_then(|lib| lib.paper(&id)).is_none_or(|paper| paper.updated_at != form.seen) {
            Some(t("这篇文献在查找期间被改过。关掉这个窗口重新打开，按它现在的内容再核对。"))
        } else {
            None
        };
        if let Some(problem) = problem {
            self.modal_error = problem.into();
            return;
        }
        if !self.edit_paper(&id, cx, |paper| complete::apply(paper, &candidate, &fields)) { return }
        self.run(Cmd::CloseModal, window, cx);
        self.say(tf!("已补全：{}", "Completed: {}", self.lib.as_ref().and_then(|lib| lib.paper(&id)).map(|paper| paper.title.clone()).unwrap_or_default()), cx);
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
            volume: self.modal_input("", &paper.volume, false, window, cx),
            issue: self.modal_input("", &paper.issue, false, window, cx),
            pages: self.modal_input("例如 357-362", &paper.pages, false, window, cx),
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
        let hint = self.modal_input("说明（可留空）：让助手提取时找什么，例如“研究用的物种，写学名”", &field.hint, false, window, cx);
        self.modal = Some(Modal::Fields { editing, name, options, hint, kind: field.kind, in_table: field.in_table });
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
            lib.guide.error = t("先写下研究主题或几个关键词。").into();
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
                    if added > 0 { this.say(tf!("自动更新：新增 {added} 篇待判断的文献", "Automatic update: {added} new papers awaiting screening"), cx) }
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
    /// With `only`, it is asked about those papers, whatever it said of them before.
    /// `scope` is which papers a run over the queue takes: `pending`, the
    /// `included` ones (继续筛) or all `decided` ones; such a run begins a
    /// round of the funnel when the goal is new (`Store::begin_round`).
    pub(crate) fn screen_start(&mut self, scope: &str, only: Option<Vec<String>>, cx: &mut Context<Self>) {
        let profile = self.settings.profile_for_or_default(Task::Screen);
        let workers = self.settings.screen_workers;
        let Some(lib) = self.lib.as_mut() else { return };
        if lib.screen.running() { return }
        lib.screen.error.clear();
        // The key and address are checked once here, not once per paper.
        if let Err(err) = if profile.judges() { wisp_core::judge::ready(&profile) } else { ai::plain_request(&profile, "s", "p").map(|_| ()) } {
            lib.screen.error = err;
            return cx.notify();
        }
        let whole = only.is_none();
        let mut todo: Vec<String> = only.unwrap_or_else(|| lib.screen_unasked(scope != "pending").iter().filter(|p| scope != "included" || p.screening == "included").map(|p| p.id.clone()).collect());
        if todo.is_empty() {
            lib.screen.error = match scope {
                "pending" => t("待判断的文献都已经有按当前目标的建议了。"),
                "included" => t("已纳入的文献都有按当前目标的建议了。"),
                _ => t("已判断的文献都有按当前目标的建议了。"),
            }.into();
            return cx.notify();
        }
        // A run over the queue is part of the funnel; asking about one paper is not.
        if whole && lib.store.begin_round(&lib.info.criteria, scope).is_ok() && let Ok(rounds) = lib.store.screening_rounds() { lib.rounds = rounds }
        todo.reverse();
        let screen = &mut lib.screen;
        screen.run += 1;
        (screen.total, screen.done, screen.failed, screen.streak, screen.todo) = (todo.len(), 0, 0, 0, todo);
        screen.reading.clear();
        screen.model = profile.labelled();
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
        // A paper that left the library meanwhile is done with, so the run still ends at its total.
        let Some(paper) = lib.paper(&id).cloned() else {
            lib.screen.done += 1;
            return self.screen_next(run, cx);
        };
        let pages = lib.store.paper_pages(&id).ok().flatten();
        let (system, prompt, basis) = ai::screen_prompt(&lib.info.topic, &lib.info.criteria, &paper, pages.as_deref());
        let author = profile.labelled();
        // What it is judged against, kept with the answer: the goal may change while it is read.
        let criteria = Some(lib.info.criteria.clone());
        // Nothing to read: say so without asking, rather than have the model guess from a title.
        if basis == "none" {
            let mut suggestion = Suggestion { paper_id: id, verdict: "uncertain".into(), reason: t("这篇文献没有摘要，也没有保存全文，无法判断。").into(), basis: basis.into(), author, criteria, ..Default::default() };
            let saved = lib.store.save_suggestion(&mut suggestion);
            lib.screen.done += 1;
            if saved.is_ok() { lib.suggestions.insert(suggestion.paper_id.clone(), suggestion); }
            return self.screen_next(run, cx);
        }
        // A model that judges is asked its own way, about the abstract (the first pages without one): it answers
        // with a verdict and how sure it is, and no reason. Any other model is asked in words and reads the full text.
        let judge = profile.judges().then(|| wisp_core::judge::screen_request(&profile, &lib.info.topic, &lib.info.criteria, &paper, pages.as_deref()));
        let (request, basis) = match judge {
            Some(Ok((request, basis))) => (Ok(request), basis),
            Some(Err(err)) => (Err(err), basis),
            None => (ai::plain_request(&profile, &system, &prompt), basis),
        };
        let request = match request {
            Ok(request) => request,
            Err(err) => {
                lib.screen.error = err;
                lib.screen.todo.clear();
                return;
            }
        };
        lib.screen.reading.push(id);
        let (library, protocol, judges) = (lib.info.id.clone(), profile.protocol, profile.judges());
        let work = cx.background_executor().spawn(async move {
            let judged = if judges { wisp_net::judge(request).and_then(|reply| wisp_core::judge::read_screening(&reply)) } else { wisp_net::complete(request, protocol).and_then(|reply| ai::parse_judgement(&reply, &paper, pages.as_deref())) };
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
                            lib.screen.error = tf!("连续 {SCREEN_GIVE_UP} 篇没有得到可用的回答，已停止。最后一篇「{}」：{err}", "Stopped after {SCREEN_GIVE_UP} consecutive unusable responses. Last paper “{}”: {err}", paper.title);
                            lib.screen.todo.clear();
                        } else {
                            lib.screen.error = tf!("「{}」没有判断成，留待下一轮：{err}", "Could not screen “{}”; left for the next round: {err}", paper.title);
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
        sorting.model = profile.labelled();
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
        let author = profile.labelled();
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
                if pages.iter().all(|page| page.trim().is_empty()) { return Err(t("模型在页面上没有读到文字。").to_string()) }
                Ok(Made::Read(pages, count))
            } else {
                let blocks = structure::blocks(&pages);
                let request = ai::plain_request(&profile, structure::OUTLINE_SYSTEM, &structure::outline_prompt(&name, &blocks))?;
                // Told to list only body text and explain nothing, a model answers a file of legends (a supplement) with nothing: that is its answer, and `parse_outline` says what it means.
                let reply = match wisp_net::complete(request, protocol) {
                    Err(nothing) if nothing == wisp_net::NO_CONTENT => String::new(),
                    reply => reply?,
                };
                let read = structure::parse_outline(&reply, &blocks, &summary)?;
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
                        // A file without body text is an answer about the file: the service is well, and the run goes on.
                        if err != structure::NO_BODY { sorting.streak += 1 }
                        if sorting.streak >= SCREEN_GIVE_UP {
                            sorting.error = tf!("连续 {SCREEN_GIVE_UP} 篇没有成功，已停止。最后一篇「{title}」：{err}", "Stopped after {SCREEN_GIVE_UP} consecutive failures. Last paper “{title}”: {err}");
                            sorting.todo.clear();
                        } else {
                            sorting.error = tf!("「{title}」没有整理成：{err}", "Could not structure “{title}”: {err}");
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
        if lib.fetching.as_ref().is_some_and(|run| run.stopped && run.running()) { return self.say(t("正在结束当前请求，请稍后再开始新的查找。"), cx) }
        let known = wisp_core::openaccess::lookable;
        let lacking = |paper: &Paper| matches!(paper.fulltext.as_str(), "missing" | "linked");
        let wanted: Vec<String> = match &only {
            Some(id) => lib.paper(id).filter(|paper| lacking(paper)).map(|paper| paper.id.clone()).into_iter().collect(),
            None => lib.shown.iter().map(|&i| &lib.papers[i]).filter(|paper| lacking(paper) && known(paper)).map(|paper| paper.id.clone()).collect(),
        };
        if let Some(paper) = only.as_deref().and_then(|id| lib.paper(id)) && !known(paper) { return self.fail(t("这篇文献没有 DOI、PMID 或 arXiv 编号，无从查起。先在「编辑信息与词条」里补上编号。"), cx) }
        if wanted.is_empty() { return self.say(t("当前列表里没有可以查找的文献：要么已经有 PDF，要么没有 DOI、PMID 或 arXiv 编号。"), cx) }
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
                    let version = t(wisp_core::openaccess::version_label(found["version"].as_str().unwrap_or("")));
                    let from = [found["source"].as_str().unwrap_or(""), version].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ");
                    match found["textReason"].as_str() {
                        Some(reason) => tf!("来自 {from}。没读出文字：{reason}", "From {from}. No text extracted: {reason}"),
                        None => tf!("来自 {from}，读出 {} 页文字", "From {from}, {} pages of text read", found["pages"].as_u64().unwrap_or(0)),
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
            tools::call_in_with(&mut store, &info, name, &args, &wisp_net::get_text, &wisp_net::download, Some(&wisp_net::notes::publish))
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

    /// One identifier of 通过 DOI / PMID 添加 looked up: its paper joins the library unless it is held already. With
    /// the last one the dialog closes and says what came of them; those nothing came of stay in it with the reasons,
    /// to be mended and tried again. `false` when the dialog that asked is no longer open on this library.
    fn added_by_id(&mut self, asked: EntityId, library: &str, id: Identifier, found: Result<Paper, String>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !matches!(&self.modal, Some(Modal::AddById { input, run: Some(_) }) if input.entity_id() == asked) { return false }
        let Some(lib) = self.lib.as_mut().filter(|l| l.info.id == library) else { return false };
        let stored = found.and_then(|paper| lib.store.import_papers(vec![paper]).and_then(|result| lib.reload().map(|()| result)).map_err(|e| format!("{e:#}")));
        let Some(Modal::AddById { input, run }) = self.modal.as_mut() else { return false };
        let Some(going) = run.as_mut() else { return false };
        let (Identifier::Doi(name) | Identifier::Pmid(name)) = id;
        going.done += 1;
        match stored.map(|result| (result.added.into_iter().next(), result.skipped.into_iter().next())) {
            Ok((Some(paper), _)) => going.added.push(paper.id),
            Ok((None, Some(held))) => going.skipped.push(if held.in_trash { tf!("「{}」在回收站中。", "“{}” is in the trash.", held.title) } else { tf!("「{}」已在文献库中。", "“{}” is already in this library.", held.title) }),
            Ok((None, None)) => {}
            Err(err) => going.failed.push((name, err)),
        }
        cx.notify();
        if going.done < going.total { return true }
        let (input, run) = (input.clone(), run.take().unwrap_or_default());
        let single = run.total == 1;
        // One paper says its title, a list how many: 「导入 5 篇，跳过 2 篇已有的。」
        let summary = match (run.added.as_slice(), run.skipped.as_slice()) {
            ([], []) => String::new(),
            ([only], []) if single => tf!("已添加：{}", "Added: {}", self.lib.as_ref().and_then(|l| l.paper(only)).map(|p| p.title.clone()).unwrap_or_default()),
            ([], [only]) => only.clone(),
            (added, skipped) => tf!("导入 {} 篇{}。", "Imported {} papers{}.", added.len(), if skipped.is_empty() { String::new() } else { tf!("，跳过 {} 篇已有的", ", skipped {} existing", skipped.len()) }),
        };
        // The one paper asked for and already held is said in the dialog, where the identifier still is.
        if run.failed.is_empty() && !(single && run.added.is_empty()) {
            self.run(Cmd::CloseModal, window, cx);
            if let [only] = run.added.as_slice() { self.select(Some(only.clone()), window, cx) }
            self.say(summary, cx);
            return false;
        }
        let mut lines = vec![];
        if !single {
            input.update(cx, |input, cx| input.set_text(&run.failed.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join("\n"), cx));
            if !summary.is_empty() { lines.push(summary.clone()) }
            lines.push(tf!("{} 个没有添加，留在了上面，可以改好再试：", "{} not added. They are left above to fix and try again:", run.failed.len()));
        } else if run.failed.is_empty() {
            lines.push(summary);
        }
        lines.extend(run.failed.iter().map(|(name, err)| if single { err.clone() } else { format!("{name}：{err}") }));
        self.modal_error = lines.join("\n");
        false
    }

    fn submit_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = |input: &Entity<TextInput>, cx: &App| input.read(cx).text().trim().to_string();
        self.modal_error.clear();
        let Some(modal) = self.modal.as_mut() else { return };
        match modal {
            Modal::ExportNote(_) => {}
            Modal::Confirm { cmd, .. } => {
                let cmd = cmd.clone();
                self.run(cmd, window, cx);
            }
            Modal::ImportReport => self.run(Cmd::CloseModal, window, cx),
            Modal::Section { .. } | Modal::History { .. } => self.submit_manuscript_modal(window, cx),
            Modal::Headings(_) => {
                self.headings_done(true, cx);
                if self.modal.is_none() { window.focus(&self.focus) }
            }
            Modal::ReviewEvidence(_) => self.submit_review_evidence(window, cx),
            Modal::ReviewText { .. } => self.submit_review_text(window, cx),
            Modal::Figure(_) => self.submit_figure(window, cx),
            Modal::Complete(form) => {
                if form.chosen.is_some() { self.complete_apply(window, cx) } else { self.complete_find(cx) }
            }
            Modal::Zotero { .. } => self.run(Cmd::PickZotero, window, cx),
            Modal::ZoteroReport => self.run(Cmd::CloseModal, window, cx),
            Modal::FetchReport => self.run(Cmd::CloseModal, window, cx),
            Modal::Library { editing, name, topic, color } => {
                let (name, topic, color) = (text(name, cx), text(topic, cx), color.clone());
                let mut created = None;
                let result = match editing.clone() {
                    None => libraries::create(&self.root, &name, &topic, &color).map(|info| created = Some(info.id)),
                    Some(id) => {
                        // The open library's own record is the newer one: its screening goal is typed on 筛选 and kept there, not in the home page's list.
                        let Some(mut info) = self.lib.as_ref().filter(|l| l.info.id == id).map(|l| l.info.clone()).or_else(|| self.libraries.iter().find(|l| l.id == id).cloned()) else { return };
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
                    self.modal_error = t("标签不能没有名字。要去掉它，在标签上点右键选「去掉这个标签」。").into();
                    return;
                }
                if name == tag || self.retag(&tag, Some(&name), cx) { self.run(Cmd::CloseModal, window, cx) }
            }
            Modal::EmptyTrash { phrase } => {
                if crate::dialogs::phrase_typed(&text(phrase, cx)) { self.run(Cmd::EmptyTrash, window, cx) }
            }
            Modal::TagPapers { ids, name } => {
                let (ids, tags) = (ids.clone(), wisp_core::model::split_list(&text(name, cx)));
                if tags.is_empty() {
                    self.modal_error = t("写上要添加的标签，多个用逗号或分号隔开。").into();
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
                    self.say(tf!("已为 {} 篇文献添加标签。", "Added tags to {} papers.", ids.len()), cx);
                }
            }
            Modal::Note { paper_id, text: input } => {
                let (paper_id, note) = (paper_id.clone(), text(input, cx));
                if note.is_empty() {
                    self.modal_error = t("没有内容可以记下。").into();
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
                self.say(t("已记到这篇文献的「我的笔记」里。"), cx);
            }
            Modal::AddById { input, run } => {
                if run.is_some() { return }
                let (ids, unknown) = lookup::parse_identifiers(&text(input, cx));
                if ids.is_empty() {
                    self.modal_error = t("没有认出 DOI 或 PMID。DOI 形如 10.1038/s41586-020-2649-2，PMID 是一串数字。").into();
                    return;
                }
                // Several are an import of their own, in the background; what is not an identifier is left out.
                if ids.len() > 1 || !unknown.is_empty() {
                    self.run(Cmd::CloseModal, window, cx);
                    self.import_ids(ids, unknown, cx);
                    return;
                }
                *run = Some(IdRun { total: ids.len(), ..Default::default() });
                // The dialog that asked, told by its field: closed meanwhile, its answers are dropped, and it is not
                // taken for a dialog opened after it (which it would close, adding papers nobody is waiting for).
                let asked = input.entity_id();
                let library = self.lib.as_ref().map(|l| l.info.id.clone()).unwrap_or_default();
                // The wait is the two services asked about each identifier: a few are asked side by side.
                let queue = std::rc::Rc::new(std::cell::RefCell::new(ids.into_iter().rev().collect::<Vec<_>>()));
                for _ in 0..IMPORT_WORKERS.min(queue.borrow().len()) {
                    let (queue, library) = (queue.clone(), library.clone());
                    cx.spawn_in(window, async move |this, cx| {
                        loop {
                            let Some(id) = queue.borrow_mut().pop() else { break };
                            let looked = id.clone();
                            let found = cx.background_executor().spawn(async move { wisp_net::lookup_paper(&looked) }).await;
                            let open = this.update_in(cx, |this, window, cx| this.added_by_id(asked, &library, id, found, window, cx));
                            if !matches!(open, Ok(true)) { break }
                        }
                    })
                    .detach();
                }
            }
            Modal::Paper(form) => {
                let Some(lib) = self.lib.as_mut() else { return };
                let mut paper = form.id.as_ref().and_then(|id| lib.paper(id)).cloned().unwrap_or_default();
                let year = text(&form.year, cx);
                paper.year = match year.parse::<i32>() {
                    Ok(year) => Some(year),
                    Err(_) if year.is_empty() => None,
                    Err(_) => {
                        self.modal_error = t("年份应为数字，例如 2024；不确定可以留空。").into();
                        return;
                    }
                };
                paper.title = text(&form.title, cx);
                paper.authors = text(&form.authors, cx);
                paper.journal = text(&form.journal, cx);
                paper.volume = text(&form.volume, cx);
                paper.issue = text(&form.issue, cx);
                paper.pages = text(&form.pages, cx);
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
                        self.say(if editing { t("修改已保存。") } else { t("已添加。") }, cx);
                    }
                    Err(err) => self.modal_error = format!("{err:#}"),
                }
            }
            Modal::Extract(_) => self.submit_extract(window, cx),
            Modal::Fields { editing, name, options, hint, kind, in_table } => {
                let mut field = FieldDef { id: editing.clone().unwrap_or_default(), name: text(name, cx), kind: *kind, options: split_list(&text(options, cx)), in_table: *in_table, hint: text(hint, cx) };
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
                    self.modal_error = t("每次取回的篇数应为数字，例如 50。").into();
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
        return (call.id, "用户没有同意这次操作，它没有执行。不要重试；需要时向用户说明它会做什么，由用户决定。".into(), true, t("没有执行：未获同意").into());
    }
    if let Some(outside) = call.outside {
        let result = match outside {
            crate::custom::Outside::Said(said) => said,
            crate::custom::Outside::Connector(connector, tool) => call.args.and_then(|args| wisp_net::mcp::call(&connector, &tool, &args)),
        };
        return match result {
            Ok(text) => (call.id, text, false, t("完成").into()),
            Err(err) => (call.id, err.clone(), true, err),
        };
    }
    let result = call.args.and_then(|args| {
        let mut store = Store::open(&info.dir).map_err(|e| format!("{e:#}"))?;
        // As the tool table has it; what is written into a manuscript cites as the conversation does (`compose::call`).
        let mut value = wisp_core::compose::call_with(&mut store, info, &call.name, &args, sources, &wisp_net::get_text, &wisp_net::download, Some(&wisp_net::notes::publish))?;
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
                "do" => {
                    for step in arg.split('|') { self.dev_step(step, window, cx) }
                    return;
                }
                // UI layout checks without changing the normal startup or real libraries.
                "size" => {
                    if let Some((width, height)) = arg.split_once(';').and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))) {
                        window.resize(size(px(width), px(height)));
                    }
                    return;
                }
                "ui" => match arg {
                    "clear" => {
                        self.run(Cmd::CloseModal, window, cx);
                        self.run(Cmd::CloseSettings, window, cx);
                        self.menu = None;
                        return;
                    }
                    "home" => Some(Cmd::GoHome),
                    "detail-close" => Some(Cmd::CloseDetail),
                    "new-library" => Some(Cmd::NewLibrary),
                    "edit-library" => self.lib.as_ref().map(|lib| Cmd::EditLibrary(lib.info.id.clone())),
                    "add" => Some(Cmd::AddById),
                    "manual" => Some(Cmd::AddManual),
                    "fields" => Some(Cmd::ManageFields),
                    "new-search" => Some(Cmd::NewSearch),
                    "zotero" => Some(Cmd::ImportZotero),
                    "edit-paper" => self.lib.as_ref().and_then(|lib| lib.selected.clone()).map(Cmd::EditPaper),
                    "delete-paper" => self.lib.as_ref().and_then(|lib| lib.selected.clone()).map(Cmd::AskDeletePaper),
                    // The trash page's head: 全部恢复 and the dialog of 清空回收站.
                    "restore-all" => Some(Cmd::RestoreAll),
                    "empty-trash" => Some(Cmd::AskEmptyTrash),
                    "guide" => Some(Cmd::OpenGuide(true)),
                    "guide-search" => Some(Cmd::OpenGuide(false)),
                    _ => None,
                },
                "open" => self.libraries.get(arg.parse::<usize>().unwrap_or(0)).map(|l| Cmd::OpenLibrary(l.id.clone())),
                "select" => self.lib.as_ref().and_then(|l| l.shown.get(arg.parse::<usize>().unwrap_or(0)).map(|&i| Cmd::Select(l.papers[i].id.clone()))),
                "tab" => Some(Cmd::SetTab(Tab::from_key(arg))),
                "page" => Some(Cmd::SetPage(Page::from_key(arg))),
                // `restore` goes back to where `session.json` says the window was, as a start that is not scripted does.
                "restore" => {
                    self.restore_session(window, cx);
                    return;
                }
                // `write:methods` shows another part of the manuscript; `draft:<text>` leaves text in it, as if typed.
                "write" => Some(Cmd::WriteSection(arg.to_string())),
                // `manuscript:new` makes another manuscript, `manuscript:<title>` gives the current one its title;
                // `section` / `section:<id>` is the dialog for a new section / one's name and kind;
                // `reading` shows the whole manuscript, `versions` the section's history.
                "manuscript" if arg == "new" => Some(Cmd::NewManuscript),
                // `manuscript:open;<n>` writes the n-th of the library's manuscripts, from 0.
                "manuscript" if arg.starts_with("open;") => self.lib.as_ref().and_then(|lib| lib.writing.manuscripts.get(arg[5..].parse::<usize>().ok()?)).map(|m| Cmd::WriteManuscript(m.id.clone())),
                "manuscript" => {
                    if let Some(lib) = self.lib.as_ref() { lib.writing.title.update(cx, |input, cx| input.set_text(arg, cx)) }
                    self.write_title_typed(arg.to_string(), cx);
                    None
                }
                "section" => Some(Cmd::EditSection((!arg.is_empty()).then(|| arg.to_string()))),
                // `split` / `split:heading` cut the section at the caret's paragraph (that paragraph a new section's first / its heading); `merge` puts the section being written back into the one before;
                // `headings:<n>` / `headings:all` / `headings:none` tick in the dialog of a Word file without heading styles (`submit` imports).
                "split" => Some(Cmd::SplitSection(arg == "heading")),
                "merge" => self.lib.as_ref().map(|lib| Cmd::MergeSection(lib.writing.section_id.clone())),
                "headings" => Some(Cmd::Comment(match arg {
                    "all" => crate::comments::Do::Headings(true),
                    "none" => crate::comments::Do::Headings(false),
                    n => crate::comments::Do::Heading(n.parse().unwrap_or(0)),
                })),
                "reading" => Some(Cmd::WriteReading(true)),
                "versions" => Some(Cmd::WriteHistory),
                // `importdoc:<path>` starts a manuscript from a Word or Markdown file; `comments` shows the comments beside the manuscript (`comments:off` the candidates);
                // `comment:<step>` is `review`, `stop`, `write`, `save`, `cancel`, `takeall`, `import`, `fromreview` (a manuscript of the newest review the assistant wrote sections for), and by a comment's place among those of the section from 0: `take;<n>`, `back;<n>`, `drop;<n>`, `done;<n>`, `show;<n>`;
                // `wish:<text>` / `commenttext:<text>` leave text in the review's box / the comment being written; `exportdoc:<path>` / `exportdoc:track;<path>` write the manuscript as a Word file there.
                "importdoc" => {
                    self.import_manuscript_files(vec![PathBuf::from(arg)], cx);
                    None
                }
                "comments" => Some(Cmd::WriteSide(if arg == "off" { Side::Candidates } else { Side::Comments })),
                // `references` shows the reference list beside the manuscript (`references:off` the candidates), `references:whole` the whole manuscript's, as the outline's row does.
                "references" => Some(match arg { "off" => Cmd::WriteSide(Side::Candidates), "whole" => Cmd::WriteReferences, _ => Cmd::WriteSide(Side::References) }),
                "wish" | "commenttext" => {
                    if let Some(comments) = self.lib.as_ref().map(|lib| &lib.writing.comments) { (if name == "wish" { &comments.wish } else { &comments.draft }).update(cx, |input, cx| input.set_text(arg, cx)) }
                    None
                }
                "exportdoc" => {
                    let (track, path) = arg.strip_prefix("track;").map_or((false, arg), |path| (true, path));
                    self.write_word_to(PathBuf::from(path), track, cx);
                    None
                }
                "comment" => {
                    use crate::comments::Do;
                    let (what, n) = arg.split_once(';').map_or((arg, 0), |(what, n)| (what, n.parse::<usize>().unwrap_or(0)));
                    let nth = self.lib.as_ref().and_then(|lib| lib.writing.comments.list.iter().filter(|comment| comment.section_id == lib.writing.section_id).nth(n)).map(|comment| comment.id.clone()).unwrap_or_default();
                    Some(Cmd::Comment(match what {
                        "review" => Do::Review,
                        "stop" => Do::Stop,
                        "write" => Do::Write,
                        "save" => Do::Save,
                        "cancel" => Do::Cancel,
                        "takeall" => Do::TakeAll,
                        "import" => Do::Import,
                        "fromreview" => Do::FromReview(self.reviews_written().first().map(|(conversation, _, _)| conversation.clone()).unwrap_or_default()),
                        "take" => Do::Take(nth),
                        "back" => Do::Back(nth),
                        "drop" => Do::Drop(nth),
                        "done" => Do::Done(nth, true),
                        _ => Do::Show(nth),
                    }))
                }
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
                // `click:x;y`, `click2:x;y` / `click3:x;y` (the second click of a double click, the third of a triple), and `wheel:x;y;dy;cmd` (dy in pixels;
                // `cmd` or `shift` held, or nothing).
                // A key as the window hears it (`key:down`, `key:cmd-backspace`).
                "key" => {
                    if let Ok(stroke) = Keystroke::parse(arg) { window.defer(cx, move |window, cx| { window.dispatch_keystroke(stroke, cx); }) }
                    return;
                }
                "press" | "move" | "hover" | "release" | "click" | "click2" | "click3" | "rclick" | "wheel" => {
                    let mut numbers = arg.split(';');
                    let mut number = || numbers.next().and_then(|n| n.parse::<f32>().ok()).unwrap_or(0.);
                    let position = point(px(number()), px(number()));
                    let turned = number();
                    let held = arg.rsplit(';').next().unwrap_or("");
                    let modifiers = Modifiers { platform: held == "cmd", control: held == "ctrl", shift: held == "shift", ..Default::default() };
                    let count = match name { "click2" => 2, "click3" => 3, _ => 1 };
                    let events = match name {
                        "press" => vec![PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position, modifiers, click_count: 1, first_mouse: false })],
                        "move" => vec![PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(MouseButton::Left), modifiers })],
                        "hover" => vec![PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers })],
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
                // `scopefind:xylem` searches in the panel over the library's conversation, as if typed; `pin:2` keeps the next conversation to the third paper it lists (again lets it go).
                "scopefind" => {
                    if let Some(lib) = self.lib.as_ref() { lib.scope_find.update(cx, |input, cx| input.set_text(arg, cx)) }
                    self.scope_typed(arg.to_string(), cx);
                    None
                }
                "pin" => self.lib.as_ref().and_then(|lib| {
                    let id = lib.scope_listed().get(arg.parse::<usize>().unwrap_or(0))?.id.clone();
                    Some(Cmd::Scope(if lib.asked.contains(&id) { ScopeDo::Unpin(id) } else { ScopeDo::Pin(id) }))
                }),
                // `review:requirements|framework|outline|section|checks` takes the last answer for that step, `review:confirm` confirms what the step waits for, `review:skip` goes on without a framework,
                // `review:record` shows the record or the conversation, `review:judge` says the first row of evidence is borne out, `review:manuscript` makes the manuscript.
                "review" => self.lib.as_ref().and_then(|lib| {
                    let answer = lib.library_chat.messages.iter().rev().find_map(|item| if let ChatItem::Answer(text) = item { Some(text.clone()) } else { None }).unwrap_or_default();
                    let review = lib.library_chat.review.as_ref();
                    match arg {
                        "requirements" => Some(Cmd::Review(ReviewDo::Requirements, answer)),
                        "framework" => Some(Cmd::Review(ReviewDo::Framework, answer)),
                        "skip" => Some(Cmd::Review(ReviewDo::SkipFramework, String::new())),
                        "outline" => Some(Cmd::Review(ReviewDo::Outline, answer)),
                        "section" => Some(Cmd::Review(ReviewDo::Section, answer)),
                        "checks" => Some(Cmd::Review(ReviewDo::Checks, answer)),
                        "confirm" => Some(Cmd::Review(match review.map_or(1, |review| review.step()) { 1 => ReviewDo::ConfirmRequirements, 2 => ReviewDo::ConfirmFramework, _ => ReviewDo::ConfirmOutline }, String::new())),
                        "complete" => Some(Cmd::Review(ReviewDo::Complete, String::new())),
                        "record" => Some(Cmd::ToggleReview),
                        "judge" => review.and_then(|review| review.evidence_rows.first()).map(|row| Cmd::JudgeReviewEvidence(row.id.clone(), wisp_core::review::EvidenceJudgment::Supports)),
                        "evidence" => Some(Cmd::EditReviewEvidence(None, String::new())),
                        "edit" => Some(Cmd::EditReviewText(ReviewText::Requirements)),
                        "manuscript" => Some(Cmd::ReviewToManuscript),
                        _ => Some(Cmd::StartReview),
                    }
                }),
                "history-paper" => Some(Cmd::ToggleHistory(Talk::Paper)),
                "newchat" => Some(Cmd::NewChat(match arg { "paper" => Talk::Paper, "write" => Talk::Write, _ => Talk::Library })),
                "note" => {
                    let Some(answer) = self.lib.as_ref().and_then(|lib| lib.paper_chat.messages.iter().rev().find_map(|item| if let ChatItem::Answer(text) = item { Some(text.clone()) } else { None })) else { return };
                    Some(Cmd::AnswerToNote("（脚本）".into(), answer))
                }
                "exportnote" => Some(Cmd::NoteOpen(match arg { "paper" => Some(Talk::Paper), "library" => Some(Talk::Library), _ => None }, true)),
                "notepreview" => Some(Cmd::NotePreview),
                "notedestination" => Some(Cmd::NoteDestination(arg != "local")),
                "notepick" => arg.parse().ok().map(Cmd::NotePick),
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
                // `asklist:240` is the width of the list beside a paper that is asked about (`tab:ask`).
                "asklist" => {
                    self.settings.ask_list_width = arg.parse().unwrap_or(0.);
                    cx.notify();
                    return;
                }
                // `column:journal=240` gives a column of the paper table a width (`year`, `fulltext`, `reading`, `screening`, `field:<id>`).
                "column" => {
                    if let Some((key, width)) = arg.split_once('=') { self.settings.column_widths.insert(key.into(), width.parse().unwrap_or(0.)); }
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
                // `screen` starts the assistant on the pending papers, `screen:decided` on the decided ones, `screen:included` on the included ones;
                // `screen:unincluded` asks to move the decided papers not included to the trash.
                // `screen:single`, `screen:confirm` and `screen:batch` are the page's three faces; `screen:go;<n>` the n-th paper of a round.
                "screen" => Some(match arg {
                    "decided" => Cmd::ScreenRecheck(false),
                    "included" => Cmd::ScreenRecheck(true),
                    "unincluded" => Cmd::AskTrashUnincluded,
                    "single" => Cmd::ScreenMode(ScreenMode::Single),
                    "confirm" => Cmd::ScreenMode(ScreenMode::Confirm),
                    "batch" => Cmd::ScreenMode(ScreenMode::Batch),
                    "apply" => Cmd::ConfirmApply,
                    "unconfirm" => Cmd::ConfirmUndo,
                    go if go.starts_with("go;") => Cmd::ScreenGo(go[3..].parse().unwrap_or(0)),
                    _ => Cmd::ScreenStart,
                }),
                // 提取: `extract` is the dialog for the list on show (`extract:marked` for the marked papers), `submit` starts it;
                // the rest are the steps of `Workspace::extract_dev` (extract.rs).
                "extract" => self.extract_dev(arg),
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
                "assist" => Some(Cmd::WriteAssist(match arg { "rewrite" => ai::Assist::Rewrite, "translate" => ai::Assist::Translate, "polish" => ai::Assist::Polish, "cite" => ai::Assist::Cite, own => ai::Assist::Own(own.to_string()) })),
                "assiststop" => Some(Cmd::WriteAssistStop),
                // `cited:tick;<sentence>;<paper>` ticks a paper 配引用 found (by their places from 0), `cited:more;<sentence>` shows its other papers,
                // `cited:rest` the sentences nothing was chosen for, `cited:insert` writes the ticked citations.
                "cited" => {
                    use crate::write_cite::Do;
                    let mut parts = arg.split(';');
                    let what = parts.next().unwrap_or("");
                    let mut number = || parts.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
                    Some(Cmd::WriteCited(match what {
                        "tick" => Do::Tick(number(), number()),
                        "more" => Do::Unfold(number()),
                        "rest" => Do::Rest,
                        _ => Do::Insert,
                    }))
                }
                // `cite:<n>` is 引用 on the n-th candidate (from 0): the paper cited at the caret, the sentence kept as what the citation rests on.
                "cite" => self.lib.as_ref().and_then(|lib| {
                    let found = lib.writing.search.candidates.as_ref()?.as_ref().ok()?;
                    let sentence = found["sentences"].get(arg.parse::<usize>().unwrap_or(0))?;
                    Some(Cmd::WriteCiteFrom(sentence["paperId"].as_str()?.to_string(), wisp_core::passage::Candidate { text: sentence["text"].as_str()?.to_string(), part: found["section"].as_str().unwrap_or("").to_string(), page: sentence["page"].as_u64().unwrap_or(0) as usize }))
                }),
                // `source` looks for the passages of the citations that have none, `source:stop` stops that;
                // `source:go;<n>` shows the n-th passage kept (from 0) in its paper, `source:drop;<n>` lets go of it.
                "source" => {
                    use crate::write_source::Do;
                    let (what, n) = arg.split_once(';').map_or((arg, 0), |(what, n)| (what, n.parse::<usize>().unwrap_or(0)));
                    let nth = self.lib.as_ref().and_then(|lib| lib.citing.sources.kept.get(n)).cloned();
                    match (what, nth) {
                        ("stop", _) => Some(Cmd::Source(Do::Stop)),
                        ("go", Some(passage)) => Some(Cmd::GotoSource(passage.paper_id.clone(), (passage.page > 0).then_some(passage.page), passage.passage.clone())),
                        ("drop", Some(passage)) => Some(Cmd::Source(Do::Drop(passage.id.clone()))),
                        ("go" | "drop", None) => None,
                        _ => Some(Cmd::Source(Do::Find(None))),
                    }
                }
                // `revise:<wish>` tells the assistant what to change in the section, as if typed into the box under the manuscript and sent;
                // `revise:stop`, `revise:keep`, `revise:undo` (all) / `revise:undo;<n>`, `revise:go;<n>`, `revise:next` / `revise:prev` are its buttons.
                "revise" => {
                    use crate::revise::Do;
                    let (what, n) = arg.split_once(';').map_or((arg, 0), |(what, n)| (what, n.parse::<usize>().unwrap_or(0)));
                    Some(Cmd::Revise(match (what, arg.contains(';')) {
                        ("stop", _) => Do::Stop,
                        ("keep", _) => Do::Keep,
                        ("undo", false) => Do::UndoAll,
                        ("undo", true) => Do::Undo(n),
                        ("go", _) => Do::Go(n),
                        ("next", _) => Do::Step(true),
                        ("prev", _) => Do::Step(false),
                        _ => {
                            if let Some(lib) = self.lib.as_ref() { lib.writing.revise.input.update(cx, |input, cx| input.set_text(arg, cx)) }
                            Do::Send
                        }
                    }))
                }
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
                    let profile = arg.parse::<usize>().ok().and_then(|n| self.settings.profiles().into_iter().nth(n)).map(|p| p.id);
                    if let Some(pdf) = self.lib.as_ref().and_then(|l| l.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.translate(profile, cx)) }
                    return;
                }
                // `try:0` runs the first saved search as a trial.
                "try" => self.lib.as_ref().and_then(|l| l.searches.get(arg.parse::<usize>().unwrap_or(0)).map(|s| Cmd::TrySearch(s.id.clone()))),
                "wide" => Some(Cmd::ToggleWide),
                "cut" => Some(Cmd::PdfCut),
                // `pdftool:box` / `pdftool:note` draw boxes / place a note on the PDF on show; again, or `pdftool:off`, selects text.
                "pdftool" => Some(Cmd::PdfTool(match arg { "box" => crate::pdf::Tool::Box, "note" => crate::pdf::Tool::Note, _ => crate::pdf::Tool::Select })),
                // `readfile:<n>` reads the n-th PDF of the selected paper (0 its main text, then its attachments) in the viewer.
                "readfile" => self.lib.as_ref().and_then(|lib| {
                    let paper = lib.selected.as_deref()?;
                    let files: Vec<String> = lib.main_pdf(paper).into_iter().chain(lib.attachments(paper)).map(|f| f.id.clone()).collect();
                    files.get(arg.parse::<usize>().unwrap_or(0)).cloned().map(Cmd::ReadFile)
                }),
                // `pdfcut:<page>;<x0>;<y0>;<x1>;<y1>` marks a piece of a 1-based page of the PDF on show as cut out, in the page's
                // points (after a `shot:` has drawn the page); `collect` hands it to the figure library, as 收藏到图片库 does.
                "pdfcut" => {
                    let numbers: Vec<f32> = arg.split(';').filter_map(|n| n.parse().ok()).collect();
                    if let (Some(pdf), [page, x0, y0, x1, y1]) = (self.lib.as_ref().and_then(|l| l.pdf.clone()), numbers.as_slice()) { pdf.update(cx, |pdf, cx| pdf.set_cut((*page as usize).saturating_sub(1), [*x0, *y0, *x1, *y1], cx)) }
                    return;
                }
                "collect" => {
                    if let Some(pdf) = self.lib.as_ref().and_then(|l| l.pdf.clone()) { pdf.update(cx, |pdf, cx| pdf.collect_cut(cx)) }
                    return;
                }
                // `figure:<n>` opens the n-th figure of the figure library, `figure:source:<n>` the place it was cut from;
                // `figtag:<tag>` narrows the figures to a tag, `figfind:<words>` searches them.
                "figure" => {
                    let (source, n) = arg.strip_prefix("source:").map_or((false, arg), |n| (true, n));
                    self.lib.as_ref().and_then(|l| l.gallery.items.get(n.parse::<usize>().unwrap_or(0))).map(|figure| if source { Cmd::FigureSource(figure.id.clone()) } else { Cmd::OpenFigure(figure.id.clone()) })
                }
                "figtag" => Some(Cmd::FigureTag(arg.to_string())),
                "figfind" => {
                    if let Some(lib) = self.lib.as_ref() { lib.gallery.search.update(cx, |input, cx| input.insert(arg, cx)) }
                    return;
                }
                // `figinfer:legend|tags|notes` has the model write that field of the figure's dialog, as its sparkle does.
                "figinfer" => Some(Cmd::FigureInfer(match arg { "tags" => wisp_core::figure::Ask::Tags, "notes" => wisp_core::figure::Ask::Notes, _ => wisp_core::figure::Ask::Legend })),
                // `figdraw` opens the dialog a picture is drawn in, `figdraw:go` has the model draw (again stops it);
                // `figprompt:<text>` leaves the description in its box, `figshape:wide|tall|square` picks the shape.
                "figdraw" => Some(if arg == "go" { Cmd::FigureDraw } else { Cmd::DrawFigure }),
                "figshape" => Some(Cmd::FigureShape(match arg { "wide" => wisp_core::imagegen::Shape::Wide, "tall" => wisp_core::imagegen::Shape::Tall, _ => wisp_core::imagegen::Shape::Square })),
                "figprompt" => {
                    if let Some(prompt) = self.modal.as_ref().and_then(|modal| match modal { Modal::Figure(form) => form.draw.as_ref().map(|draw| draw.prompt.clone()), _ => None }) {
                        prompt.update(cx, |input, cx| input.set_text(arg, cx));
                    }
                    return;
                }
                // `figlabel:` / `figlegend:` / `figtags:` / `fignotes:` leave text in a field of the figure's dialog, as if typed.
                "figlabel" | "figlegend" | "figtags" | "fignotes" => {
                    if let Some(Modal::Figure(form)) = self.modal.as_ref() {
                        let field = match name { "figlabel" => &form.label, "figlegend" => &form.legend, "figtags" => &form.tags, _ => &form.notes };
                        field.update(cx, |input, cx| input.set_text(arg, cx));
                    }
                    return;
                }
                "beside" => Some(Cmd::BesidePdf(arg != "off")),
                // `swap` puts the paper's PDF and its conversation at each other's side.
                "swap" => Some(Cmd::SwapPanes),
                // `pdftools` / `pdftools:bar`: the PDF's tools floating at the page's right edge, or back in its toolbar.
                "pdftools" => Some(Cmd::FloatPdfTools(arg != "bar")),
                // `float` / `float:off`: the paper's conversation over its PDF, or beside it; `fold` / `fold:off` folds it up or opens it.
                "float" => Some(Cmd::FloatChat(arg != "off")),
                "fold" => Some(Cmd::FoldChat(arg != "off")),
                // `pdfsplit:side`, `pdfsplit:stacked`, `pdfsplit:off`: the PDF on show twice, or once again.
                "pdfsplit" => Some(Cmd::PdfSplit(match arg {
                    "side" => Split::Side,
                    "stacked" => Split::Stacked,
                    _ => Split::Off,
                })),
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
                // `profile:1` shows the second provider on 设置 › AI › 模型.
                "profile" => Some(Cmd::SelectProvider(arg.parse().unwrap_or(0))),
                // `palette` opens the command palette; `palette:<text>` with that typed into it.
                "palette" => {
                    if self.palette.is_none() { self.toggle_palette(window, cx) }
                    if let Some(input) = self.palette.as_ref().map(|palette| palette.input.clone()) { input.update(cx, |input, cx| input.set_text(arg, cx)) }
                    self.palette_event(InputEvent::Changed, window, cx);
                    return;
                }
                "menu" => {
                    let kind = match arg { "sort" => MenuKind::Sort, "columns" => MenuKind::Columns, "year" => MenuKind::Filter(FilterKey::Year), "zoom" => MenuKind::PdfZoom, "pdfsplit" => MenuKind::PdfSplit, "fulltext" => MenuKind::Filter(FilterKey::Fulltext), "manuscripts" => MenuKind::Manuscripts, "export" => MenuKind::ManuscriptExport, "retry" => MenuKind::ExtractRetry, _ => MenuKind::Add };
                    self.open_menu(kind, point(px(if matches!(arg, "zoom" | "manuscripts" | "export" | "retry") { 900. } else { 20. }), px(120.)), cx);
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
                    Some(Cmd::Ask(Talk::Library))
                }
                // The library's conversation, by places from 0: `retract:<n>` takes back the n-th question, `unretract` puts it back,
                // `regenerate` asks for the last answer again, `fork:<n>` branches after the n-th answer.
                "retract" | "regenerate" | "fork" | "unretract" => self.lib.as_ref().and_then(|lib| {
                    let messages = &lib.library_chat.messages;
                    let n: usize = arg.parse().unwrap_or(0);
                    // An answer ends where the next question (or the conversation) begins.
                    let answer_ends: Vec<usize> = (0..messages.len()).filter(|&i| matches!(messages[i], ChatItem::Answer(_)) && !matches!(messages.get(i + 1), Some(ChatItem::Answer(_) | ChatItem::Tool { .. }))).collect();
                    match name {
                        "retract" => messages.iter().enumerate().filter(|(_, item)| matches!(item, ChatItem::User { .. })).nth(n).map(|(at, _)| Cmd::Retract(Talk::Library, at)),
                        "regenerate" => Some(Cmd::Regenerate(Talk::Library, messages.len().checked_sub(1)?)),
                        "fork" => answer_ends.get(n).map(|&at| Cmd::Fork(Talk::Library, at)),
                        _ => Some(Cmd::Unretract(Talk::Library)),
                    }
                }),
                // `agent` / `agent:off` is 写作's other mode, where the assistant writes; `agent:earlier|later|now|restore` steps through the versions of the section on show.
                "agent" => Some(Cmd::Agent(match arg {
                    "off" => crate::write_agent::Do::Mode(false),
                    "earlier" => crate::write_agent::Do::Step(false),
                    "later" => crate::write_agent::Do::Step(true),
                    "now" => crate::write_agent::Do::Now,
                    "restore" => crate::write_agent::Do::Restore,
                    _ => crate::write_agent::Do::Mode(true),
                })),
                // `askwrite:<what to write>` says it to the assistant that writes the manuscript on show.
                "askwrite" => {
                    self.agent_attend(cx);
                    if let Some(lib) = self.lib.as_ref() { lib.write_chat.input.update(cx, |input, cx| input.set_text(arg, cx)) }
                    Some(Cmd::Ask(Talk::Write))
                }
                "history-write" => Some(Cmd::ToggleHistory(Talk::Write)),
                // The same of the selected paper.
                "askpaper" => {
                    if let Some(lib) = self.lib.as_ref() { lib.paper_chat.input.update(cx, |input, cx| input.set_text(arg, cx)) }
                    Some(Cmd::Ask(Talk::Paper))
                }
                // `type:/se` leaves text in the library's question box, as if typed (the slash commands show).
                "type" => {
                    if let Some(lib) = self.lib.as_ref() {
                        lib.library_chat.input.update(cx, |input, cx| input.set_text(arg, cx));
                        lib.library_chat.input.read(cx).focus(window);
                    }
                    self.chat_input(Talk::Library, InputEvent::Changed, cx);
                    None
                }
                // `image:/path/to.png` puts a picture with the next question to the library.
                "image" => {
                    self.attach_images(Talk::Library, vec![Picture::File(arg.into())], cx);
                    None
                }
                "history" => Some(Cmd::ToggleHistory(Talk::Library)),
                "settings" => Some(match arg {
                    "ai" | "models" => Cmd::AiTab(AiTab::Models),
                    "tasks" => Cmd::AiTab(AiTab::Tasks),
                    "commands" => Cmd::AiTab(AiTab::Commands),
                    "customize" => Cmd::OpenSettings(SettingsPage::Customize),
                    _ => Cmd::OpenSettings(match arg {
                        "appearance" => SettingsPage::Appearance,
                        "shortcuts" => SettingsPage::Shortcuts,
                        "network" => SettingsPage::Network,
                        "about" => SettingsPage::About,
                        _ => SettingsPage::General,
                    }),
                }),
                "candidate" => Some(Cmd::CompleteChoose(arg.parse().ok())),
                // `command:0` opens the first of the person's own commands on 设置 › 命令.
                "command" => Some(Cmd::SelectCommand(arg.parse().unwrap_or(0))),
                // `consent` / `consent:always` / `consent:deny` answers the card that asks about the assistant's tools.
                "consent" => Some(Cmd::AnswerTools(Talk::Library, match arg { "always" => Consent::Always, "deny" => Consent::Deny, _ => Consent::Once })),
                // `connector:0` opens the first connector on 设置 › 自定义 (after `settings:customize`).
                "connector" => Some(Cmd::Custom(crate::custom::Do::SelectConnector(arg.parse().unwrap_or(0)))),
                // `signin:0` signs in to the first connector in the browser (a scripted run prints the address instead of opening it); `signout:0` signs out.
                "signin" => Some(Cmd::Custom(crate::custom::Do::SignIn(arg.parse().unwrap_or(0)))),
                "signout" => Some(Cmd::Custom(crate::custom::Do::SignOut(arg.parse().unwrap_or(0)))),
                // Saved like the choice in 设置 › 外观: "light", "dark", or empty for the system's.
                "theme" => Some(Cmd::SetAppearance(arg.to_string())),
                // Typography is saved just as through the appearance controls.
                "fontsize" => arg.parse().ok().map(Cmd::SetFontSize),
                // `fontpicker:cjk` opens the Chinese font's picker, anything else the Latin one's.
                "fontpicker" => {
                    self.toggle_font_picker(if arg == "cjk" { crate::settings::FontSlot::Cjk } else { crate::settings::FontSlot::Latin }, window, cx);
                    return;
                }
                "fontsearch" => {
                    self.search_settings_fonts(arg, cx);
                    return;
                }
                "fontfamily" => Some(Cmd::SetFontFamily(crate::settings::FontSlot::Latin, arg.to_string())),
                "cjkfont" => Some(Cmd::SetFontFamily(crate::settings::FontSlot::Cjk, arg.to_string())),
                "fontreset" => Some(Cmd::ResetTypography),
                "linespacing" => Some(Cmd::SetLineSpacing(match arg { "compact" => wisp_core::settings::Spacing::Compact, "relaxed" => wisp_core::settings::Spacing::Relaxed, _ => wisp_core::settings::Spacing::Standard })),
                "paragraphspacing" => Some(Cmd::SetParagraphSpacing(match arg { "compact" => wisp_core::settings::Spacing::Compact, "relaxed" => wisp_core::settings::Spacing::Relaxed, _ => wisp_core::settings::Spacing::Standard })),
                "textwidth" => Some(Cmd::SetTextWidth(match arg { "narrow" => wisp_core::settings::TextWidth::Narrow, "wide" => wisp_core::settings::TextWidth::Wide, _ => wisp_core::settings::TextWidth::Standard })),
                "density" => Some(Cmd::SetDensity(arg.to_string())),
                "contrast" => Some(Cmd::ToggleHighContrast),
                "appearancereset" => Some(Cmd::ResetAppearance),
                "language" => Some(Cmd::SetLanguage(if arg == "en" { Language::En } else { Language::Zh })),
                "record" => Some(Cmd::RecordShortcut(arg.to_string())),
                "update" => Some(Cmd::CheckUpdate),
                // The window's close button: put away while 设置 › 通用 › 后台 is on, else closed (and the app quits).
                "close" => {
                    if crate::background::close(self.settings.keep_running, window, cx) { window.remove_window() }
                    return;
                }
                // `addid:<DOI or PMID>` opens 通过 DOI / PMID 添加 with that typed; `submit` then looks it up.
                "addid" => {
                    self.run(Cmd::AddById, window, cx);
                    if let Some(Modal::AddById { input, .. }) = &self.modal { input.update(cx, |input, cx| input.set_text(arg, cx)) }
                    None
                }
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
