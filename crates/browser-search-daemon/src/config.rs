use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rand::{RngExt, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::model::SearchKind;

#[derive(Debug, Parser)]
#[command(
    name = "search-server",
    version,
    about = "Local browser-backed search API"
)]
pub struct Cli {
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub listen: Option<SocketAddr>,
    #[arg(long)]
    pub bridge_listen: Option<SocketAddr>,
    #[arg(long, env = "SEARCH_API_KEY")]
    pub api_token: Option<String>,
    #[arg(long, env = "BROWSER_SEARCH_EXTENSION_TOKEN")]
    pub extension_token: Option<String>,
    #[arg(long)]
    pub max_concurrency: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub bridge: BridgeConfig,
    pub executor: ExecutorConfig,
    pub search: SearchConfig,
}

impl Config {
    pub fn load(cli: &Cli) -> Result<(Self, GeneratedTokens)> {
        let mut config = if let Some(path) = cli.config.as_deref() {
            Self::from_file(path)?
        } else {
            Self::default()
        };

        if let Some(value) = cli.listen {
            config.server.listen = value;
        }
        if let Some(value) = cli.bridge_listen {
            config.bridge.listen = value;
        }
        if let Some(value) = cli.api_token.as_ref() {
            config.server.api_token.clone_from(value);
        }
        if let Some(value) = cli.extension_token.as_ref() {
            config.bridge.extension_token.clone_from(value);
        }
        if let Some(value) = cli.max_concurrency {
            config.executor.max_concurrency = value;
        }

        config.executor.max_concurrency = config.executor.max_concurrency.max(1);
        config.executor.max_queue_size = config.executor.max_queue_size.max(1);
        config.executor.max_timeout_ms = config.executor.max_timeout_ms.max(1_000);
        config.executor.load_timeout_ms = config.executor.load_timeout_ms.max(1_000);
        config.executor.selector_timeout_ms = config.executor.selector_timeout_ms.max(250);
        config.bridge.ping_interval_seconds = config.bridge.ping_interval_seconds.clamp(5, 300);
        config.search.validate()?;

        let mut generated = GeneratedTokens::default();
        if config.server.api_token.is_empty() {
            config.server.api_token = random_token();
            generated.api_token = Some(config.server.api_token.clone());
        }
        if config.bridge.extension_token.is_empty() {
            config.bridge.extension_token = random_token();
            generated.extension_token = Some(config.bridge.extension_token.clone());
        }

        Ok((config, generated))
    }

    fn from_file(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("无法读取配置文件 {}", path.display()))?;
        let overrides: toml::Value =
            toml::from_str(&raw).with_context(|| format!("配置文件格式错误 {}", path.display()))?;
        let mut merged = toml::Value::try_from(Self::default()).context("无法序列化默认配置")?;
        merge_toml(&mut merged, overrides);
        merged
            .try_into()
            .with_context(|| format!("配置文件内容无效 {}", path.display()))
    }
}

