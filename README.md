# WorkBuddy2API Rust

WorkBuddy 网关的 Rust 实现：单个静态二进制，无 Python 运行时依赖，面向 Linux VPS（Debian / Alpine，amd64 / arm64）。

本分支 `rust` 只包含 Rust 工作区，与 Python 历史完全分离（orphan 分支，没有共同祖先）：

| 分支 | 内容 |
|---|---|
| `rust` | 本 Rust 网关（工作区在仓库根目录） |
| `rust-port` | 移植时使用的 Python 参考实现 + 旧的 `rust/` 子目录快照，仅存档 |
| `main` | 跟随原作者上游的 Python 版本 |

行为基线：Python `477ba36`（v1.7.32），并选择性同步至 v1.8.0 / `4f7ae6f`，见 [docs/UPSTREAM_SYNC.md](docs/UPSTREAM_SYNC.md)。看板 HTML 和定价快照从该基线复制到 `assets/`，编译期嵌入。

当前版本仍为 **alpha**：核心转发路径可用并有离线契约测试，但不是 Python 版的完整功能替换，也没有在真实账号上做过长期验证。

## 已覆盖的核心

- Chat Completions、Responses、Anthropic Messages 的请求转换、流式和非流式响应；Messages 默认关闭。
- 工具调用、namespace/custom tools、reasoning、usage、SSE心跳、断流错误处理和受限缓冲。
- 多账号、会话亲和、加权选择、账号/API Key/全局并发、每日额度、熔断与模型冷却。
- 账号 JSON 导入/导出/编辑；OAuth、令牌刷新、余额读取通过独立管理模块实现。
- API Key和面板会话独立鉴权，密码PBKDF2，登录限速，敏感数据不写入响应日志。
- 内嵌原看板、模型目录、用量与定价历史查询、内存日志。
- Tokio异步服务、OpenSSL校验证书的HTTPS、HTTP CONNECT代理、按凭据隔离的连接池。

## 不能当作“已迁移”的部分

签到/成长/旅行/每日网页聊天调度、配额扫描和剩余额度预测、本地Web工具执行、实时OpenRouter调价、SQLite运行日志、桌面导入/配置器和自更新尚未覆盖。SOCKS代理也暂不支持。原看板仍有对应入口，调用会得到明确的“不支持”，不是假成功。

完整清单、与Python有意不同的行为、数据回滚规则见 [docs/MIGRATION.md](docs/MIGRATION.md)。实现规格在 `docs/spec/`，但规格说明某项功能不代表已经实现。

## Linux 源码构建

需要现代 stable Rust，建议与验证工具链一致使用 Rust 1.99；使用仓库内 `Cargo.lock`。

Debian：

```sh
sudo apt-get install build-essential perl pkg-config ca-certificates
cd workbuddy2api-hub   # git checkout rust
cargo build --release --locked -p wb2api
```

Alpine：

```sh
apk add build-base perl pkgconf linux-headers ca-certificates tzdata
cd workbuddy2api-hub   # git checkout rust
cargo build --release --locked -p wb2api
```

OpenSSL默认从源码静态链接；第一次构建耗时明显长于增量构建。验证产物是 `x86_64-unknown-linux-musl` 静态ELF，不要求部署机安装Python。ARM64尚未验证。

## 最小启动

先创建私有数据目录，将账号JSON放进 `accounts/`，或者启动后通过受保护的面板导入。**首次迁移用原数据目录的副本，不要让Python和Rust同时写同一套文件。**

```sh
mkdir -p accounts usage
chmod 700 accounts usage
export API_KEY='replace-with-a-long-random-api-key'
export WB_PANEL_PASSWORD='replace-with-a-different-strong-password'
export TZ='Asia/Shanghai'  # 与旧部署保持一致；影响每日额度边界
./wb2api --host 127.0.0.1 --port 8788 \
  --accounts-dir ./accounts --usage-dir ./usage
```

