# AI Usage

两套独立静态二进制：采集端 `ai-usage-agent` 只读解析本机 AI 工具日志并增量上报；看板端 `ai-usage-dash` 接收多主机数据、落 SQLite、估算费用并内嵌 Web UI。

本机（`./run.sh help` 查看全部命令）：

```bash
./run.sh run
# 按提示复制 ingest token
./run.sh agent init --url http://127.0.0.1:3847 --token <token>
```

浏览器打开 http://127.0.0.1:3847 。采集端已装 user service 时用 `./run.sh agent reload` 更新二进制并重启，再 `./run.sh panel`（默认 http://127.0.0.1:3848 ）改上报间隔（按钟面对齐，如 5m → `:00/:05`）、增删看板地址（新地址全量同步，同步按钮按地址增量）、以卡片管理 Cursor 账号（粘贴 JWT 或上传导出 JSON）；不要再开一条前台 daemon。改看板 UI 用 `./run.sh dev`（Vite :5173，API 仍走看板）。自身文件只走 XDG（`~/.config/ai-usage`、`~/.local/share/ai-usage`），可用 `--config` / `--data-dir` 改到任意目录。不写 `~/.claude`、`~/.codex` 等工具目录。

多机：看板部署到一台机器，各宿主机 agent 向外推送。跨 glibc 版本用 `./run.sh build musl`，产物在 `target/x86_64-unknown-linux-musl/release/`。Agent 不容器化。看板可选 `deploy/Dockerfile.dash`。

首批解析器：Claude Code、Codex、Grok、Cursor。Cursor 用量来自账号 CSV（本机只读 JWT），按账号幂等、不按机器累加。费用来自构建期嵌入的 LiteLLM 价目快照（MIT），可用 `ai-usage-dash pricing update` 刷新数据目录缓存；LiteLLM 没有的 Cursor 自有模型用嵌入的官方列表价补缺（不随刷新更新）。查询时按归一化模型名计价；未知模型计入 token、排除出费用并给出 coverage%。

## License

[Apache License 2.0](LICENSE)。允许使用、修改与再分发；二次开发须保留版权声明与 `NOTICE`，并写明源自本项目。
