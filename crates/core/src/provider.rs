//! Model services as the settings keep them: a provider (one address, one
//! key, one proxy) offering any number of models. A task is bound to one of
//! those models; what a request is made with is the flat `ai::Profile` a
//! provider and one of its models make together (`Provider::profile`).

use crate::ai::{ANTHROPIC_BASE, DEFAULT_MODEL, Profile, Protocol};
use serde::{Deserialize, Serialize};

/// A service a provider can start from: its address, its interface, the
/// environment variable its key may be in and the models it is set up with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    pub key: &'static str,
    pub name: &'static str,
    pub protocol: Protocol,
    pub base_url: &'static str,
    /// Read when no key is stored. Each service has its own, so a key for one is never sent to another.
    pub key_env: &'static str,
    pub models: &'static [&'static str],
    /// Where an API key is had.
    pub console: &'static str,
}

pub const PRESETS: [Preset; 5] = [
    Preset { key: "deepseek", name: "DeepSeek", protocol: Protocol::OpenAI, base_url: "https://api.deepseek.com", key_env: "DEEPSEEK_API_KEY", models: &["deepseek-v4-pro", "deepseek-v4-flash"], console: "https://platform.deepseek.com/api_keys" },
    Preset { key: "kimi", name: "Kimi", protocol: Protocol::OpenAI, base_url: "https://api.moonshot.cn/v1", key_env: "MOONSHOT_API_KEY", models: &["kimi-k2.6"], console: "https://platform.moonshot.cn/console/api-keys" },
    Preset { key: "glm", name: "GLM", protocol: Protocol::OpenAI, base_url: "https://open.bigmodel.cn/api/paas/v4", key_env: "ZHIPUAI_API_KEY", models: &["glm-5.3", "glm-5.1"], console: "https://open.bigmodel.cn/usercenter/apikeys" },
    Preset { key: "claude", name: "Claude", protocol: Protocol::Anthropic, base_url: ANTHROPIC_BASE, key_env: "ANTHROPIC_API_KEY", models: &[DEFAULT_MODEL, "claude-sonnet-5-5", "claude-haiku-5-5"], console: "https://console.anthropic.com/settings/keys" },
    Preset { key: "openai", name: "OpenAI", protocol: Protocol::Responses, base_url: "https://api.openai.com/v1", key_env: "OPENAI_API_KEY", models: &[], console: "https://platform.openai.com/api-keys" },
];

/// The providers a new settings file starts with.
pub const DEFAULT_PRESETS: [&str; 3] = ["deepseek", "kimi", "glm"];

pub fn preset(key: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.key == key)
}

/// The preset an address belongs to, for settings written before presets existed.
pub fn preset_of_address(base_url: &str) -> Option<&'static Preset> {
    let host = base_url.split("://").nth(1).unwrap_or(base_url).split(['/', ':']).next().unwrap_or("").to_lowercase();
    let key = if host.ends_with("deepseek.com") { "deepseek" } else if host.contains("moonshot") { "kimi" } else if host.ends_with("bigmodel.cn") { "glm" } else if host.ends_with("anthropic.com") { "claude" } else if host == "api.openai.com" { "openai" } else { return None };
    preset(key)
}

/// One model a provider offers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Model {
    /// What the task bindings point at; unique among all providers' models.
    pub id: String,
    /// The model's name as the service knows it (`deepseek-v4-pro`).
    pub model: String,
    /// How hard it thinks, a key of `ai::EFFORTS`.
    pub effort: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    /// What the person calls it; empty is the preset's name, else the address's host.
    pub name: String,
    /// The `Preset::key` it started from; empty for a service set up by hand.
    pub preset: String,
    pub protocol: Protocol,
    pub base_url: String,
    /// Empty means the preset's environment variable, else ANTHROPIC_API_KEY / OPENAI_API_KEY.
    pub api_key: String,
    /// A proxy for this service alone: empty uses the global one, `direct` none at all.
    pub proxy: String,
    pub models: Vec<Model>,
}

impl Default for Provider {
    /// A service set up by hand: anything that speaks chat completions.
    fn default() -> Self {
        Provider { id: String::new(), name: String::new(), preset: String::new(), protocol: Protocol::OpenAI, base_url: String::new(), api_key: String::new(), proxy: String::new(), models: vec![] }
    }
}

impl Provider {
    pub fn from_preset(preset: &Preset) -> Provider {
        Provider { preset: preset.key.into(), protocol: preset.protocol, base_url: preset.base_url.into(), models: preset.models.iter().map(|model| Model { model: model.to_string(), ..Default::default() }).collect(), ..Default::default() }
    }

    pub fn preset(&self) -> Option<&'static Preset> {
        preset(&self.preset)
    }

    pub fn title(&self) -> String {
        if !self.name.trim().is_empty() { return self.name.trim().to_string() }
        if let Some(preset) = self.preset() { return preset.name.into() }
        let host = self.base_url.split("://").nth(1).unwrap_or(&self.base_url).split(['/', ':']).next().unwrap_or("").trim();
        if host.is_empty() { "未命名服务".into() } else { host.to_string() }
    }

    /// The environment variable a key is read from when none is stored.
    pub fn key_env(&self) -> &'static str {
        match self.preset() {
            // A preset moved to another interface of the same service keeps its variable,
            Some(preset) if preset.protocol == self.protocol || (preset.protocol != Protocol::Anthropic && self.protocol != Protocol::Anthropic) => preset.key_env,
            _ => match self.protocol {
                Protocol::Anthropic => "ANTHROPIC_API_KEY",
                Protocol::OpenAI | Protocol::Responses => "OPENAI_API_KEY",
            },
        }
    }

    /// What a request with one of its models is made with.
    pub fn profile(&self, model: &Model) -> Profile {
        Profile { id: model.id.clone(), name: String::new(), provider: self.title(), protocol: self.protocol, base_url: self.base_url.clone(), api_key: self.api_key.clone(), key_env: self.key_env().into(), model: model.model.clone(), effort: model.effort.clone(), proxy: self.proxy.clone() }
    }

    /// The key it would send now, stored or from the environment.
    pub fn key(&self) -> String {
        self.profile(&Model::default()).key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_keys() {
        let deepseek = Provider::from_preset(preset("deepseek").unwrap());
        assert_eq!((deepseek.title().as_str(), deepseek.models.len(), deepseek.key_env()), ("DeepSeek", 2, "DEEPSEEK_API_KEY"));
        // A key of the OpenAI kind is never what DeepSeek is asked with.
        let profile = deepseek.profile(&deepseek.models[0]);
        assert_eq!((profile.key_env.as_str(), profile.model.as_str(), profile.provider.as_str(), profile.title().as_str()), ("DEEPSEEK_API_KEY", "deepseek-v4-pro", "DeepSeek", "deepseek-v4-pro"));
        let custom = Provider { base_url: "http://localhost:11434/v1".into(), ..Default::default() };
        assert_eq!((custom.title().as_str(), custom.key_env()), ("localhost", "OPENAI_API_KEY"));
        assert_eq!(preset_of_address("https://api.moonshot.cn/v1").map(|p| p.key), Some("kimi"));
        assert_eq!(preset_of_address("https://api.anthropic.com").map(|p| p.key), Some("claude"));
        assert_eq!(preset_of_address("http://127.0.0.1:8787").map(|p| p.key), None);
        assert!(DEFAULT_PRESETS.iter().all(|key| preset(key).is_some()));
    }
}
