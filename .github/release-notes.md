<!-- 每次发布就地替换「本次变更」一节，历史版本由 git log 与既往 Release 承载，本文件不累积。 -->
## 本次变更（v0.4.0）

**新增 Pi agent 用量统计。**

- 只读解析 `~/.pi/agent/sessions/` 下的 JSONL 会话文件，支持 `PI_CODING_AGENT_DIR` 与 `PI_CODING_AGENT_SESSION_DIR` 覆盖路径。
- 统计 Pi 的 input、output、cache read、cache write、reasoning token，并按模型、项目和 UTC 半小时聚合。
- 支持会话明细、增量解析、缓存、分支历史和压缩摘要用量。
- `cursor-agent` provider 不计 token，Pi fork / clone 会话保留明细但不计 token，避免重复统计。
- 更新采集端面板图标、工具筛选和解析边界文档。

**看板价目表热更新**：设置页新增「更新价目表」按钮，刷新后立即生效，无需重启 dash。

**从 v0.3.0 升级**：已有配置、接入 token 和历史数据继续有效，ingest schema 仍为 1。先升级 dash，再升级 agent，首次同步会导入已有 Pi 日志。若先升级了 agent，升级 dash 后执行 `ai-usage-agent sync --full` 补报会话；多目标配置可用 `--url <看板地址>` 限定补报目标。

---

Linux x86_64 静态二进制（musl），不依赖宿主机 glibc / Node / Python。

本版本只提供 Linux x86_64。macOS / Windows / aarch64 尚未发布。

校验：

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

用法见 [README](https://github.com/Huanfiy/ai-usage#快速体验)。