- 面板：`http://127.0.0.1:8788/`
- API：`http://127.0.0.1:8788/v1`
- 建议通过SSH隧道访问面板，或在配置TLS的反向代理后部署。
- 监听非loopback地址时，服务要求有效API鉴权和非默认面板密码；不会用`admin`直接暴露公网。
- `WB_PANEL_PASSWORD` 每次启动都会设置密码；用它完成首次配置后也可以改为由现有settings文件管理。
- 默认日志不输出API Key；`--lan`生成的launcher key保存在私有settings文件。
- 删除或禁用最后一个Key不会关闭鉴权。即使监听loopback，匿名API访问也必须显式设置`auth_disabled:true`且未配置launcher key。

请求示例：

```sh
curl http://127.0.0.1:8788/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"deepseek-v4.1-flash","messages":[{"role":"user","content":"Reply with OK"}],"stream":true}'
```

Messages须登录面板后显式启用 `enable_anthropic_messages`。仅有API Key不能修改面板设置。

上游新增的桌面请求兼容模式可用 `WB_DESKTOP_CHAT_COMPAT=1` 显式开启（默认关闭，设置后重启；`true`不会开启）。它只调整WorkBuddy桌面chat的应用层标头，不修改TLS、正文或自动切换账号身份。429带明确重置时间时会写入并在重启后恢复`accounts/model_windows.json`；这不启用自动配额探测。

## Docker / systemd

GitHub Actions 定义位于 `.github/workflows/ci.yml`，对 AMD64、ARM64 使用原生 Linux runner，依次执行完整测试、假上游冒烟、Docker构建和容器运行检查，全部通过才发布独立GHCR镜像。触发规则、标签和拉取方式见 [GitHub Actions说明](deploy/GITHUB_ACTIONS.md)。

在仓库根目录构建：

```sh
docker build -f deploy/Dockerfile -t wb2api:local .
```

`deploy/compose.yml` 默认仅发布到 `127.0.0.1:8788`，要求设置 `API_KEY`、`WB_PANEL_PASSWORD`，使用非root用户和独立数据卷。先复制并填写 `deploy/.env.example`，不要把真实 `.env` 提交到Git。

systemd示例为 `deploy/wb2api.service` 和 `wb2api.env.example`。它们是配置模板，不会自动修改系统；需自行创建服务用户并让其拥有数据目录。

本次开发环境没有Docker/WSL，Docker和systemd仅做静态配置检查，**未声称已在本机启动容器或Linux服务**。

## 测试与复现

Rust单元/契约测试：

```sh
cargo test --workspace --locked
```

整机冒烟测试（只连接loopback假上游、临时虚构账号）：

```sh
python tests/gateway_smoke.py --binary /absolute/path/to/wb2api \
  --report tests/gateway-smoke-results.json
python tests/upstream_sync_smoke.py --binary /absolute/path/to/wb2api \
  --report tests/upstream-sync-smoke-results.json
```

Python基线与黄金样本见 `tests/baseline/README.md`；重新生成需要把 `WB_PY_ROOT` 指向 `rust-port` 分支的 Python 检出。基线运行时阻止非loopback连接，避免测试意外启动价格刷新访问外网。

`--offline` 禁止网关后台刷新，但普通API请求仍可按账号访问上游。隐藏的 `--test-upstream` 只允许HTTP数字loopback地址且要求网关也监听loopback；仅用于本地契约测试，不是生产上游配置。

最终验证次数和产物信息记录在 [docs/VALIDATION.md](docs/VALIDATION.md)。代码相容性测试不能证明真实上游不会拒绝请求，也不能证明Rust已带来某个倍数的性能提升。

## 项目结构

```text
assets/             编译期嵌入的看板HTML与定价快照
crates/wbcore       JSON兼容、时间、SSE、工具调用、用量公共逻辑
crates/wbhttp       异步HTTP/TLS与连接池
crates/wbsettings   设置、面板密码/会话、API Key
crates/wbaccounts   账号状态、选择、持久化、上游请求构造
crates/wbcatalog    模型目录与能力/套餐信息
crates/wbchat       Chat请求规范化和流式输出
crates/wbtranslate  Responses/Anthropic协议转换
crates/wbusage      用量聚合、历史定价
gateway/           HTTP路由、调度集成、管理接口
deploy/            Dockerfile、compose、systemd模板
docs/              迁移范围、验证记录、行为规格
tests/             整机冒烟测试与Python黄金样本
```
