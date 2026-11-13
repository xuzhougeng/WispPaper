//! settings.json: where the libraries live, the model profiles, and the
//! choices made on the settings pages.

use crate::ai::{Command, Profile};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The desktop interface language. Unknown values fall back to Chinese,
/// as do settings files written before language switching was available.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "en")]
    En,
    #[default]
    #[serde(rename = "zh", other)]
    Zh,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// The folder holding every library; empty means the default.
    pub data_dir: String,
    /// The model services this person can reach.
    pub profiles: Vec<Profile>,
    /// Which of them each task uses.
    pub tasks: Tasks,
    /// How many papers the screening assistant reads at the same time, 1 to `MAX_SCREEN_WORKERS`.
    pub screen_workers: usize,
    /// The instructions translations are made with; empty means `ai::TRANSLATION_PROMPT`.
    pub translation_prompt: String,
    /// The slash commands the person wrote: prompt templates under a name (`ai::read_input`).
    pub commands: Vec<Command>,
    /// Look for a newer release after launch and twice a day.
    pub auto_update: bool,
    /// "light" or "dark"; empty follows the system.
    pub appearance: String,
    /// The interface language; independent of manuscript and translation languages.
    pub language: Language,
    /// "compact" tightens the paper table; empty is the standard rows.
    pub density: String,
    /// The proxy every request goes through, as a URL (`http://127.0.0.1:7890`,
    /// `socks5://…`); empty leaves it to the environment (`HTTPS_PROXY` and the like).
    pub proxy: String,
    /// Changed shortcuts by id; an empty key means the shortcut is removed.
    pub shortcuts: BTreeMap<String, String>,
    /// How wide the reader made the detail panel, in logical pixels; 0 is the width it comes with.
    pub detail_width: f32,
    /// How wide the reader made a library's sidebar; 0 is the width it comes with.
    pub sidebar_width: f32,
    /// The sidebar is put away: the list and the paper have the whole window.
    pub sidebar_hidden: bool,
    /// How wide the reader made a paper's conversation beside its PDF; 0 is the width it comes with.
    pub chat_width: f32,
    /// How wide the reader made the list of papers beside a paper that is asked about; 0 is the width it comes with.
    pub ask_list_width: f32,
    /// The paper's conversation over its PDF instead of beside it, and where its window was left.
    pub floating: crate::floating::Floating,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { data_dir: String::new(), profiles: vec![], tasks: Tasks::default(), screen_workers: 3, translation_prompt: String::new(), commands: vec![], auto_update: true, appearance: String::new(), language: Language::default(), density: String::new(), proxy: String::new(), shortcuts: BTreeMap::new(), detail_width: 0., sidebar_width: 0., sidebar_hidden: false, chat_width: 0., ask_list_width: 0., floating: Default::default() }
    }
}

pub const MAX_SCREEN_WORKERS: usize = 8;

/// What a model is used for. Each task can have a model of its own, so a
/// cheap one that does not think can translate while a stronger one answers
/// questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// Questions to the library or a paper, and the guide's search queries.
    Chat,
    /// The assistant that reads the papers waiting for a decision, several at a time.
    Screen,
    Translate,
    /// 写作's groundwork: sorting a full text into the parts of a paper, reading the pages of a scan.
    Structure,
}

impl Task {
    pub const ALL: [Task; 4] = [Task::Chat, Task::Screen, Task::Translate, Task::Structure];

    pub fn label(self) -> &'static str {
        match self {
            Task::Chat => "助手",
            Task::Screen => "批量筛选",
            Task::Translate => "翻译",
            Task::Structure => "整理全文",
        }
    }
}

/// The profile id each task is bound to. An empty one follows the model
/// for questions, and that one the first profile, so a single model works
/// everywhere until the person decides otherwise.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tasks {
    pub chat: String,
    pub screen: String,
    pub translate: String,
    pub structure: String,
}

impl Tasks {
    pub fn get(&self, task: Task) -> &str {
        match task {
            Task::Chat => &self.chat,
            Task::Screen => &self.screen,
            Task::Translate => &self.translate,
            Task::Structure => &self.structure,
        }
    }

