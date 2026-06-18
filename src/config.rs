use std::net::TcpListener;
use std::time::Duration;

use anyhow::{bail, Result};
use duration_str::deserialize_duration;
use once_cell::sync::OnceCell;
use rand::Rng;
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
    #[serde(default)]
    pub onebot: OneBot,
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
    Proxy { mode: ProxyMode::None, url: None }
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
pub struct OneBot {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_onebot_listen_host")]
    pub listen_host: String,
    #[serde(default)]
    pub listen_port: u16,
    #[serde(default = "default_onebot_path")]
    pub path: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub private_user_ids: Vec<i64>,
    #[serde(default)]
    pub group_ids: Vec<i64>,
}

impl Default for OneBot {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_host: default_onebot_listen_host(),
            listen_port: 0,
            path: default_onebot_path(),
            access_token: String::new(),
            private_user_ids: Vec::new(),
            group_ids: Vec::new(),
        }
    }
}

fn default_onebot_listen_host() -> String {
    "0.0.0.0".to_string()
}

fn default_onebot_path() -> String {
    "/expush".to_string()
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
        let mut config: Config = toml::from_str(&s)?;
        if config.onebot.enabled {
            if config.onebot.path != "/expush" {
                bail!("onebot.path 必须为 /expush");
            }
            if config.onebot.access_token.is_empty() {
                bail!("启用 OneBot 时必须设置 onebot.access_token");
            }
            if config.onebot.listen_port == 0 {
                let port = pick_onebot_port(&config.onebot.listen_host)?;
                persist_onebot_port(path, &s, port)?;
                config.onebot.listen_port = port;
            }
        }
        Ok(config)
    }

    /// 更新配置文件中的 igneous 值
    pub fn update_igneous_in_file(config_path: &str, new_igneous: &str) -> Result<()> {
        let content = std::fs::read_to_string(config_path)?;

        // 使用正则表达式替换 igneous 行
        let re = regex::Regex::new(r#"(?m)^igneous\s*=\s*"[^"]*""#)?;
        let new_content = re.replace(&content, format!(r#"igneous = "{}""#, new_igneous));

        std::fs::write(config_path, new_content.as_bytes())?;
        Ok(())
    }
}

fn pick_onebot_port(host: &str) -> Result<u16> {
    let mut rng = rand::thread_rng();
    for _ in 0..512 {
        let port = rng.gen_range(30000..=60000);
        if TcpListener::bind((host, port)).is_ok() {
            return Ok(port);
        }
    }
    bail!("无法在 30000-60000 范围内找到可用 OneBot 端口")
}

fn persist_onebot_port(path: &str, content: &str, port: u16) -> Result<()> {
    let listen_port_re = regex::Regex::new(r#"(?m)^listen_port\s*=\s*\d+"#)?;
    let next = if listen_port_re.is_match(content) {
        listen_port_re.replace(content, format!("listen_port = {port}")).to_string()
    } else if content.contains("[onebot]") {
        let onebot_re = regex::Regex::new(r#"(?m)^\[onebot\]\s*$"#)?;
        onebot_re.replace(content, format!("[onebot]\nlisten_port = {port}")).to_string()
    } else {
        format!("{content}\n\n[onebot]\nlisten_port = {port}\n")
    };
    std::fs::write(path, next.as_bytes())?;
    Ok(())
}
