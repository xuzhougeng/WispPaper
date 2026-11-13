//! settings.json: where the libraries live, the model services, and the
//! choices made on the settings pages.

use crate::ai::{Command, Profile};
use crate::provider::{self, Model, Provider};
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
    /// The model services this person can reach, each with its models.
    pub providers: Vec<Provider>,
    /// What settings files before providers held: one service and one model each.
    /// Read only to become `providers` (`Settings::load`), never written.
    #[serde(rename = "profiles", skip_serializing)]
    legacy_profiles: Vec<Profile>,
    /// Which model (`provider::Model::id`) each task uses.
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
    /// Closing the window keeps the app running in the background (macOS: the Dock, Windows: the tray) instead of quitting.
    pub keep_running: bool,
    /// Whose comments and tracked changes a manuscript exported to Word carries; empty is the name this computer knows the person by.
    pub author: String,
}

impl Default for Settings {
    fn default() -> Self {
        let providers = provider::DEFAULT_PRESETS.iter().filter_map(|key| provider::preset(key)).map(Provider::from_preset).collect();
        let mut settings = Settings { data_dir: String::new(), providers, legacy_profiles: vec![], tasks: Tasks::default(), screen_workers: 3, translation_prompt: String::new(), commands: vec![], auto_update: true, appearance: String::new(), language: Language::default(), density: String::new(), proxy: String::new(), shortcuts: BTreeMap::new(), detail_width: 0., sidebar_width: 0., sidebar_hidden: false, chat_width: 0., ask_list_width: 0., floating: Default::default(), keep_running: true, author: String::new() };
        settings.tidy();
        settings
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

/// The model id each task is bound to. An empty one follows the model
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
    /// Whose name the person's comments and tracked changes carry: the one set, else the one this computer knows them by.
    pub fn author_name(&self) -> String {
        let named = self.author.trim();
        if !named.is_empty() { return named.to_string() }
        ["USER", "USERNAME"].iter().find_map(|key| std::env::var(key).ok()).map(|name| name.trim().to_string()).filter(|name| !name.is_empty()).unwrap_or_else(|| "作者".to_string())
    }

    /// A missing file is the defaults; a broken one is an error, so it is
    /// never silently overwritten with defaults.
    pub fn load(path: &Path) -> Result<Settings> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let value: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("设置文件格式不正确：{}", path.display()))?;
                // Before tasks had models of their own, one index said which profile questions go to.
                let active = value["activeProfile"].as_u64().map(|index| index as usize);
                let had_providers = value.get("providers").is_some();
                let mut settings: Settings = serde_json::from_value(value).with_context(|| format!("设置文件格式不正确：{}", path.display()))?;
                if !had_providers && !settings.legacy_profiles.is_empty() {
                    let legacy = std::mem::take(&mut settings.legacy_profiles);
                    let mut chat = active.and_then(|index| legacy.get(index)).map(|profile| profile.id.clone());
                    // A profile of an old file without an id is given one here, so that the binding above can find it.
                    let mut used: Vec<String> = legacy.iter().map(|profile| profile.id.trim().to_string()).filter(|id| !id.is_empty()).collect();
                    let legacy: Vec<Profile> = legacy.into_iter().enumerate().map(|(i, mut profile)| {
                        if profile.id.trim().is_empty() {
                            profile.id = (1..).map(|n| format!("m{n}")).find(|id| !used.contains(id)).unwrap_or_default();
                            used.push(profile.id.clone());
                            if active == Some(i) { chat = Some(profile.id.clone()) }
                        }
                        profile
                    }).collect();
                    settings.providers = migrate(legacy, std::mem::take(&mut settings.providers));
                    if let Some(id) = chat.filter(|_| settings.tasks.chat.is_empty()) { settings.tasks.chat = id }
                }
                settings.legacy_profiles.clear();
                settings.tidy();
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

