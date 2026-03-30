use anyhow::Result;
use chrono::Utc;
use cron::Schedule;
use reqwest::header::{HeaderMap, HeaderValue, COOKIE, SET_COOKIE};
use reqwest::Client;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::config::{ExHentai, ProxyMode, Site};

/// Igneous 刷新器
#[derive(Clone)]
pub struct IgneousRefresher {
    config: Arc<RwLock<ExHentai>>,
    schedule: Schedule,
    client: Client,
}

impl IgneousRefresher {
    /// 创建刷新器
    pub fn new(config: ExHentai) -> Result<Self> {
        let schedule = Schedule::from_str(&config.refresh_cron)?;
        
        // 构建用于刷新的 HTTP 客户端（可能需要代理）
        let mut client_builder = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(30));

        // 如果配置了半代理或全代理，添加代理设置（这里刷新总是需要代理，如果配置了的话）
        if config.proxy.mode == ProxyMode::Half || config.proxy.mode == ProxyMode::Full {
            if let Some(ref proxy_url) = config.proxy.url {
                client_builder = client_builder.proxy(reqwest::Proxy::all(proxy_url)?);
                info!("刷新 igneous 将使用代理: {}", proxy_url);
            }
        }

        let client = client_builder.build()?;

        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            schedule,
            client,
        })
    }

    /// 启动定时刷新任务
    pub async fn start(self) -> Result<()> {
        loop {
            let now = Utc::now();
            let next = self.schedule.upcoming(Utc).next();

            if let Some(next_time) = next {
                let wait_duration = (next_time - now).to_std().unwrap_or(Duration::from_secs(60));
                info!("下次刷新 igneous 时间: {}, 等待 {:?}", next_time, wait_duration);
                
                tokio::time::sleep(wait_duration).await;
                
                // 执行刷新
                if let Err(e) = self.refresh().await {
                    error!("刷新 igneous 失败: {}", e);
                }
            } else {
                warn!("无法计算下次刷新时间，等待 1 小时后重试");
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
    }

    /// 执行一次刷新
    pub async fn refresh(&self) -> Result<()> {
        info!("开始刷新 igneous");

        let config = self.config.read().await;
        
        // 只有里站才需要刷新 igneous
        if config.site != Site::Exhentai {
            warn!("当前站点不是里站，跳过刷新");
            return Ok(());
        }

        let base_cookie = config.base_cookie();
        let current_igneous = config.igneous.clone();
        drop(config); // 释放读锁

        // 1. 如果有当前的 igneous，先测试是否有效
        if let Some(ref igneous) = current_igneous {
            debug!("测试当前 igneous 是否有效: {}", igneous);
            let full_cookie = format!("{}; igneous={}", base_cookie, igneous);
            
            match self.test_cookie(&full_cookie).await {
                Ok(true) => {
                    info!("当前 igneous 仍然有效，无需刷新");
                    return Ok(());
                }
                Ok(false) => {
                    warn!("当前 igneous 已失效，需要刷新");
                }
                Err(e) => {
                    warn!("测试 igneous 时出错: {}, 尝试刷新", e);
                }
            }
        } else {
            info!("当前没有 igneous，尝试获取新的");
        }

        // 2. 使用基础 cookie 请求获取新的 igneous
        let new_igneous = self.fetch_new_igneous(base_cookie).await?;
        
        // 3. 测试新的 igneous 是否有效
        let full_cookie = format!("{}; igneous={}", base_cookie, new_igneous);
        if self.test_cookie(&full_cookie).await? {
            info!("成功获取并验证新的 igneous: {}", new_igneous);
            
            // 4. 更新配置中的 igneous
            let mut config = self.config.write().await;
            config.igneous = Some(new_igneous.clone());
            drop(config);
            
            // TODO: 这里应该持久化到配置文件，但目前先只更新内存
            warn!("新的 igneous 已更新到内存，但未持久化到配置文件");
            
            Ok(())
        } else {
            error!("新获取的 igneous 验证失败");
            Err(anyhow::anyhow!("新获取的 igneous 验证失败"))
        }
    }

    /// 测试 cookie 是否有效（通过请求里站首页判断）
    async fn test_cookie(&self, cookie: &str) -> Result<bool> {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_str(cookie)?);

        let resp = self
            .client
            .get("https://exhentai.org")
            .headers(headers)
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;

        // 如果返回的内容为空或很短（通常表站会返回很短的内容），说明 cookie 无效
        // 有效的里站页面通常会包含很多内容
        let is_valid = status.is_success() && body.len() > 1000;
        
        debug!("Cookie 测试结果: status={}, body_len={}, valid={}", 
               status, body.len(), is_valid);
        
        Ok(is_valid)
    }

    /// 获取新的 igneous
    async fn fetch_new_igneous(&self, base_cookie: &str) -> Result<String> {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_str(base_cookie)?);

        let resp = self
            .client
            .get("https://exhentai.org")
            .headers(headers)
            .send()
            .await?;

        // 从 Set-Cookie 响应头中提取 igneous
        let set_cookie_headers = resp.headers().get_all(SET_COOKIE);
        
        for cookie in set_cookie_headers {
            let cookie_str = cookie.to_str()?;
            debug!("收到 Set-Cookie: {}", cookie_str);
            
            // 解析 igneous=xxx; ...
            if let Some(igneous_part) = cookie_str.split(';').next() {
                if let Some((key, value)) = igneous_part.split_once('=') {
                    if key.trim() == "igneous" {
                        info!("成功从响应中提取 igneous: {}", value);
                        return Ok(value.to_string());
                    }
                }
            }
        }

        Err(anyhow::anyhow!("未能从响应中找到 igneous cookie"))
    }

    /// 获取当前的完整 cookie（供外部使用）
    pub async fn get_current_cookie(&self) -> String {
        self.config.read().await.full_cookie()
    }

    /// 手动更新 igneous（供外部调用）
    pub async fn update_igneous(&self, igneous: String) {
        let mut config = self.config.write().await;
        config.igneous = Some(igneous);
    }
}
