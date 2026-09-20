<!-- 每次发布就地替换「本次变更」一节，历史版本由 git log 与既往 Release 承载，本文件不累积。 -->
## 本次变更（v0.6.0）

**看板：Cursor 账号卡片按账期统计。**

- 卡片悬浮的模型用量列表改为「当前账期起点 → 现在」，标题标出起点时刻；此前是滚动 30 天。
- 采集端从 usage-summary 提取 `billingCycleStart` 随快照上报（ingest 新增可选字段 `billing_cycle_start`），看板库启动自动补列。旧采集端没报起点时，前端按月账期从 `billing_cycle_end` 反推。

**看板：长窗口边界精确。**

- 超过 2 天的窗口，中间整 UTC 日走日汇总，首尾不足一天的部分改查小时桶，不再把窗口外同一 UTC 日的用量算进来。影响概览、趋势、分布与悬浮列表；7D / 30D 预设从本地零点起算，此前也会多算几小时。

**价目表：发版快照为主，支持导入。**

- `./run.sh build musl` 发版前自动刷新内置 LiteLLM 快照（`./run.sh pricing` 可单独刷；CI 从已提交的快照构建）。本版快照 2581 → 3702 个模型，含 `claude-fable-5-1` 等新条目。
- 新增 `POST /v1/pricing/import`、设置页「导入文件」与 `ai-usage-dash pricing import <file>`：接受 LiteLLM 原始 JSON 或精简快照，供看板主机不能访问外网时使用。
- 数据目录 `pricing.json` 早于内置快照时忽略，某次手动刷新留下的老缓存不再盖住发版带来的新快照；设置页状态改为反映实际生效的那份。

**从 v0.5.0 升级**：配置、接入 token、历史数据继续有效，ingest schema 仍为 1；看板库启动自动补列 `billing_cycle_start`。先升级 dash 再升级 agent：旧 agent 对新 dash 只是少报账期起点（前端反推），新 agent 对旧 dash 的新字段会被忽略，两侧都不报错。刷新过价目表的看板主机，若 `pricing.json` 早于本版快照会自动改用内置快照；费用按查询时价目折算，历史数据一并重算（如 claude-fable-5-1 的 cache_read 单价从前缀回退的 $1/M 变为 $0.25/M）。

---

Linux x86_64 静态二进制（musl），不依赖宿主机 glibc / Node / Python。

本版本只提供 Linux x86_64。macOS / Windows / aarch64 尚未发布。

校验：

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

用法见 [README](https://github.com/Huanfiy/ai-usage#快速体验)。
