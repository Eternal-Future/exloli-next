use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use regex::Regex;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::{Response as HttpResponse, StatusCode};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::config::{Config, OneBot};
use crate::database::{GalleryEntity, ImageEntity, TelegraphEntity};
use crate::ehentai::GalleryInfo;
use crate::tags::EhTagTransDB;

#[derive(Clone, Debug)]
pub struct OneBotHub {
    config: OneBot,
    trans: EhTagTransDB,
    sessions: Arc<Mutex<Vec<mpsc::UnboundedSender<Message>>>>,
}

impl OneBotHub {
    pub fn new(config: OneBot, trans: EhTagTransDB) -> Self {
        Self { config, trans, sessions: Arc::new(Mutex::new(Vec::new())) }
    }

    pub fn disabled(trans: EhTagTransDB) -> Self {
        Self::new(OneBot::default(), trans)
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub async fn start(self) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let addr = format!("{}:{}", self.config.listen_host, self.config.listen_port);
        let listener = TcpListener::bind(&addr).await?;
        info!("OneBot reverse WebSocket listening on ws://{}/expush", addr);

        loop {
            let (stream, peer) = listener.accept().await?;
            let hub = self.clone();
            tokio::spawn(async move {
                if let Err(err) = hub.accept(stream).await {
                    warn!("OneBot connection from {} closed: {}", peer, err);
                }
            });
        }
    }

    pub async fn push_gallery<T: GalleryInfo>(&self, gallery: &T, article: &str) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let preview = first_image_url(gallery.url().id()).await?;
        let private_text = render_private_message(gallery, article, &self.trans);
        let group_text = render_group_message(gallery, article);

        for user_id in &self.config.private_user_ids {
            self.broadcast_action(
                "send_private_msg",
                json!({
                    "user_id": user_id,
                    "message": message_segments(preview.as_deref(), &private_text),
                }),
            )
            .await;
        }

        for group_id in &self.config.group_ids {
            self.broadcast_action(
                "send_group_msg",
                json!({
                    "group_id": group_id,
                    "message": message_segments(preview.as_deref(), &group_text),
                }),
            )
            .await;
        }

        Ok(())
    }

    async fn accept(&self, stream: tokio::net::TcpStream) -> Result<()> {
        let expected_path = self.config.path.clone();
        let access_token = self.config.access_token.clone();
        let ws = tokio_tungstenite::accept_hdr_async(
            stream,
            move |request: &Request, response: Response| {
                validate_request(request, &expected_path, &access_token).map(|_| response)
            },
        )
        .await?;

        let (mut writer, mut reader) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.sessions.lock().await.push(tx.clone());

        let writer_task = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if writer.send(message).await.is_err() {
                    break;
                }
            }
        });

        while let Some(message) = reader.next().await {
            match message? {
                Message::Text(text) => self.handle_event(&tx, &text).await?,
                Message::Close(_) => break,
                _ => {}
            }
        }

        writer_task.abort();
        self.sessions.lock().await.retain(|session| !session.is_closed());
        Ok(())
    }

    async fn handle_event(&self, tx: &mpsc::UnboundedSender<Message>, text: &str) -> Result<()> {
        let value: Value = serde_json::from_str(text)?;
        if value.get("post_type").and_then(Value::as_str) != Some("message") {
            return Ok(());
        }

        let Some(raw_message) = raw_message(&value) else {
            return Ok(());
        };
        if raw_message != "#ping" && raw_message != "#latestbook" {
            return Ok(());
        }

        let Some(target) = OneBotTarget::from_event(&value) else {
            return Ok(());
        };
        if !self.is_whitelisted(&target) {
            return Ok(());
        }

        let reply = if raw_message == "#ping" {
            "pong!".to_string()
        } else {
            self.latest_book_message(target).await?
        };
        self.send_reply(tx, target, reply).await;
        Ok(())
    }

    async fn latest_book_message(&self, target: OneBotTarget) -> Result<String> {
        let gallery = GalleryEntity::latest_published()
            .await?
            .ok_or_else(|| anyhow!("未找到最近发布记录"))?;
        let telegraph = TelegraphEntity::get(gallery.id)
            .await?
            .ok_or_else(|| anyhow!("未找到 Telegraph 记录"))?;
        Ok(match target {
            OneBotTarget::Private(_) => {
                render_private_message(&gallery, &telegraph.url, &self.trans)
            }
            OneBotTarget::Group(_) => render_group_message(&gallery, &telegraph.url),
        })
    }

    async fn send_reply(
        &self,
        tx: &mpsc::UnboundedSender<Message>,
        target: OneBotTarget,
        text: String,
    ) {
        let (action, params) = match target {
            OneBotTarget::Private(user_id) => {
                ("send_private_msg", json!({ "user_id": user_id, "message": text }))
            }
            OneBotTarget::Group(group_id) => {
                ("send_group_msg", json!({ "group_id": group_id, "message": text }))
            }
        };
        let payload = json!({ "action": action, "params": params, "echo": echo(action) });
        if tx.send(Message::Text(payload.to_string())).is_err() {
            warn!("OneBot reply dropped because connection is closed");
        }
    }

    async fn broadcast_action(&self, action: &str, params: Value) {
        let payload = Message::Text(
            json!({ "action": action, "params": params, "echo": echo(action) }).to_string(),
        );
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|session| !session.is_closed());
        if sessions.is_empty() {
            warn!("OneBot push skipped: no active reverse WebSocket connection");
            return;
        }
        for session in sessions.iter() {
            if session.send(payload.clone()).is_err() {
                warn!("OneBot push dropped because a connection is closed");
            }
        }
    }

    fn is_whitelisted(&self, target: &OneBotTarget) -> bool {
        match target {
            OneBotTarget::Private(user_id) => self.config.private_user_ids.contains(user_id),
            OneBotTarget::Group(group_id) => self.config.group_ids.contains(group_id),
        }
    }
}

