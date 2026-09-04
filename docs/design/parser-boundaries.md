# 解析计入边界

本文约束已接入采集源「扫哪里、计什么、明确不计什么」。系统拓扑、ingest 契约与费用估算见 [architecture.md](architecture.md)。解析器内部算法以 `crates/parsers` 为准。

| 本文覆盖 | 本文不覆盖 |
| --- | --- |
| 已接入源的扫描根与计入规则 | 未接入源的实现方案 |
| 本项目自定的计入 / 不计与不做 | crate 拆分、缓存文件格式 |
| 重开某条边界前须满足的条件 | 工具日志格式的跟踪说明 |

后续若要把某条「不计」改成「计」，先改本文再改代码。边界以本文为准，不跟随外部统计工具的覆盖面或实现复杂度。

## 判定原则

定位是用量可见性，不是结算凭证。计入规则优先满足：

1. **数字不可虚高**：同一段 token 被父会话与子文件各计一次，会抬高看板费用估算；漏计一块增量可接受。
2. **单文件可解析**：每个 rollout / jsonl 独立出数。禁止为对齐父会话而引入跨文件索引、fingerprint 匹配或禁止增量尾读的第二套解析器。
3. **边界自洽**：扫根、字段与是否计入由本文给出；新增源的准入见 [architecture.md](architecture.md) 本阶段范围。

`cache_read` 与 `cache_creation` 必须分字段保留。Anthropic `cache_creation` 约基价 1.25×、`cache_read` 约 0.1×，单价差 12.5 倍，折进 `input` 后费用无法回补。Codex 无 `cache_creation`，该字段为 0。`total_tokens` 为五项之和（含 `cache_read` / `cache_creation`）。

桶时间按 UTC 半小时对齐。Claude / Codex / Grok 的主机身份由 ingest token 派生，不进入解析维度。Cursor 是账号级源：解析器带上 `account_hash`，ingest 改写为 `acct:<hash>`。

## 已接入源

某一源失败不影响其它源。扫描根可通过 `AdapterEnv` 与对应环境变量覆盖；采集端默认跟环境变量与 `$HOME`，不另开第二套 Home，除非产品增加显式开关。

### Claude Code（`claude-code`）

**扫描根**：`~/.claude`、`$CLAUDE_CONFIG_DIR`、带 `projects/` 或 `transcripts/` 的 `~/.claude-*`。

**计入**：`projects/` 出 token 与 session；`transcripts/` 只补尚未在 `projects/` 出现的 session，不计 token。同一 `session_id` 多副本按体积与 mtime 取最完整一份。只从 `type=assistant` 且带 `message.usage` 的记录取 token；`message.id` + `requestId`（否则 `uuid`）去重，保留用量更大的一条。

**不计**：Claude Desktop Cowork 在 Electron user-data 下的 `local-agent-mode-sessions/**/.claude`。该路径属于 GUI 私有目录，不并入 `~/.claude` 扫描。

### Codex（`codex`）

**扫描根**：`$CODEX_HOME/sessions/` 与 `archived_sessions/`（缺省 `~/.codex`）。同一 `session_id` 在 live / archive / 多 root 下只留最完整一份物理文件。

**计入**：普通 rollout 的 `event_msg` / `token_count`。优先 `last_token_usage`，否则对 `total_token_usage` 做非负 delta；累计 `total_tokens` 未变则跳过。`input` 扣除 `cache_read`，`output` 扣除 reasoning。

**不计 token、仍出 session**：首条 `session_meta` 标明 fork 或 sub-agent 的文件——`forked_from_id` 有值，或 `thread_source` / `source` / `parent_thread_id` 表明 sub-agent。此类文件含父会话复制历史；本阶段不把复制段与子会话自身增量拆开，因此整文件不产生 Bucket。父会话文件仍按普通规则计入。看板可能看到子会话条目，其用量为 0。

**不做**：跨文件 replay 拆分（按 payload fingerprint 对齐父会话、对全部 session 建索引再裁子文件、对 fork / sub-agent 文件禁止增量尾读）。与「单文件可解析」冲突。

### Grok（`grok`）

**扫描根**：`$GROK_HOME/sessions/<encoded-cwd>/<session-id>/`（缺省 `~/.grok`）。token 来自 `updates.jsonl` 的 `turn_completed.usage`；`modelUsage` 非空则按模型拆。`input` 扣除 `cache_read`，`output` 扣除 reasoning；`cacheCreationTokens` 写入 `cache_creation_input_tokens`，不折进 `input`。

**计入**：普通会话（无 `session_kind` 或非 `subagent`）的 `turn_completed.usage`。父会话 `usage` 已含子用量。

**不计 token、仍出 session**：`summary.json` 的 `session_kind == "subagent"`。此类目录与父会话同级；本阶段不读父目录 `subagents/` sidecar，也不按 fingerprint 从父用量里拆子增量，因此整目录不产生 Bucket。父会话文件仍按普通规则计入。看板可能看到子会话条目，其用量为 0。

**不计**：encoded cwd 超长时 group 目录 sidecar `.cwd`。有 `summary.json` 的 `info.cwd` 或 `git_root_dir` 时不依赖该文件。未出现真实超长路径前不增加扫描分支。

**不做**：跨文件 replay 拆分（按 fingerprint 对齐父会话、从父用量减去子增量）。与「单文件可解析」冲突。

### Cursor（`cursor`）

