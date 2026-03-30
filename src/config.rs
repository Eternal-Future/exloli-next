use std::time::Duration;

use anyhow::Result;
use duration_str::deserialize_duration;
use once_cell::sync::OnceCell;
use serde::Deserialize;
use teloxide::types::{ChatId, Recipient};

pub static CHANNEL_ID: OnceCell<String> = OnceCell::new();

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// 日志等级
    pub log_level: String,
    /// 同时下载线程数量
    pub threads_num: usize,
    /// 定时爬取间隔
    #[serde(deserialize_with = "deserialize_duration")]
    pub interval: Duration,
    /// Sqlite 数据库位置
    pub database_url: String,
    pub exhentai: ExHentai,
    pub telegraph: Telegraph,
    pub telegram: Telegram,
    pub s3: S3,
}

/// 站点类型
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Site {
    /// 表站 e-hentai.org
    Ehentai,
    /// 里站 exhentai.org
    Exhentai,
}

impl Site {
    pub fn base_url(&self) -> &'static str {
        match self {
            Site::Ehentai => "https://e-hentai.org",
            Site::Exhentai => "https://exhentai.org",
        }
    }

    pub fn host(&self) -> &'static str {
        match self {
            Site::Ehentai => "e-hentai.org",
            Site::Exhentai => "exhentai.org",
        }
    }
}

/// 代理类型
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// 不使用代理
    None,
    /// 半代理：仅刷新 igneous 时使用
    Half,
    /// 全代理：所有流量都使用代理
    Full,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Proxy {
    /// 代理模式
    pub mode: ProxyMode,
    /// 代理地址，格式：http://host:port 或 socks5://host:port
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExHentai {
    /// 站点选择
    #[serde(default = "default_site")]
    pub site: Site,
    /// 基础 cookie (ipb_member_id 和 ipb_pass_hash)
    pub cookie: String,
    /// igneous 值（可选，如果站点是 exhentai 且需要自动刷新）
    pub igneous: Option<String>,
    /// 是否自动刷新 igneous（仅 exhentai 有效）
    #[serde(default)]
    pub auto_refresh_igneous: bool,
    /// 刷新 igneous 的 cron 表达式（默认每天 0 点）
    #[serde(default = "default_refresh_cron")]
    pub refresh_cron: String,
    /// 代理配置
    #[serde(default = "default_proxy")]
    pub proxy: Proxy,
    /// 搜索参数
    pub search_params: Vec<(String, String)>,
    /// 最大遍历画廊数量
    pub search_count: usize,
    /// 翻译文件的位置
    pub trans_file: String,
}

fn default_site() -> Site {
    Site::Exhentai
}

fn default_refresh_cron() -> String {
    "0 0 0 * * * *".to_string()
}

fn default_proxy() -> Proxy {
    Proxy {
        mode: ProxyMode::None,
        url: None,
    }
}

impl ExHentai {
    /// 获取完整的 cookie 字符串
    pub fn full_cookie(&self) -> String {
        if let Some(ref igneous) = self.igneous {
            format!("{}; igneous={}", self.cookie, igneous)
        } else {
            self.cookie.clone()
        }
    }

    /// 获取基础 cookie (不含 igneous)
    pub fn base_cookie(&self) -> &str {
        &self.cookie
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Telegraph {
    /// Telegraph token
    pub access_token: String,
    /// 文章作者名称
    pub author_name: String,
    /// 文章作者连接
    pub author_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Telegram {
    /// 频道 id
    pub channel_id: Recipient,
    /// bot 名称
    pub bot_id: String,
    /// bot token
    pub token: String,
    /// 讨论组 ID
    pub group_id: ChatId,
    /// 入口讨论组 ID
    pub auth_group_id: ChatId,
}

#[derive(Debug, Clone, Deserialize)]
pub struct S3 {
    /// region
    pub region: String,
    /// S3 endpoint
    pub endpoint: String,
    /// bucket 名称
    pub bucket: String,
    /// access-key
    pub access_key: String,
    /// secret-key
    pub secret_key: String,
    /// 公开访问连接
    pub host: String,
}

impl Config {
    pub fn new(path: &str) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&s)?)
    }
}
