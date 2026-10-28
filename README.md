# WorkBuddy2API Rust

WorkBuddy / CodeBuddy 账号池网关的 Rust 实现。对外提供 OpenAI Chat Completions、OpenAI Responses 和 Anthropic Messages 三种接口，内置原版管理看板。单个静态二进制，运行时不需要 Python。

- 镜像：`ghcr.io/d3-3109/workbuddy2api-hub-rust`（linux/amd64、linux/arm64），由 GitHub Actions 测试后发布。
- 当前版本：0.1.x alpha。核心转发可用，有完整离线契约测试；部分 Python 版后台任务尚未移植，见[功能范围](#功能范围)。

## 用 Docker Compose 启动

需要 Docker 和 Docker Compose v2。不需要克隆源码，只需要两个文件：

```sh
mkdir wb2api && cd wb2api
curl -fsSLO https://raw.githubusercontent.com/D3-3109/workbuddy2api-hub/rust/docker-compose.yml
curl -fsSL  https://raw.githubusercontent.com/D3-3109/workbuddy2api-hub/rust/.env.example -o .env
chmod 600 .env
```

编辑 `.env`，填入两个**不同**的随机值（可用 `openssl rand -hex 24` 生成）：

```sh
API_KEY=替换为随机字符串                  # 客户端调用 /v1 用的密钥
WB_PANEL_PASSWORD=替换为另一个随机字符串   # 看板登录密码，不能是 admin
TZ=Asia/Shanghai
```

启动：

```sh
docker compose up -d
docker compose ps        # STATUS 显示 healthy 即可
docker compose logs -f   # 查看日志
```

- 看板：<http://127.0.0.1:8788/>，用 `WB_PANEL_PASSWORD` 登录，在「账号」页导入账号 JSON 或走 OAuth 登录。
- API：`http://127.0.0.1:8788/v1`，鉴权头 `Authorization: Bearer <API_KEY>`。

测试一次请求（需要先导入至少一个账号）：

```sh
curl http://127.0.0.1:8788/v1/chat/completions \
  -H "Authorization: Bearer $(grep ^API_KEY= .env | cut -d= -f2)" \
  -H 'Content-Type: application/json' \
  -d '{"model":"deepseek-v4.1-flash","messages":[{"role":"user","content":"Reply with OK"}],"stream":true}'
```

### docker-compose.yml

即仓库根目录的 [docker-compose.yml](docker-compose.yml)：

```yaml
services:
  wb2api:
    image: ${WB_IMAGE:-ghcr.io/d3-3109/workbuddy2api-hub-rust:edge}
    container_name: wb2api
    restart: unless-stopped
    init: true
    user: "10001:10001"
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    environment:
      API_KEY: ${API_KEY:?set API_KEY in .env}
      WB_PANEL_PASSWORD: ${WB_PANEL_PASSWORD:?set WB_PANEL_PASSWORD in .env}
      TZ: ${TZ:-Asia/Shanghai}
    ports:
      - "${WB_BIND_ADDRESS:-127.0.0.1}:${WB_PORT:-8788}:8788"
    volumes:
      - wb2api-accounts:/data/accounts
      - wb2api-usage:/data/usage
    tmpfs:
      - /tmp:rw,noexec,nosuid,nodev,size=16m,mode=1777
    stop_grace_period: 40s
    healthcheck:
      test: ["CMD", "wget", "-q", "-T", "3", "-O", "/dev/null", "http://127.0.0.1:8788/health"]
      interval: 30s
      timeout: 5s
      start_period: 10s
      retries: 3

volumes:
  wb2api-accounts:
  wb2api-usage:
```

### 远程访问

默认只监听宿主机的 `127.0.0.1`。从别的机器访问有两种方式：

- **SSH 隧道**（推荐用于看板）：`ssh -L 8788:127.0.0.1:8788 user@vps`，然后在本地打开 <http://127.0.0.1:8788/>。
- **反向代理**：用 Caddy / Nginx 在 443 上加 TLS，反代到 `127.0.0.1:8788`。Nginx 需要为流式接口关闭缓冲：`proxy_buffering off;`。

也可以在 `.env` 里设置 `WB_BIND_ADDRESS=0.0.0.0` 直接暴露端口。网关此时强制要求 API Key 和非默认面板密码，但流量是明文 HTTP，不建议在公网这样用。

### 数据、备份与升级

账号、设置、API Key 保存在卷 `wb2api-accounts`，用量记录保存在卷 `wb2api-usage`。卷名前缀（下例中的 `wb2api_`）来自所在目录名，可用 `docker volume ls` 确认。

```sh
# 备份
docker run --rm -v wb2api_wb2api-accounts:/a -v wb2api_wb2api-usage:/u -v "$PWD":/backup \
  alpine tar czf /backup/wb2api-backup-$(date +%F).tgz -C / a u

# 升级到最新 edge
docker compose pull && docker compose up -d
```

`edge` 标签跟随 `rust` 分支，每次 CI 通过都会更新。需要固定版本时，在 `.env` 里设置 `WB_IMAGE=ghcr.io/d3-3109/workbuddy2api-hub-rust:sha-<完整提交号>`。可用标签见 [GHCR 包页面](https://github.com/D3-3109/workbuddy2api-hub/pkgs/container/workbuddy2api-hub-rust)。

### 从 Python 版迁移

Python 版的 `accounts/` 目录（账号 JSON、`settings.json`）和 `usage/` 目录格式兼容，拷进卷即可。**先停掉 Python 版，用数据副本测试，两者不能同时写同一套文件。** 时区保持和旧部署一致，否则每日额度的日界线会错位。

```sh
docker compose up -d && docker compose stop      # 先创建空卷
docker run --rm -v wb2api_wb2api-accounts:/a -v /path/to/old/accounts:/src:ro alpine \
  sh -c 'cp -a /src/. /a/ && chown -R 10001:10001 /a && chmod 700 /a'
docker compose start
```

`usage/` 同理拷到 `wb2api_wb2api-usage`。完整规则和回滚方法见 [docs/MIGRATION.md](docs/MIGRATION.md)。

## 配置

| 环境变量 | 说明 |
|---|---|
| `API_KEY` | 启动密钥。只在面板里没有任何 API Key 时生效；在面板创建 Key 后请改用面板的 Key |
| `WB_PANEL_PASSWORD` | 每次启动都会把面板密码设为这个值 |
| `TZ` | 本地时区，影响每日额度和用量统计的日界线 |
| `WB_MAX_CONCURRENT_CHAT` | 全局并发 chat 上限，默认 32 |
| `WB_CHAT_SLOT_WAIT` | 并发已满时等待空位的秒数，默认 30 |
| `WB_ENABLE_ANTHROPIC_MESSAGES` | 设为 `1` 开启 `/v1/messages`（也可在面板设置里开启） |
| `WB_DESKTOP_CHAT_COMPAT` | 设为 `1` 使用桌面客户端兼容的请求标头 |
| `WB_TRUSTED_PROXIES` | 反向代理 IP，逗号分隔；登录限速据此识别真实客户端 IP |

在 `docker-compose.yml` 的 `environment:` 下添加，然后 `docker compose up -d` 重建容器。模型别名、每日额度、代理槽、账号并发等其余选项在看板「设置」页修改，保存在 `settings.json`。

## 接口

| 路径 | 说明 |
|---|---|
| `POST /v1/chat/completions` | OpenAI Chat Completions，支持流式、工具调用、reasoning |
| `POST /v1/responses`、`/v1/responses/compact` | OpenAI Responses |
| `POST /v1/messages`、`/v1/messages/count_tokens` | Anthropic Messages，默认关闭 |
| `GET /v1/models` | 模型列表 |
| `/u/<账号 uid 或昵称>/v1/...` | 固定使用某个账号 |
| `GET /health` | 健康检查，无需鉴权 |

## 功能范围

已实现：三种协议的转换与流式输出；多账号选择、会话亲和、加权轮换；账号 / API Key / 全局并发限制；每日额度、熔断与模型冷却；账号导入导出与 OAuth 登录；令牌自动刷新；余额查询；看板、用量与定价历史；HTTP CONNECT 代理。

尚未移植：签到/成长/旅行/每日网页聊天等后台任务、配额扫描与余量预测、本地 Web 搜索工具、在线调价、SQLite 日志、SOCKS 代理、自更新。看板上对应按钮会返回明确的「不支持」。完整清单与有意的行为差异见 [docs/MIGRATION.md](docs/MIGRATION.md)。

## 镜像构建（GitHub Actions）

[.github/workflows/ci.yml](.github/workflows/ci.yml) 在每次推送 `rust` 分支时运行，amd64 和 arm64 各用一台原生 Linux runner：

1. `cargo fmt --check`、`cargo clippy -D warnings`、全部单元/契约测试；
2. 用本地假上游跑整机冒烟测试；
3. 构建 Alpine + musl 静态链接镜像，并对**这份镜像**做容器测试（非 root、只读根文件系统、鉴权、看板、优雅退出）；
4. 两个架构都通过后，推送这两份已测试的镜像并合成多架构清单，打 `edge` 和 `sha-<提交号>` 标签。

推送 `rust-v<版本号>` 标签会额外发布同名版本标签，版本号须与 `Cargo.toml` 一致。也可以在 Actions 页面手动运行（Run workflow）。细节见 [deploy/GITHUB_ACTIONS.md](deploy/GITHUB_ACTIONS.md)。

## 从源码构建

```sh
git clone -b rust https://github.com/D3-3109/workbuddy2api-hub.git
cd workbuddy2api-hub
cargo build --release --locked -p wb2api      # 需要 Rust 1.99+、perl、make、C 编译器
./target/release/wb2api --help
```

本地构建镜像：`docker build -f deploy/Dockerfile -t wb2api:local .`；`deploy/compose.yml` 是从源码构建的 compose 版本。systemd 模板见 `deploy/wb2api.service`。

测试：

```sh
cargo test --workspace --locked
python tests/gateway_smoke.py --binary "$PWD/target/debug/wb2api"
python tests/upstream_sync_smoke.py --binary "$PWD/target/debug/wb2api"
```

冒烟测试只连接本机假上游，使用临时虚构账号。

## 性能

相对 `rust-port` 上的初版移植，本分支重写了转发热路径、上游连接池、看板与用量接口缓存和日志写入，接口输出不变。本机回环假上游压测：32 并发流式吞吐 412 → 6,450 req/s，p99 延迟 116 → 8 ms。方法和完整数据见 [docs/PERFORMANCE.md](docs/PERFORMANCE.md)，压测工具在 `bench/`。这些数字不代表真实上游下的表现，真实延迟主要取决于上游。

## 仓库结构

```text
assets/      编译期嵌入的看板 HTML 与定价快照
crates/      wbcore wbhttp wbsettings wbaccounts wbcatalog wbchat wbtranslate wbusage
gateway/     wb2api 可执行文件：HTTP 服务、路由、调度、管理接口
deploy/      Dockerfile、源码构建版 compose、systemd 模板、CI 说明
docs/        迁移范围、验证记录、性能记录、行为规格
tests/       整机冒烟测试与 Python 黄金样本
bench/       离线压测工具（独立 crate）
```

分支：`rust` 是本 Rust 网关，与 Python 历史完全分离；`rust-port` 是移植时的 Python 参考实现快照；`main` 跟随原作者上游。

## 许可与声明

MIT，见 [LICENSE](LICENSE) 与 [NOTICE.md](NOTICE.md)。行为、看板与定价数据来自 [ardeyouxipianyi/workbuddy2api-hub](https://github.com/ardeyouxipianyi/workbuddy2api-hub) `477ba36`。本项目与 WorkBuddy、CodeBuddy、OpenAI、Anthropic 无关联；请只使用你有权访问的账号，并遵守相应服务条款与额度限制。