pub async fn start_onebot(config: Config, hub: OneBotHub) {
    if !config.onebot.enabled {
        return;
    }
    if config.onebot.private_user_ids.is_empty() && config.onebot.group_ids.is_empty() {
        warn!("OneBot enabled but both private_user_ids and group_ids are empty; push and interaction disabled");
    }
    if config.onebot.access_token.is_empty() {
        warn!("OneBot access_token is empty; public reverse WebSocket should configure a token");
    }
    if let Err(err) = hub.start().await {
        error!("OneBot server stopped: {}", err);
    }
}

fn validate_request(
    request: &Request,
    expected_path: &str,
    access_token: &str,
) -> std::result::Result<(), HttpResponse<Option<String>>> {
    if request.uri().path() != expected_path {
        return Err(response(StatusCode::NOT_FOUND, "invalid OneBot path"));
    }
    if access_token.is_empty()
        || token_from_header(request) == Some(access_token)
        || token_from_query(request) == Some(access_token)
    {
        return Ok(());
    }
    Err(response(StatusCode::UNAUTHORIZED, "invalid OneBot access token"))
}

fn response(status: StatusCode, body: &str) -> HttpResponse<Option<String>> {
    HttpResponse::builder().status(status).body(Some(body.to_string())).unwrap()
}

fn token_from_header(request: &Request) -> Option<&str> {
    request.headers().get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")
}

fn token_from_query(request: &Request) -> Option<&str> {
    request.uri().query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "access_token").then_some(value)
    })
}

fn raw_message(value: &Value) -> Option<String> {
    if let Some(raw) = value.get("raw_message").and_then(Value::as_str) {
        return Some(raw.trim().to_string());
    }
    value.get("message")?.as_array().map(|segments| {
        segments
            .iter()
            .filter(|segment| segment.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|segment| segment.get("data")?.get("text")?.as_str())
            .collect::<String>()
            .trim()
            .to_string()
    })
}

#[derive(Clone, Copy)]
enum OneBotTarget {
    Private(i64),
    Group(i64),
}

impl OneBotTarget {
    fn from_event(value: &Value) -> Option<Self> {
        match value.get("message_type").and_then(Value::as_str)? {
            "private" => value.get("user_id").and_then(Value::as_i64).map(Self::Private),
            "group" => value.get("group_id").and_then(Value::as_i64).map(Self::Group),
            _ => None,
        }
    }
}

fn message_segments(preview: Option<&str>, text: &str) -> Vec<Value> {
    let mut segments = Vec::new();
    if let Some(preview) = preview {
        segments.push(json!({ "type": "image", "data": { "file": preview } }));
    }
    segments.push(json!({ "type": "text", "data": { "text": text } }));
    segments
}

fn render_private_message<T: GalleryInfo>(
    gallery: &T,
    article: &str,
    trans: &EhTagTransDB,
) -> String {
    let mut text = render_tags(gallery, trans);
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("预览：{}\n", article));
    text.push_str(&format!("原始地址：{}", gallery.url().url()));
    text
}

fn render_group_message<T: GalleryInfo>(gallery: &T, article: &str) -> String {
    format!("📚 {}\n\n🔗 预览：{}\n🌐 原始：{}", gallery.title(), article, gallery.url().url())
}

fn render_tags<T: GalleryInfo>(gallery: &T, trans: &EhTagTransDB) -> String {
    let re = Regex::new("[-/· ]").unwrap();
    trans
        .trans_tags(gallery.tags())
        .into_iter()
        .map(|(namespace, tags)| {
            let tags = tags
                .iter()
                .map(|tag| format!("#{}", re.replace_all(tag, "_")))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{}：{}", namespace, tags)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn first_image_url(gallery_id: i32) -> Result<Option<String>> {
    Ok(ImageEntity::get_by_gallery_id(gallery_id)
        .await?
        .into_iter()
        .next()
        .map(|image| image.url()))
}

fn echo(action: &str) -> String {
    format!("exloli-next-{action}-{}", chrono::Utc::now().timestamp_millis())
}
