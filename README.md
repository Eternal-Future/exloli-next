#### 原库已归档，这个fork下来的库使用AI进行维护
# exloli-next

因为受不了当初乱写代码的自己而重写的新一代的 exloli 客户端

## 安装

### 通过 cargo

```bash
# 安装 rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# 激活 rust 环境
source $HOME/.cargo/env
# 安装 exloli-next
cargo install --git https://github.com/lolishinshi/exloli-next
# 测试是否安装成功
exloli-next --help
```

### 通过 docker

```bash
# 注：docker-compose 自行安装 
mkdir exloli-next && cd exloli-next
wget https://raw.githubusercontent.com/lolishinshi/exloli/master/docker-compose.yml
wget https://github.com/EhTagTranslation/Database/releases/download/v6.7880.1/db.text.json
touch db.sqlite db.sqlite-shm db.sqlite-wal
mv config.toml.example config.toml
docker-compose up -d
```

## 配置

请参考 config.toml.example

### OneBot / QQ 推送

OneBot 推送是可选功能，不影响原有 Telegram 推送。启用后，本项目会启动一个反向 WebSocket 服务，OneBot/NapCat 端需要作为客户端连接：

```text
ws://<host>:<port>/expush
```

配置项在 `[onebot]`：

```toml
[onebot]
enabled = true
listen_host = "0.0.0.0"
listen_port = 0
path = "/expush"
access_token = "change-me"
private_user_ids = [123456789]
group_ids = [987654321]
```

- `path` 固定为 `/expush`。
- `listen_port = 0` 时，首次启动会在 `30000-60000` 中随机选择一个未占用端口并写回配置文件。
- 启用 OneBot 时必须设置 `access_token`，连接时使用 `Authorization: Bearer <token>` 或 `?access_token=<token>`。
- 私聊和群聊都强制使用白名单；对应列表为空时，不会对该类型目标推送，也不会响应交互命令。
- QQ 私聊推送保留标签信息；QQ群推送不带标签，并会先发送本子的第一张图片作为预览图。
- 当前只支持两个 OneBot 交互命令：`#ping` 返回 `pong!`，`#latestbook` 返回最近一条发布结果。

## 从 exloli 迁移

直接运行即可，但是建议备份好数据库

## TODO

- 处理旧本子的投票：通过 /query 返回 OR 重新编辑频道消息添加投票 OR ？
- 标记坏图片，可以分为无效图片和广告图片，后者不会上传，两者均不会出现在 challenge 中
