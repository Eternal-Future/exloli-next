use chrono::prelude::*;
use futures::prelude::*;
use indexmap::IndexMap;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::*;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Serialize;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, Instrument};

use super::error::*;
use super::types::*;
use crate::config::{ExHentai, ProxyMode, Site};
use crate::utils::html::SelectorExtend;

macro_rules! headers {
    ($host:expr, $($k:ident => $v:expr), *) => {{
        let mut map = HeaderMap::new();
        $(map.insert($k.clone(), $v.parse().unwrap());)*
        map.insert(HOST.clone(), $host.parse().unwrap());
        map.insert(REFERER.clone(), format!("https://{}", $host).parse().unwrap());
        map
    }};
}

macro_rules! send {
    ($e:expr) => {
        $e.send().await.and_then(reqwest::Response::error_for_status)
    };
}

macro_rules! selector {
    ($selector:tt) => {
        Selector::parse($selector).unwrap()
    };
}

#[derive(Debug, Clone)]
pub struct EhClient {
    client: Client,
    site: Site,
    cookie: Arc<RwLock<String>>,
}

impl EhClient {
    #[tracing::instrument(skip(config))]
    pub async fn new(config: &ExHentai) -> Result<Self> {
        info!("登陆 {} 中", if config.site == Site::Exhentai { "里站" } else { "表站" });
        
        let cookie = config.full_cookie();
        let site = config.site;
        
        let mut client_builder = Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(30));

        // 如果是全代理模式，添加代理
        if config.proxy.mode == ProxyMode::Full {
            if let Some(ref proxy_url) = config.proxy.url {
                client_builder = client_builder.proxy(reqwest::Proxy::all(proxy_url)?);
                info!("使用代理: {}", proxy_url);
            }
        }

        let client = client_builder.build()?;

        let base_url = site.base_url();
        
        // 初始请求以设置必要的 cookie
        let headers = headers! {
            site.host(),
            ACCEPT => "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            ACCEPT_ENCODING => "gzip, deflate, br",
            ACCEPT_LANGUAGE => "zh-CN,en-US;q=0.7,en;q=0.3",
            CACHE_CONTROL => "max-age=0",
            CONNECTION => "keep-alive",
            UPGRADE_INSECURE_REQUESTS => "1",
            USER_AGENT => "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:67.0) Gecko/20100101 Firefox/67.0",
            COOKIE => &cookie
        };