    pub fn set(&mut self, task: Task, id: &str) {
        *match task {
            Task::Chat => &mut self.chat,
            Task::Screen => &mut self.screen,
            Task::Translate => &mut self.translate,
            Task::Structure => &mut self.structure,
        } = id.to_string();
    }
}

/// `WISP_PAPER_HOME` keeps the data and the settings of tests and
/// side-by-side builds in one folder of their own.
fn home_override() -> Option<PathBuf> {
    std::env::var("WISP_PAPER_HOME").ok().filter(|home| !home.trim().is_empty()).map(PathBuf::from)
}

/// Where the libraries live unless the settings name another folder.
pub fn default_root() -> PathBuf {
    home_override().unwrap_or_else(|| dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("WispPaper"))
}

pub fn settings_file() -> PathBuf {
    match home_override() {
        Some(home) => home.join("settings.json"),
        None => dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("WispPaper").join("settings.json"),
    }
}

/// The schemes a proxy address may have.
pub const PROXY_SCHEMES: [&str; 6] = ["http", "https", "socks4", "socks4a", "socks5", "socks5h"];

/// What is wrong with a proxy address, if anything. Empty is fine: it means
/// "not set". `direct` (no proxy at all) is only meaningful for one model
/// service, so it is accepted only where `allow_direct` says so.
pub fn proxy_problem(value: &str, allow_direct: bool) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || (allow_direct && value.eq_ignore_ascii_case("direct")) { return None }
    let Some((scheme, rest)) = value.split_once("://") else { return Some("代理地址要写完整，例如 http://127.0.0.1:7890 或 socks5://127.0.0.1:1080。".into()) };
    if !PROXY_SCHEMES.contains(&scheme.to_lowercase().as_str()) { return Some(format!("不支持 {scheme}:// 代理。可用：{}。", PROXY_SCHEMES.join("、"))) }
    // What stands after any `user:password@`, up to the port or a path.
    let host = rest.rsplit('@').next().unwrap_or("").split(['/', ':']).next().unwrap_or("");
    if host.is_empty() || value.contains(char::is_whitespace) { return Some("代理地址里缺少主机名，或者含有空格。".into()) }
    None
}