    /// Make what was read usable: every provider and model gets an id of its
    /// own, a task bound to a model that is gone follows the default again,
    /// and the number of screening workers stays in range.
    fn tidy(&mut self) {
        let mut seen = std::collections::HashSet::new();
        for p in 0..self.providers.len() {
            let id = self.providers[p].id.trim().to_string();
            if id.is_empty() || !seen.insert(id) {
                let free = (1..).map(|n| format!("p{n}")).find(|id| !self.providers.iter().any(|other| other.id == *id)).unwrap_or_default();
                seen.insert(free.clone());
                self.providers[p].id = free;
            }
        }
        let mut seen = std::collections::HashSet::new();
        for p in 0..self.providers.len() {
            for m in 0..self.providers[p].models.len() {
                let id = self.providers[p].models[m].id.trim().to_string();
                if id.is_empty() || !seen.insert(id) {
                    let free = self.free_id();
                    seen.insert(free.clone());
                    self.providers[p].models[m].id = free;
                }
            }
        }
        for task in Task::ALL {
            if self.model(self.tasks.get(task)).is_none() { self.tasks.set(task, "") }
        }
        self.screen_workers = self.screen_workers.clamp(1, MAX_SCREEN_WORKERS);
        if !self.detail_width.is_finite() || self.detail_width < 0. { self.detail_width = 0. }
        if !self.sidebar_width.is_finite() || self.sidebar_width < 0. { self.sidebar_width = 0. }
        if !self.chat_width.is_finite() || self.chat_width < 0. { self.chat_width = 0. }
        if !self.ask_list_width.is_finite() || self.ask_list_width < 0. { self.ask_list_width = 0. }
    }

    /// The first of `m1`, `m2`, … no model has yet.
    fn free_id(&self) -> String {
        (1..).map(|n| format!("m{n}")).find(|id| self.model(id).is_none()).unwrap_or_default()
    }

    /// A model by its id, with the provider that offers it.
    pub fn model(&self, id: &str) -> Option<(&Provider, &Model)> {
        if id.is_empty() { return None }
        self.providers.iter().find_map(|provider| provider.models.iter().find(|model| model.id == id).map(|model| (provider, model)))
    }

    /// What a request with that model is made with.
    pub fn profile(&self, id: &str) -> Option<Profile> {
        self.model(id).map(|(provider, model)| provider.profile(model))
    }

    /// Every model of every provider, in the order they are listed.
    pub fn profiles(&self) -> Vec<Profile> {
        self.providers.iter().flat_map(|provider| provider.models.iter().map(|model| provider.profile(model))).collect()
    }

    /// The model a task uses: its own, else the one for questions, else the
    /// first that can answer (a key and a model name), else the first.
    pub fn profile_for(&self, task: Task) -> Option<Profile> {
        if let Some(profile) = self.profile(self.tasks.get(task)).or_else(|| self.profile(&self.tasks.chat)) { return Some(profile) }
        let all = self.profiles();
        all.iter().find(|profile| profile.missing().is_none()).or(all.first()).cloned()
    }

    /// The model a task uses; without any model at all, the built-in
    /// default, which works as soon as its API key is in the environment.
    pub fn profile_for_or_default(&self, task: Task) -> Profile {
        self.profile_for(task).unwrap_or_default()
    }

    /// Add a provider, its models given ids, and return its place in `providers`.
    pub fn add_provider(&mut self, provider: Provider) -> usize {
        self.providers.push(Provider { id: String::new(), ..provider });
        for model in &mut self.providers.last_mut().into_iter().flat_map(|p| p.models.iter_mut()) { model.id.clear() }
        self.tidy();
        self.providers.len() - 1
    }

    /// Take a provider away with its models; the tasks they served follow the default again.
    pub fn remove_provider(&mut self, index: usize) {
        if index >= self.providers.len() { return }
        self.providers.remove(index);
        self.tidy();
    }

    /// One more model of a provider, and its id.
    pub fn add_model(&mut self, provider: usize, model: &str) -> Option<String> {
        let id = self.free_id();
        self.providers.get_mut(provider)?.models.push(Model { id: id.clone(), model: model.trim().to_string(), effort: String::new() });
        Some(id)
    }