        // 获取必要的 cookie
        let _response = client
            .get(&format!("{}/uconfig.php", base_url))
            .headers(headers.clone())
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)?;
        
        let _response = client
            .get(&format!("{}/mytags", base_url))
            .headers(headers)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)?;

        Ok(Self {
            client,
            site,
            cookie: Arc::new(RwLock::new(cookie)),
        })
    }

    /// 更新 cookie（用于 igneous 刷新后更新）
    pub async fn update_cookie(&self, new_cookie: String) {
        let mut cookie = self.cookie.write().await;
        *cookie = new_cookie;
        info!("EhClient cookie 已更新");
    }

    /// 获取当前 cookie
    async fn get_cookie(&self) -> String {
        self.cookie.read().await.clone()
    }

    /// 获取请求头
    async fn get_headers(&self) -> HeaderMap {
        let cookie = self.get_cookie().await;
        headers! {
            self.site.host(),
            ACCEPT => "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            ACCEPT_ENCODING => "gzip, deflate, br",
            ACCEPT_LANGUAGE => "zh-CN,en-US;q=0.7,en;q=0.3",
            CACHE_CONTROL => "max-age=0",
            CONNECTION => "keep-alive",
            UPGRADE_INSECURE_REQUESTS => "1",
            USER_AGENT => "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:67.0) Gecko/20100101 Firefox/67.0",
            COOKIE => &cookie
        }
    }

    /// 访问指定页面，返回画廊列表
    #[tracing::instrument(skip(self, params))]
    async fn page<T: Serialize + ?Sized + Debug>(
        &self,
        url: &str,
        params: &T,
        next: &str,
    ) -> Result<(Vec<EhGalleryUrl>, Option<String>)> {
        let headers = self.get_headers().await;
        let resp = send!(self.client.get(url).headers(headers).query(params).query(&[("next", next)]))?;
        let html = Html::parse_document(&resp.text().await?);

        let selector = selector!("table.itg.gltc tr");
        let gl_list = html.select(&selector);

        let mut ret = vec![];
        // 第一个是 header
        for gl in gl_list.skip(1) {
            let title = gl.select_text("td.gl3c.glname a div.glink").unwrap();
            let url = gl.select_attr("td.gl3c.glname a", "href").unwrap();
            debug!(url, title);
            ret.push(url.parse()?)
        }

        let next = html
            .select_attr("a#dnext", "href")
            .and_then(|s| s.rsplit('=').next().map(|s| s.to_string()));

        Ok((ret, next))
    }

    /// 搜索前 N 页的本子，返回一个异步迭代器
    #[tracing::instrument(skip(self, params))]
    pub fn search_iter<'a, T: Serialize + ?Sized + Debug>(
        &'a self,
        params: &'a T,
    ) -> impl Stream<Item = EhGalleryUrl> + 'a {
        let base_url = self.site.base_url();
        self.page_iter(base_url, params)
    }

    /// 获取指定页面的画廊列表，返回一个异步迭代器
    #[tracing::instrument(skip(self, params))]
    pub fn page_iter<'a, T: Serialize + ?Sized + Debug>(
        &'a self,
        url: &'a str,
        params: &'a T,
    ) -> impl Stream<Item = EhGalleryUrl> + 'a {
        stream::unfold(Some("0".to_string()), move |next| {
            async move {
                match next {
                    None => None,
                    Some(next) => match self.page(url, params, &next).await {
                        Ok((gls, next)) => {
                            debug!("下一页 {:?}", next);
                            Some((stream::iter(gls), next))
                        }
                        Err(e) => {
                            error!("search error: {}", e);
                            None
                        }
                    },
                }
            }
            .in_current_span()
        })
        .flatten()
    }

    #[tracing::instrument(skip(self))]
    pub async fn archive_gallery(&self, url: &EhGalleryUrl) -> Result<()> {
        static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"or=(?P<or>[0-9a-z-]+)").unwrap());

        let headers = self.get_headers().await;
        let resp = send!(self.client.get(url.url()).headers(headers.clone()))?;
        let html = Html::parse_document(&resp.text().await?);
        let onclick = html.select_attr("p.g2 a", "onclick").unwrap();

        let or = RE.captures(&onclick).and_then(|c| c.name("or")).unwrap().as_str();

        let base_url = self.site.base_url();
        send!(self
            .client
            .post(&format!("{}/archiver.php", base_url))
            .headers(headers)
            .query(&[("gid", &*url.id().to_string()), ("token", url.token()), ("or", or)])
            .form(&[("hathdl_xres", "org")]))?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_gallery(&self, url: &EhGalleryUrl) -> Result<EhGallery> {
        // NOTE: 由于 Html 是 !Send 的，为了避免它被包含在 Future 上下文中，这里将它放在一个单独的作用域内
        // 参见：https://rust-lang.github.io/async-book/07_workarounds/03_send_approximation.html
        let (title, title_jp, parent, tags, favorite, mut pages, posted, mut next_page) = {
            let headers = self.get_headers().await;
            let resp = send!(self.client.get(url.url()).headers(headers))?;
            let html = Html::parse_document(&resp.text().await?);

            // 英文标题、日文标题、父画廊
            let title = html.select_text("h1#gn").expect("xpath fail: h1#gn");
            let title_jp = html.select_text("h1#gj");
            let parent = html.select_attr("td.gdt2 a", "href").and_then(|s| s.parse().ok());

            // 画廊 tag
            let mut tags = IndexMap::new();
            let selector = selector!("div#taglist tr");
            for ele in html.select(&selector) {
                let namespace = ele
                    .select_text("td.tc")
                    .expect("xpath fail: td.tc")
                    .trim_matches(':')
                    .to_string();
                let tag = ele.select_texts("td div a");
                tags.insert(namespace, tag);
            }

            // 收藏数量
            let favorite = html.select_text("#favcount").expect("xpath fail: #favcount");
            let favorite = favorite.split(' ').next().unwrap().parse().unwrap();

            // 发布时间
            let posted = &html.select_texts("td.gdt2")[0];
            let posted = NaiveDateTime::parse_from_str(posted, "%Y-%m-%d %H:%M")?;

            // 每一页的 URL
            let pages = html.select_attrs("div#gdt a", "href");

            // 下一页的 URL
            let next_page = html.select_attr("table.ptb td:last-child a", "href");

            (title, title_jp, parent, tags, favorite, pages, posted, next_page)
        };

        while let Some(next_page_url) = &next_page {
            debug!(next_page_url);
            let headers = self.get_headers().await;
            let resp = send!(self.client.get(next_page_url).headers(headers))?;
            let html = Html::parse_document(&resp.text().await?);
            // 每一页的 URL
            pages.extend(html.select_attrs("div#gdt a", "href"));
            // 下一页的 URL
            next_page = html.select_attr("table.ptb td:last-child a", "href");
        }

        let pages = pages.into_iter().map(|s| s.parse()).collect::<Result<Vec<_>>>()?;
        info!("图片数量：{}", pages.len());

        let cover = url.cover();

        Ok(EhGallery {
            url: url.clone(),
            title,
            title_jp,
            parent,
            tags,
            favorite,
            pages,
            posted,
            cover,
        })
    }

    /// 获取画廊的某一页的图片的 fileindex 和实际地址和 nl
    #[tracing::instrument(skip(self))]
    pub async fn get_image_url(&self, page: &EhPageUrl) -> Result<(u32, String)> {
        let headers = self.get_headers().await;
        let resp = send!(self.client.get(&page.url()).headers(headers.clone()))?;
        let (url, nl, fileindex) = {
            let html = Html::parse_document(&resp.text().await?);
            let url = html.select_attr("img#img", "src").unwrap();
            let nl = html.select_attr("img#img", "onerror").and_then(extract_nl);
            let fileindex = extract_fileindex(&url).unwrap();
            (url, nl, fileindex)
        };

        return if send!(self.client.head(&url).headers(headers.clone())).is_ok() {
            Ok((fileindex, url))
        } else if nl.is_some() {
            let resp = send!(self.client.get(&page.with_nl(&nl.unwrap()).url()).headers(headers))?;
            let html = Html::parse_document(&resp.text().await?);
            let url = html.select_attr("img#img", "src").unwrap();
            Ok((fileindex, url))
        } else {
            Err(EhError::HaHUrlBroken(url))
        };
    }
}

fn extract_fileindex(url: &str) -> Option<u32> {
    static RE1: Lazy<Regex> = Lazy::new(|| Regex::new(r"fileindex=(?P<fileindex>\d+)").unwrap());
    static RE2: Lazy<Regex> = Lazy::new(|| Regex::new(r"/om/(?P<fileindex>\d+)/").unwrap());
    let captures = RE1.captures(url).or_else(|| RE2.captures(url))?;
    let fileindex = captures.name("fileindex")?.as_str().parse().ok()?;
    Some(fileindex)
}

fn extract_nl(onerror: String) -> Option<String> {
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"nl\('(?P<nl>.+)'\)").unwrap());
    let captures = RE.captures(&onerror)?;
    Some(captures.name("nl")?.as_str().to_string())
}
