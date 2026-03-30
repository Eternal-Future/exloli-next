use std::env;

use anyhow::Result;
use exloli_next::bot::start_dispatcher;
use exloli_next::config::{Config, ProxyMode, Site, CHANNEL_ID};
use exloli_next::ehentai::{EhClient, IgneousRefresher};
use exloli_next::tags::EhTagTransDB;
use exloli_next::uploader::ExloliUploader;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::new("./config.toml")?;
    CHANNEL_ID.set(config.telegram.channel_id.to_string()).unwrap();

    // NOTE: 全局数据库连接需要用这个变量初始化
    env::set_var("DATABASE_URL", &config.database_url);
    env::set_var("RUST_LOG", &config.log_level);

    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .unwrap();

    let trans = EhTagTransDB::new(&config.exhentai.trans_file);
    let ehentai = EhClient::new(&config.exhentai).await?;
    let bot = Bot::new(&config.telegram.token)
        .throttle(Default::default())
        .parse_mode(ParseMode::Html)
        .cache_me();
    let uploader =
        ExloliUploader::new(config.clone(), ehentai.clone(), bot.clone(), trans.clone()).await?;

    // 启动 igneous 刷新任务（如果需要）
    let igneous_task = if config.exhentai.site == Site::Exhentai 
        && config.exhentai.auto_refresh_igneous 
        && config.exhentai.proxy.mode != ProxyMode::None 
    {
        info!("启动 igneous 自动刷新任务");
        let refresher = IgneousRefresher::new(config.exhentai.clone())?;
        let ehentai_clone = ehentai.clone();
        
        Some(tokio::spawn(async move {
            loop {
                // 执行刷新
                if let Err(e) = refresher.refresh().await {
                    tracing::error!("刷新 igneous 失败: {}", e);
                } else {
                    // 刷新成功后更新 EhClient 的 cookie
                    let new_cookie = refresher.get_current_cookie().await;
                    ehentai_clone.update_cookie(new_cookie).await;
                }
                
                // 等待下次刷新（由 refresher.start() 内部处理）
                // 这里我们直接调用 start，它会一直运行
                if let Err(e) = refresher.clone().start().await {
                    tracing::error!("igneous 刷新任务出错: {}", e);
                    // 等待一段时间后重试
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                }
            }
        }))
    } else {
        if config.exhentai.site == Site::Exhentai && config.exhentai.auto_refresh_igneous {
            if config.exhentai.proxy.mode == ProxyMode::None {
                tracing::warn!("已启用 igneous 自动刷新，但未配置代理。");
            }
        }
        None
    };

    let t1 = {
        let uploader = uploader.clone();
        tokio::spawn(async move { uploader.start().await })
    };
    let t2 = {
        let trans = trans.clone();
        tokio::spawn(async move { start_dispatcher(config, uploader, bot, trans).await })
    };
    let t3 = tokio::spawn(async move { trans.start().await });

    if let Some(igneous_task) = igneous_task {
        tokio::try_join!(t1, t2, t3, igneous_task)?;
    } else {
        tokio::try_join!(t1, t2, t3)?;
    }

    Ok(())
}