    pub fn remove_model(&mut self, provider: usize, index: usize) {
        let Some(provider) = self.providers.get_mut(provider) else { return };
        if index >= provider.models.len() { return }
        provider.models.remove(index);
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

/// The profiles of a settings file from before providers: those sharing an
/// address, a key and a proxy become one provider with their models (keeping
/// their ids, which the tasks are bound to), and the default providers come
/// after them unless one of them is already that service.
fn migrate(legacy: Vec<Profile>, defaults: Vec<Provider>) -> Vec<Provider> {
    let mut providers: Vec<Provider> = vec![];
    for profile in legacy {
        let base = profile.base_url.trim().trim_end_matches('/').to_string();
        let model = Model { id: profile.id.clone(), model: profile.model.trim().to_string(), effort: profile.effort.clone() };
        let same = providers.iter_mut().find(|p| p.protocol == profile.protocol && p.base_url.trim_end_matches('/') == base && p.api_key == profile.api_key.trim() && p.proxy == profile.proxy.trim());
        match same {
            Some(provider) => provider.models.push(model),
            None => {
                let preset = provider::preset_of_address(&base);
                // A name given by hand stays the provider's; the preset's, 模型 N and the old
                // default name (a profile's name was "Claude" until changed) do not.
                let name = profile.name.trim();
                let generic = name.is_empty() || preset.is_some_and(|p| p.name == name) || name.starts_with("模型 ") || name.starts_with("Model ") || (name == "Claude" && profile.protocol != crate::ai::Protocol::Anthropic);
                providers.push(Provider { id: String::new(), name: if generic { String::new() } else { name.to_string() }, preset: preset.map(|p| p.key.to_string()).unwrap_or_default(), protocol: profile.protocol, base_url: profile.base_url.trim().to_string(), api_key: profile.api_key.trim().to_string(), proxy: profile.proxy.trim().to_string(), models: vec![model] });
            }
        }
    }
    for default in defaults {
        if !providers.iter().any(|p| p.preset == default.preset) { providers.push(Provider { models: default.models.into_iter().map(|m| Model { id: String::new(), ..m }).collect(), ..default }) }
    }
    providers
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
        s.add_provider(Provider::from_preset(provider::preset("claude").unwrap()));
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        std::fs::write(&path, "{broken").unwrap();
        assert!(Settings::load(&path).is_err());
        // A file from before a setting existed takes that setting's default.
        std::fs::write(&path, r#"{"dataDir":"/x","shortcuts":{"search":""}}"#).unwrap();
        let old = Settings::load(&path).unwrap();
        assert!(old.auto_update && old.appearance.is_empty() && old.keep_running);
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
        std::fs::write(&path, r#"{"proxy":"socks5://127.0.0.1:1080","profiles":[{"name":"x","protocol":"openai","baseUrl":"http://h/v1","apiKey":"k","model":"m"}]}"#).unwrap();
        let with = Settings::load(&path).unwrap();
        assert_eq!((with.proxy.as_str(), with.providers[0].proxy.as_str(), with.providers[0].title().as_str()), ("socks5://127.0.0.1:1080", "", "x"));
        // Nor an id, a thinking setting or a task: it is given an id and serves everything.
        let model = &with.providers[0].models[0];
        assert_eq!((model.id.as_str(), model.effort.as_str(), with.screen_workers), ("m1", "", 3));
        assert_eq!(with.profile_for(Task::Translate).map(|p| p.model), Some("m".into()));
        // The providers every file starts with come after it, and nothing of the old list is written again.
        assert_eq!(with.providers.iter().map(|p| p.title()).collect::<Vec<_>>(), ["x", "DeepSeek", "Kimi", "GLM"]);
        with.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("\"profiles\""));
        assert_eq!(Settings::load(&path).unwrap(), with);
        // A file that has providers keeps exactly those, none at all included.
        let mut none = Settings::default();
        none.remove_provider(0);
        none.remove_provider(0);
        none.remove_provider(0);
        none.save(&path).unwrap();
        assert!(Settings::load(&path).unwrap().providers.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn models_per_task() {
        let mut s = Settings::default();
        // A new file has DeepSeek, Kimi and GLM, none with a key: the first model is used until one can answer.
        assert_eq!(s.providers.iter().map(|p| p.preset.as_str()).collect::<Vec<_>>(), provider::DEFAULT_PRESETS);
        let ids: Vec<String> = s.profiles().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["m1", "m2", "m3", "m4", "m5"]);
        let uses = |s: &Settings, task: Task| s.profile_for(task).unwrap().model.clone();
        assert_eq!(uses(&s, Task::Chat), "deepseek-v4-pro");
        s.providers[1].api_key = "k".into();
        assert_eq!(uses(&s, Task::Screen), "kimi-k2.6");
        s.providers[1].api_key.clear();
        // Unbound, everything is the first; screening and translation follow the one for questions.
        s.tasks.set(Task::Chat, "m4");
        assert_eq!(Task::ALL.map(|task| uses(&s, task)), ["glm-5.3", "glm-5.3", "glm-5.3", "glm-5.3"]);
        s.tasks.set(Task::Translate, "m2");
        assert_eq!(Task::ALL.map(|task| uses(&s, task)), ["glm-5.3", "glm-5.3", "deepseek-v4-flash", "glm-5.3"]);
        // A removed model or provider takes its bindings along; an id is not handed out twice while in use.
        s.remove_provider(2);
        assert_eq!((s.tasks.chat.as_str(), s.tasks.translate.as_str(), uses(&s, Task::Chat).as_str()), ("", "m2", "deepseek-v4-pro"));
        assert_eq!(s.add_model(1, " kimi-k3 "), Some("m4".into()));
        assert_eq!(s.profile("m4").map(|p| (p.model, p.provider)), Some(("kimi-k3".into(), "Kimi".into())));
        s.remove_model(0, 1);
        assert_eq!((s.tasks.translate.as_str(), uses(&s, Task::Translate).as_str()), ("", "deepseek-v4-pro"));
        // A provider added from a preset gets ids of its own, whatever it came with.
        let claude = s.add_provider(Provider::from_preset(provider::preset("claude").unwrap()));
        assert_eq!(s.providers[claude].models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["m2", "m5", "m6"]);
        s.remove_provider(0);
        s.remove_provider(0);
        s.remove_provider(0);
        assert_eq!(s.profile_for(Task::Chat), None);
        assert_eq!(s.profile_for_or_default(Task::Screen), Profile::default());

        // An older file's `activeProfile` becomes the model for questions; a binding to nothing is dropped.
        // Profiles with one address and key are one provider.
        let dir = std::env::temp_dir().join(format!("wisp-settings-{}", crate::model::new_id("T")));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"activeProfile":1,"screenWorkers":40,"tasks":{"screen":"gone"},"profiles":[{"name":"Claude"},{"name":"b","effort":"off","model":"claude-haiku-5-5"},{"protocol":"openai","baseUrl":"https://api.deepseek.com/","model":"deepseek-v4-pro","id":"m1"}]}"#).unwrap();
        let old = Settings::load(&path).unwrap();
        assert_eq!((old.tasks.chat.as_str(), old.tasks.screen.as_str(), old.screen_workers), ("m3", "", MAX_SCREEN_WORKERS));
        assert_eq!(old.profile_for(Task::Screen).map(|p| (p.model, p.effort, p.provider)), Some(("claude-haiku-5-5".into(), "off".into(), "Claude".into())));
        assert_eq!(old.providers.iter().map(|p| (p.title(), p.models.len())).collect::<Vec<_>>(), [("Claude".to_string(), 2), ("DeepSeek".to_string(), 1), ("Kimi".to_string(), 1), ("GLM".to_string(), 2)]);
        assert_eq!(old.providers.iter().map(|p| p.preset.as_str()).collect::<Vec<_>>(), ["claude", "deepseek", "kimi", "glm"]);
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