impl Settings {
    /// A missing file is the defaults; a broken one is an error, so it is
    /// never silently overwritten with defaults.
    pub fn load(path: &Path) -> Result<Settings> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let value: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("设置文件格式不正确：{}", path.display()))?;
                // Before tasks had models of their own, one index said which profile questions go to.
                let active = value["activeProfile"].as_u64().map(|index| index as usize);
                let mut settings: Settings = serde_json::from_value(value).with_context(|| format!("设置文件格式不正确：{}", path.display()))?;
                settings.tidy();
                if let Some(profile) = active.filter(|_| settings.tasks.chat.is_empty()).and_then(|index| settings.profiles.get(index)) { settings.tasks.chat = profile.id.clone() }
                Ok(settings)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(err) => Err(err).with_context(|| format!("无法读取设置文件 {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)? }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&temporary, path).with_context(|| format!("无法写入设置文件 {}", path.display()))?;
        Ok(())
    }

    /// Make what was read usable: every profile gets an id, a task bound to
    /// a profile that is gone follows the default again, and the number of
    /// screening workers stays in range.
    fn tidy(&mut self) {
        for index in 0..self.profiles.len() {
            if self.profiles[index].id.trim().is_empty() { self.profiles[index].id = self.free_id() }
        }
        for task in Task::ALL {
            if !self.profiles.iter().any(|p| p.id == self.tasks.get(task)) { self.tasks.set(task, "") }
        }
        self.screen_workers = self.screen_workers.clamp(1, MAX_SCREEN_WORKERS);
        if !self.detail_width.is_finite() || self.detail_width < 0. { self.detail_width = 0. }
        if !self.sidebar_width.is_finite() || self.sidebar_width < 0. { self.sidebar_width = 0. }
        if !self.chat_width.is_finite() || self.chat_width < 0. { self.chat_width = 0. }
        if !self.ask_list_width.is_finite() || self.ask_list_width < 0. { self.ask_list_width = 0. }
    }

    /// The first of `m1`, `m2`, … no profile has yet.
    fn free_id(&self) -> String {
        (1..).map(|n| format!("m{n}")).find(|id| !self.profiles.iter().any(|p| p.id == *id)).unwrap_or_default()
    }

    /// The profile a task uses: its own, else the one for questions, else the first.
    pub fn profile_for(&self, task: Task) -> Option<&Profile> {
        let bound = |id: &str| self.profiles.iter().find(|p| !id.is_empty() && p.id == id);
        bound(self.tasks.get(task)).or_else(|| bound(&self.tasks.chat)).or(self.profiles.first())
    }

    /// The profile a task uses; before one is saved, the default one,
    /// which works as soon as its API key is in the environment.
    pub fn profile_for_or_default(&self, task: Task) -> Profile {
        self.profile_for(task).cloned().unwrap_or_default()
    }

    /// Add a model service and return its place in `profiles`.
    pub fn add_profile(&mut self, mut profile: Profile) -> usize {
        profile.id = self.free_id();
        self.profiles.push(profile);
        self.profiles.len() - 1
    }

    /// Take a model service away; the tasks it served follow the default again.
    pub fn remove_profile(&mut self, index: usize) {
        if index >= self.profiles.len() { return }
        self.profiles.remove(index);
        self.tidy();
    }

    /// One more command of the person's own, under a name nothing has yet, and its place in `commands`.
    pub fn add_command(&mut self) -> usize {
        let name = (1..).map(|n| format!("command-{n}")).find(|name| !self.commands.iter().any(|command| command.name == *name)).unwrap_or_default();
        self.commands.push(Command { name, ..Default::default() });
        self.commands.len() - 1
    }

    /// The folder holding the libraries: the configured one, else `default`.
    pub fn data_root(&self, default: &Path) -> PathBuf {
        if self.data_dir.trim().is_empty() { default.to_path_buf() } else { PathBuf::from(self.data_dir.trim()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_language_is_backward_compatible_and_persisted() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.language, Language::Zh);
        let english: Settings = serde_json::from_str(r#"{"language":"en"}"#).unwrap();
        assert_eq!(english.language, Language::En);
        let saved = serde_json::to_string(&english).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&saved).unwrap().language, Language::En);
        let unsupported: Settings = serde_json::from_str(r#"{"language":"fr"}"#).unwrap();
        assert_eq!(unsupported.language, Language::Zh);
        assert!(serde_json::from_str::<Settings>(r#"{"language":42}"#).is_err());
    }

    #[test]
    fn load_save() {
        let dir = std::env::temp_dir().join(format!("wisp-settings-{}", crate::model::new_id("T")));
        let path = dir.join("settings.json");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let mut s = Settings::default();
        s.add_profile(Profile::default());
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        std::fs::write(&path, "{broken").unwrap();
        assert!(Settings::load(&path).is_err());
        // A file from before a setting existed takes that setting's default.
        std::fs::write(&path, r#"{"dataDir":"/x","shortcuts":{"search":""}}"#).unwrap();
        let old = Settings::load(&path).unwrap();
        assert!(old.auto_update && old.appearance.is_empty());
        assert_eq!((old.data_dir.as_str(), old.shortcuts.get("search").map(String::as_str)), ("/x", Some("")));
        assert_eq!(s.data_root(Path::new("/d")), PathBuf::from("/d"));
        assert!(old.proxy.is_empty() && old.commands.is_empty());
        // Commands of one's own are kept as written, each new one under a free name.
        let mut with = Settings::default();
        assert_eq!((with.add_command(), with.add_command()), (0, 1));
        with.commands[0].template = "检查方法：{}".into();
        with.commands.remove(0);
        assert_eq!(with.add_command(), 1);
        assert_eq!(with.commands.iter().map(|command| command.name.as_str()).collect::<Vec<_>>(), ["command-2", "command-1"]);
        with.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), with);
        // A profile saved before proxies existed has none of its own.
        std::fs::write(&path, r#"{"proxy":"socks5://127.0.0.1:1080","profiles":[{"name":"x","protocol":"openai","baseUrl":"http://h/v1","apiKey":"","model":"m"}]}"#).unwrap();
        let with = Settings::load(&path).unwrap();
        assert_eq!((with.proxy.as_str(), with.profiles[0].proxy.as_str()), ("socks5://127.0.0.1:1080", ""));
        // Nor an id, a thinking setting or a task: it is given an id and serves everything.
        assert_eq!((with.profiles[0].id.as_str(), with.profiles[0].effort.as_str(), with.screen_workers), ("m1", "", 3));
        assert_eq!(with.profile_for(Task::Translate).map(|p| p.name.as_str()), Some("x"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn models_per_task() {
        let mut s = Settings::default();
        assert_eq!(s.profile_for(Task::Chat), None);
        assert_eq!(s.profile_for_or_default(Task::Screen), Profile::default());
        let named = |name: &str| Profile { name: name.into(), ..Default::default() };
        let (strong, cheap) = (s.add_profile(named("strong")), s.add_profile(named("cheap")));
        assert_eq!((s.profiles[strong].id.as_str(), s.profiles[cheap].id.as_str()), ("m1", "m2"));
        let uses = |s: &Settings, task: Task| s.profile_for(task).unwrap().name.clone();
        // Unbound, everything is the first profile; screening and translation follow the one for questions.
        assert_eq!(Task::ALL.map(|task| uses(&s, task)), ["strong", "strong", "strong", "strong"]);
        s.tasks.set(Task::Chat, "m2");
        assert_eq!(Task::ALL.map(|task| uses(&s, task)), ["cheap", "cheap", "cheap", "cheap"]);
        s.tasks.set(Task::Chat, "");
        s.tasks.set(Task::Translate, "m2");
        assert_eq!(Task::ALL.map(|task| uses(&s, task)), ["strong", "strong", "cheap", "strong"]);
        // A removed profile takes its bindings along, and its id is not handed out twice while others keep theirs.
        s.remove_profile(strong);
        assert_eq!((s.profiles.len(), s.tasks.translate.as_str(), uses(&s, Task::Chat).as_str()), (1, "m2", "cheap"));
        assert_eq!(s.add_profile(named("third")), 1);
        assert_eq!(s.profiles[1].id, "m1");
        s.remove_profile(0);
        assert_eq!((s.tasks.translate.as_str(), uses(&s, Task::Translate).as_str()), ("", "third"));

        // An older file's `activeProfile` becomes the model for questions; a binding to nothing is dropped.
        let dir = std::env::temp_dir().join(format!("wisp-settings-{}", crate::model::new_id("T")));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"activeProfile":1,"screenWorkers":40,"tasks":{"screen":"gone"},"profiles":[{"name":"a"},{"name":"b","effort":"off"}]}"#).unwrap();
        let old = Settings::load(&path).unwrap();
        assert_eq!((old.tasks.chat.as_str(), old.tasks.screen.as_str(), old.screen_workers), ("m2", "", MAX_SCREEN_WORKERS));
        assert_eq!(old.profile_for(Task::Screen).map(|p| (p.name.as_str(), p.effort.as_str())), Some(("b", "off")));
        old.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), old);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn proxy_addresses() {
        for good in ["", "  ", "http://127.0.0.1:7890", "HTTPS://proxy.example", "socks5://user:pass@10.0.0.1:1080", "socks5h://localhost:1080/"] {
            assert_eq!(proxy_problem(good, false), None, "{good}");
        }
        for bad in ["127.0.0.1:7890", "ftp://x", "http://", "http://:8080", "http://a b:1", "direct"] {
            assert!(proxy_problem(bad, false).is_some(), "{bad}");
        }
        assert_eq!(proxy_problem(" Direct ", true), None);
    }
}