fn merge_toml(base: &mut toml::Value, overrides: toml::Value) {
    match (base, overrides) {
        (toml::Value::Table(base), toml::Value::Table(overrides)) => {
            for (key, value) in overrides {
                if let Some(existing) = base.get_mut(&key) {
                    merge_toml(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overrides) => *base = overrides,
    }
}

fn random_token() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}

#[derive(Debug, Default)]
pub struct GeneratedTokens {
    pub api_token: Option<String>,
    pub extension_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub api_token: String,
    pub allow_cors: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:17330".parse().expect("valid default address"),
            api_token: String::new(),
            allow_cors: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BridgeConfig {
    pub listen: SocketAddr,
    pub extension_token: String,
    pub browser_instance_id: String,
    pub ping_interval_seconds: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:17331".parse().expect("valid default address"),
            extension_token: String::new(),
            browser_instance_id: String::new(),
            ping_interval_seconds: 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutorConfig {
    pub max_concurrency: usize,
    /// Extension tab-opening interval, restarted by completed page cleanup, in milliseconds.
    pub min_operation_interval: u32,
    pub max_queue_size: usize,
    pub max_timeout_ms: u64,
    pub load_timeout_ms: u64,
    pub selector_timeout_ms: u64,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 1,
            min_operation_interval: 4000,
            max_queue_size: 64,
            max_timeout_ms: 120_000,
            load_timeout_ms: 20_000,
            selector_timeout_ms: 10_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    pub common: SearchCommonConfig,
    pub web: SearchProfile,
    pub news: SearchProfile,
    pub images: SearchProfile,
    pub videos: SearchProfile,
    pub forums: SearchProfile,
}

impl Default for SearchConfig {
    fn default() -> Self {
        toml::from_str(include_str!("../../../search-rules.default.toml"))
            .expect("embedded default search rules must be valid")
    }
}

impl SearchConfig {
    pub fn profile(&self, kind: SearchKind) -> &SearchProfile {
        match kind {
            SearchKind::Web => &self.web,
            SearchKind::News => &self.news,
            SearchKind::Images => &self.images,
            SearchKind::Videos => &self.videos,
            SearchKind::Forums => &self.forums,
        }
    }

    fn validate(&self) -> Result<()> {
        let url = Url::parse(&self.common.base_url).context("search.common.base_url 无效")?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("search.common.base_url 只允许 http 或 https");
        }
        if self.common.query_parameter.is_empty() {
            bail!("search.common.query_parameter 不能为空");
        }

        for (name, profile) in [
            ("web", &self.web),
            ("news", &self.news),
            ("images", &self.images),
            ("videos", &self.videos),
            ("forums", &self.forums),
        ] {
            if profile.root_selectors.is_empty()
                || profile
                    .root_selectors
                    .iter()
                    .any(|selector| selector.is_empty())
            {
                bail!("search.{name}.root_selectors 不能为空");
            }
            if profile.fields.is_empty() {
                bail!("search.{name}.fields 不能为空");
            }
            if profile.dedupe_field.is_empty() {
                bail!("search.{name}.dedupe_field 不能为空");
            }
            if profile
                .limit
                .is_some_and(|limit| !(1..=100).contains(&limit))
            {
                bail!("search.{name}.limit 必须在 1 到 100 之间");
            }
            for (field_name, field) in &profile.fields {
                if !field.enabled {
                    continue;
                }
                if field.selectors.is_empty()
                    || field.selectors.iter().any(|selector| selector.is_empty())
                {
                    bail!("search.{name}.fields.{field_name}.selectors 不能为空");
                }
                if field.max_length == Some(0) {
                    bail!("search.{name}.fields.{field_name}.max_length 必须大于 0");
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCommonConfig {
    pub base_url: String,
    pub query_parameter: String,
    pub start_parameter: String,
    pub limit_parameter: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchProfile {
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub root_selectors: Vec<String>,
    #[serde(default)]
    pub dedupe_field: String,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldRule {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub selectors: Vec<String>,
    #[serde(default)]
    pub attribute: Option<String>,
    #[serde(default)]
    pub transform: FieldTransform,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub max_length: Option<usize>,
}

impl Default for FieldRule {
    fn default() -> Self {
        Self {
            enabled: true,
            selectors: Vec::new(),
            attribute: None,
            transform: FieldTransform::None,
            required: false,
            max_length: None,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldTransform {
    #[default]
    None,
    AbsoluteUrl,
    GoogleUrl,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_uses_search_server_program_name() {
        assert_eq!(Cli::command().get_name(), "search-server");
    }

    #[test]
    fn default_rules_are_loaded_from_toml() {
        let config = Config::default();
        assert_eq!(config.executor.min_operation_interval, 4000);
        assert_eq!(config.search.web.root_selectors, ["[data-snc]"]);
        assert_eq!(
            config.search.web.params.get("udm").map(String::as_str),
            Some("14")
        );
        assert_eq!(config.search.web.fields["url"].selectors, ["[data-snhf] a"]);
        assert_eq!(
            config.search.web.fields["url"].attribute.as_deref(),
            Some("href")
        );
        assert!(matches!(
            config.search.web.fields["url"].transform,
            FieldTransform::GoogleUrl
        ));
        assert_eq!(
            config.search.web.fields["title"].selectors,
            ["[data-snhf] h3"]
        );
        assert_eq!(
            config.search.web.fields["description"].selectors,
            ["[data-sncf]"]
        );
        assert_eq!(
            config.search.news.root_selectors,
            ["[data-news-cluster-id]"]
        );
        assert_eq!(
            config.search.news.params.get("tbm").map(String::as_str),
            Some("nws")
        );
        assert_eq!(config.search.news.fields["url"].selectors, ["a"]);
        assert_eq!(
            config.search.news.fields["url"].attribute.as_deref(),
            Some("href")
        );
        assert!(matches!(
            config.search.news.fields["url"].transform,
            FieldTransform::GoogleUrl
        ));
        assert_eq!(
            config.search.news.fields["source"].selectors,
            ["a > div > div:nth-of-type(2) > div:nth-of-type(1)"]
        );
        assert_eq!(
            config.search.news.fields["title"].selectors,
            ["a > div > div:nth-of-type(2) > div:nth-of-type(2)"]
        );
        assert_eq!(
            config.search.news.fields["description"].selectors,
            ["a > div > div:nth-of-type(2) > div:nth-of-type(3)"]
        );
        assert_eq!(
            config.search.news.fields["time"].selectors,
            ["a > div > div:nth-of-type(2) > div:nth-of-type(4)"]
        );
        assert_eq!(
            config.search.images.root_selectors,
            ["[data-snf][data-snm] [data-lpage]"]
        );
        assert_eq!(config.search.images.limit, Some(100));
        assert_eq!(config.search.images.fields["url"].selectors, ["&"]);
        assert_eq!(
            config.search.images.fields["url"].attribute.as_deref(),
            Some("data-lpage")
        );
        assert_eq!(
            config.search.images.fields["imgurl"].selectors,
            ["[data-bla] > img"]
        );
        assert_eq!(
            config.search.images.fields["imgurl"].attribute.as_deref(),
            Some("src")
        );
        assert!(matches!(
            config.search.images.fields["imgurl"].transform,
            FieldTransform::AbsoluteUrl
        ));
        assert!(!config.search.images.fields.contains_key("image"));
        assert_eq!(
            config.search.images.fields["title"].selectors,
            ["[data-bla] > img"]
        );
        assert_eq!(config.search.videos.root_selectors, ["[data-curl]"]);
        assert_eq!(config.search.videos.fields["url"].selectors, ["a:has(h3)"]);
        assert_eq!(
            config.search.videos.fields["title"].selectors,
            ["a:has(h3) > h3"]
        );
        assert_eq!(
            config.search.videos.fields["description"].selectors,
            ["[data-curl] > div > div[jsshadow] > div:nth-child(2) > div:nth-child(1)"]
        );
        assert_eq!(
            config.search.videos.fields["duration"].selectors,
            [
                "[data-curl] [data-vll][data-ved][tabindex] > div > div > div:nth-child(3) > div:nth-child(1) > span"
            ]
        );
        assert!(config.search.videos.fields["duration"].required);
        assert_eq!(config.search.forums.root_selectors, ["[data-rpos]"]);
        assert_eq!(
            config.search.forums.fields["url"].selectors,
            ["[data-rpos] [data-snf][data-snhf] a"]
        );
        assert!(matches!(
            config.search.forums.fields["url"].transform,
            FieldTransform::AbsoluteUrl
        ));
        assert_eq!(
            config.search.forums.fields["title"].selectors,
            ["[data-rpos] [data-snf][data-snhf] a > h3"]
        );
        assert_eq!(
            config.search.forums.fields["description"].selectors,
            ["[data-rpos] [data-snf][data-sncf] > div"]
        );
    }

    #[test]
    fn recursive_merge_keeps_unspecified_defaults() {
        let mut base = toml::Value::try_from(Config::default()).unwrap();
        let overrides: toml::Value = toml::from_str(
            r#"
                [executor]
                min_operation_interval = 750

                [search.web]
                root_selectors = [".custom-result"]
            "#,
        )
        .unwrap();
        merge_toml(&mut base, overrides);
        let config: Config = base.try_into().unwrap();
        assert_eq!(config.executor.min_operation_interval, 750);
        assert_eq!(config.search.web.root_selectors, [".custom-result"]);
        assert!(config.search.web.fields.contains_key("url"));
    }
}
