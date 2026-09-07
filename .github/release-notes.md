<!-- 每次发布就地替换「本次变更」一节，历史版本由 git log 与既往 Release 承载，本文件不累积。 -->
## 本次变更（v0.5.0）

**采集端：Cursor 账号设置页。**

- 每个账号独立设置：面板打开时的套餐用量自动刷新可开关、可配周期。
- 登录会话逐条锁定为可信设备；可开关、可配周期的后台会话监控，自动撤销未锁定的新会话。检查状态与事件落 `cache/cursor-session-guard.json`，面板可查，不含凭证。

**看板：Cursor 账号卡片可归档。**

- 卡片上「归档」手动折叠；归档之后出现新用量桶自动恢复，快照上报不解归档。
- 快照超过 `cursor_stale_days`（dash.toml，默认 7 天）未更新的账号自动折叠到「已归档」区。
- 已归档卡片展示该账号有记录以来的累计消耗与 token 数（费用为估算）。

**Cursor 套餐卡片简化（破坏性）。**

- API / Auto / Bot 只标百分比；新增「已用量」= `plan.used` + `breakdown.bonus`，即本账期总消耗；「凭证有效期剩余」改为「凭证剩余」。
- 信用余额（credit grants）链路整体移除：不再请求 `get-client-visible-credit-grants`，面板与看板不再展示，`cache/cursor-credits.json` 不再写入，可手动删除。
- 套餐快照精简为百分比、总消耗、账期与 Bot 周期字段；`subscription_status`、`plan_limit`、`included_cents`、`bonus_cents`、`auto_used`、`auto_limit`、`credit_*` 从 ingest 载荷与看板 API 移除。

**从 v0.4.0 升级**：已有配置、接入 token 和历史数据继续有效，ingest schema 仍为 1；看板库启动时自动补列（`total_used_cents`、`archived_at`），旧列保留不读。先升级 dash，再升级 agent：新 agent 对旧 dash 只是少显示「已用量」，旧 agent 对新 dash 会被忽略已移除字段，两侧都不报错。

---

Linux x86_64 静态二进制（musl），不依赖宿主机 glibc / Node / Python。

本版本只提供 Linux x86_64。macOS / Windows / aarch64 尚未发布。

校验：

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

用法见 [README](https://github.com/Huanfiy/ai-usage#快速体验)。
