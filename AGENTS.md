# AGENTS.md

本文件为 AI 助手提供本仓库的开发协作指南。

## 项目概览

AI Usage 是用量可见性系统：采集端只读解析本机 AI 工具日志并增量上报；看板端接收多主机数据、落库、估算费用并内嵌 Web UI。两端为独立静态二进制，耦合是 ingest 协议与 agent 主动拉取的 join 接入。

| 项 | 内容 |
| --- | --- |
| 采集端 | `ai-usage-agent`（daemon 本机面板默认 `127.0.0.1:3848`） |
| 看板端 | `ai-usage-dash` |
| 语言 | Rust workspace；看板 UI 为 Vue 3 + Vite |
| 自身文件 | XDG；`--config` / `--data-dir` 可改 |
| 解析器 | Claude Code、Codex、Grok、Pi、Cursor |

架构：[docs/design/architecture.md](docs/design/architecture.md)。解析边界：[docs/design/parser-boundaries.md](docs/design/parser-boundaries.md)。冲突以代码与 `docs/design/` 为准。对外说明以 [README.md](README.md) 为准；发布产物为 Linux x86_64 musl，由 tag 工作流产出。立项需求原文已移出工作区，git 历史 `20c1b0e` 可查。

## 目录结构

```text
crates/protocol/   ingest 与 join 契约
crates/parsers/    解析器；夹具 fixtures/home/
crates/agent/      ai-usage-agent
crates/dash/       ai-usage-dash（嵌入 UI、LiteLLM 快照与 Cursor 自有价）
web/               Vue SPA
fixtures/home/     解析器测试用伪造 $HOME
fixtures/cursor/   Cursor CSV 夹具
deploy/            systemd 与 Dockerfile.dash
docs/design/       系统架构与解析边界
scripts/           价目快照脚本
run.sh             本机开发入口（build / run / clean …）
tmp/               本地产物，不入库
```

## 构建与运行

本机入口 `./run.sh`（说明见 [README.md](README.md)）。

| 命令 | 用途 |
| --- | --- |
| `./run.sh build` | 构建 Web UI 与两端二进制（host gnu，默认 debug） |
| `./run.sh build musl` | 静态 musl 发布构建，产物在 `target/x86_64-unknown-linux-musl/release/` |
| `./run.sh run` | 启动看板 |
| `./run.sh dev` | 看板 API + Vite 热更新 |
| `./run.sh test` | `cargo test --workspace` |
| `./run.sh clean` | 清理构建产物 |
| `./run.sh agent …` | 采集端 CLI |
| `./run.sh agent reload` | 编采集端、装入 user service 并重启 |
| `./run.sh panel` | 打开采集端本机面板 |
| `./run.sh dash …` | 看板 CLI |

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
