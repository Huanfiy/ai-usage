<!-- 每次发布就地替换「本次变更」一节，历史版本由 git log 与既往 Release 承载，本文件不累积。 -->
## 本次变更（v0.3.0）

**破坏性变更：接入方式改为「申请 — 人工批准 — 领取」。**

- `ai-usage-agent init` 去掉 `--token`。填 `--url` 后打印确认码，在看板设置页对照批准，agent 轮询领取 ingest token。申请 10 分钟过期，带限流。采集端本机面板新增看板地址走同一条流程。
- `ai-usage-dash token create` 移除。`token list` / `revoke` / `delete` 保留。
- 空库首次 `ai-usage-dash serve` 不再自动签发并打印本机 ingest token。本机接入同样要人批。

看板仍然不主动连接宿主机、不向采集端下发配置：token 由 agent 主动拉取。

**从 v0.2.0 升级**：配置里已有的 token 继续有效，不必重新接入。`agent init` 遇到已接入的地址会跳过申请直接同步。

**版本号修正**：v0.1.0 与 v0.2.0 的二进制内部版本都印成 `0.1.0`。本版起 `--version` 和 `/v1/health` 与 tag 一致，发布工作流会校验，不一致直接失败。

---

Linux x86_64 静态二进制（musl），不依赖宿主机 glibc / Node / Python。

本版本只提供 Linux x86_64。macOS / Windows / aarch64 尚未发布。

校验：

```bash
sha256sum -c SHA256SUMS --ignore-missing
```

用法见 [README](https://github.com/Huanfiy/ai-usage#快速体验)。
