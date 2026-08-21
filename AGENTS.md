# AGENTS.md

本文件为 AI 助手提供本仓库的开发协作指南。

## 项目概览

AI Usage 是用量可见性系统：采集端只读解析本机 AI 工具日志并增量上报；看板端接收多主机数据、落库、估算费用并内嵌 Web UI。两端为独立静态二进制，唯一耦合是 ingest 协议。

| 项 | 内容 |
| --- | --- |
| 采集端 | `ai-usage-agent` |
| 看板端 | `ai-usage-dash` |
| 语言 | Rust workspace；看板 UI 为 Vue 3 + Vite |
| 自身文件 | XDG；`--config` / `--data-dir` 可改 |
| 解析器 | Claude Code、Codex、Grok、Cursor |

架构：[docs/design/architecture.md](docs/design/architecture.md)。解析边界：[docs/design/parser-boundaries.md](docs/design/parser-boundaries.md)。需求原文：`goal.md`。冲突以代码与 `docs/design/` 为准。

## 目录结构

```text
crates/protocol/   ingest 契约
crates/parsers/    解析器；夹具 fixtures/home/
crates/agent/      ai-usage-agent
crates/dash/       ai-usage-dash（嵌入 UI、LiteLLM 快照与 Cursor 自有价）
web/               Vue SPA
fixtures/home/     解析器测试用伪造 $HOME
fixtures/cursor/   Cursor CSV 夹具
deploy/            systemd 与 Dockerfile.dash
docs/design/       系统架构与解析边界
scripts/           价目快照脚本
tmp/               本地产物，不入库
```

## 构建与运行

本机步骤见 [README.md](README.md)。

| 命令 | 用途 |
| --- | --- |
| `cargo test --workspace` | 测试 |
| `cd web && npm run dev` | UI 热更新（需同时跑 dash） |
| `ai-usage-agent {init,sync,status,daemon}` | 采集端 |
| `ai-usage-dash {serve,token,pricing}` | 看板端 |

## 索引

| 主题 | 位置 |
| --- | --- |
| 架构、约束、范围 | [docs/design/architecture.md](docs/design/architecture.md) |
| 解析计入边界 | [docs/design/parser-boundaries.md](docs/design/parser-boundaries.md) |
| 使用与部署 | [README.md](README.md) |
| ingest 契约 | `crates/protocol` |
| 文档治理 | [docs-rules.md](docs-rules.md) |

根目录 `.cursor/` 等 AI 工具目录不入库；`fixtures/home/` 须跟踪。

## Commit 规范

- 格式：`<emoji> <type>(<scope>): <subject>`，中文主题，单行。
- type：`✨ feat` / `🐞 fix` / `⚡️ perf` / `🎨 refactor` / `🔧 chore` / `📝 docs`。
- 一笔一事，可回滚。
- 不添加 `Co-Authored-By` 及任何 AI 署名尾注。