**扫描根**：本机 Cursor user-data 里的 `state.vscdb`（Linux `$XDG_CONFIG_HOME/Cursor/User/globalStorage/state.vscdb` 或 `~/.config/Cursor/…`；macOS Application Support；Windows `%APPDATA%\Cursor`）。可用 `CURSOR_STATE_DB_PATH` 覆盖。有该文件即视为已安装。只读打开，只取 `cursorAuth/accessToken` 与 `cursorAuth/cachedEmail`，禁止拷贝整库，禁止写回。

**采集为手动加入制**：解析器只采集已加入的账号（数据目录 `cursor-accounts.toml`，权限 0600）。IDE 登录只自动扫描并在面板展示，不自动加入采集；经「导入当前 IDE 登录」、粘贴 JWT / `WorkosCursorSessionToken`、或上传账号导出 JSON（只提取 `access_token` / `accessToken` 与邮箱）加入后才计入，不登录 IDE 也能采。采集严格使用加入时存储的那份 JWT，不跟随 IDE 换发、不自动覆盖；检测到 IDE 已换发时面板仅提示并提供手动「更新凭证」。无已加入账号时该源视为未启用（skipped，不算错误、不剪增量 state）。套餐消耗现拉 `GET cursor.com/api/usage-summary`，Bot/Sand 配额另走 `api2.cursor.sh` 原生 RPC `GetSandUsageStatus`（Bearer 原生 token；`type=web` 的网站 JWT 调不通，Bot 字段留空不算错误）。面板卡片 API / Auto / Bot 只标百分比；已用取 `plan.used` 美分，赠额另标。本机面板在页面可见时每 60 秒主动拉取画在卡片上，不写入 secrets 文件；另由 agent 在 Cursor 同步周期拉取一次归一化快照（无凭证、无原始 JSON）随 ingest 上报，见 [architecture.md](architecture.md) 数据契约。套餐展示对与 IDE 同号的账号可用 IDE 实时 token（仅展示，采集凭证不受影响）。失败则卡片标异常并停止该账号自动拉取，手动刷新成功后恢复；页面不可见时不拉。v1 不调用 Cursor oauth 续期；JWT `exp` 约 60 天（从签发起算），401 后重新导入。账号登录会话走 `GET /api/auth/sessions` 与 `POST /api/auth/sessions/revoke`（`type` 为数字枚举：WEB=1 / CLIENT=2 / MOBILE=10 / CHROME_EXTENSION=11），面板可查看并撤销；采集端自己那条由 JWT `time` 声明与 `createdAt` 同秒认出并标记，撤销它会使该账号采集 401，前端二次确认但不拦。会话撤销为服务端异步生效，只在本机面板操作，不经 ingest。本机面板在页面可见时每 60 秒主动拉取画在卡片上，不写入 secrets 文件；另由 agent 在 Cursor 同步周期拉取一次归一化快照（无凭证、无原始 JSON）随 ingest 上报，见 [architecture.md](architecture.md) 数据契约。套餐展示对与 IDE 同号的账号可用 IDE 实时 token（仅展示，采集凭证不受影响）。失败则卡片标异常并停止该账号自动拉取，手动刷新成功后恢复；页面不可见时不拉。v1 不调用 Cursor oauth 续期；JWT `exp` 约 60 天（从签发起算），401 后重新导入。

**计入**：`GET cursor.com/api/dashboard/export-usage-events-csv?strategy=tokens` 的全量 CSV。`Input (w/o Cache Write)` → `input`；`Input (w/ Cache Write)` → `cache_creation`（两列不相交，禁止折进 `input`）；`Cache Read` → `cache_read`；`Output Tokens` → `output`。`project = unknown`。五项全 0 的行跳过。`Cost` 忽略。每个桶带同一 `account_hash` / `account_label`（邮箱优先 `cachedEmail`，否则 JWT `email`，再否则短 hash）。CSV 禁止按滚动时间窗截断；每账号可设「统计起始」**固定 cutoff**（默认加入时刻，可改任意日期或全部历史），cutoff 前的条目不计入——固定点不会像滚动窗那样让桶反复进出 live 集。

**不计**：本地 `bubbleId`、`agent-transcripts`、session。不做 hook，不加采集开关。

**失败**：某一账号 401/403 记 warning（请重新导入），不拖垮其它账号；全部失败才 `skipped`（不入 `ok_sources`、不剪增量 state）。超时 / 5xx / 网络对单账号同样跳过。部分成功时只按成功账号的 `account_hash` 修剪 state。

## 未接入

新增采集源的准入写在 [architecture.md](architecture.md) 本阶段范围，不在本文展开实现。不追求全量 AI 编码工具覆盖。

## 重开条件

同时满足再改对应小节，否则保持本文：

| 边界 | 重开条件 |
| --- | --- |
| Codex fork / sub-agent 整文件不计 token | 目标机器上此类文件成为用量主体，且有对账级精度需求；实现仍须保持单文件可解析，或单独论证跨文件索引的维护成本 |
| Claude Desktop Cowork 扫描 | 存在以 Cowork 为主要日志位置的使用者，且扫描范围可限制在可枚举的 app-data 根下 |
| Grok subagent 不计 token | 父会话 `turn_completed.usage` 不再聚合子用量，跳过子目录会漏计 |
| Grok `.cwd` | 出现 summary 无 cwd、且 group 目录带 `.cwd` 的真实会话 |

重开后删除或改写本节对应行，不另留「已过时」段落。
