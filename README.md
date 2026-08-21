# AI Usage

两套独立静态二进制：采集端 `ai-usage-agent` 只读解析本机 AI 工具日志并增量上报；看板端 `ai-usage-dash` 接收多主机数据、落 SQLite、估算费用并内嵌 Web UI。

本机：

```bash
cd web && npm install && npm run build
cargo build --release -p ai-usage-dash -p ai-usage-agent
./target/release/ai-usage-dash serve --bind 127.0.0.1:3847
# 按提示复制 ingest token
./target/release/ai-usage-agent init --url http://127.0.0.1:3847 --token <token>
```

浏览器打开 http://127.0.0.1:3847 。自身文件只走 XDG（`~/.config/ai-usage`、`~/.local/share/ai-usage`），可用 `--config` / `--data-dir` 改到任意目录。不写 `~/.claude`、`~/.codex` 等工具目录。

多机：看板部署到一台机器，各宿主机 agent 向外推送。Agent 不容器化。看板可选 `deploy/Dockerfile.dash`。

首批解析器：Claude Code、Codex、Grok、Cursor。Cursor 用量来自账号 CSV（本机只读 JWT），按账号幂等、不按机器累加。费用来自构建期嵌入的 LiteLLM 价目快照（MIT），可用 `ai-usage-dash pricing update` 刷新数据目录缓存；LiteLLM 没有的 Cursor 自有模型用嵌入的官方列表价补缺（不随刷新更新）。查询时按归一化模型名计价；未知模型计入 token、排除出费用并给出 coverage%。
