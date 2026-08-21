<script setup lang="ts">
import type { Summary } from '../api'
import { fmtDur, fmtPct, fmtTokens, fmtUsd } from '../format'

defineProps<{ summary: Summary; loading: boolean }>()
</script>

<template>
  <section class="kpis">
    <div class="kpis-row">
      <div class="kpi">
        <div class="label">估算费用</div>
        <div class="value amber">{{ fmtUsd(summary.cost_usd) }}</div>
        <div class="hint">覆盖率 {{ fmtPct(summary.cost_coverage) }}</div>
      </div>
      <div class="kpi">
        <div class="label">Token</div>
        <div class="value mint">{{ fmtTokens(summary.tokens.total) }}</div>
        <div class="hint">
          入 {{ fmtTokens(summary.tokens.input) }} · 出 {{ fmtTokens(summary.tokens.output) }}
        </div>
      </div>
      <div class="kpi">
        <div class="label">缓存命中</div>
        <div class="value">{{ fmtPct(summary.cache_hit_rate) }}</div>
        <div class="hint">
          读 {{ fmtTokens(summary.tokens.cache_read) }} · 创建 {{ fmtTokens(summary.tokens.cache_creation) }}
        </div>
      </div>
      <div class="kpi">
        <div class="label">会话</div>
        <div class="value">{{ summary.sessions }}</div>
        <div class="hint">{{ loading ? '刷新中…' : '按 last_message 计入窗口' }}</div>
      </div>
    </div>
    <div class="kpis-row">
      <div class="kpi">
        <div class="label">总消息</div>
        <div class="value">{{ fmtTokens(summary.message_count ?? 0) }}</div>
        <div class="hint">窗口内会话合计</div>
      </div>
      <div class="kpi">
        <div class="label">用户消息</div>
        <div class="value">{{ fmtTokens(summary.user_message_count ?? 0) }}</div>
        <div class="hint">user turns</div>
      </div>
      <div class="kpi">
        <div class="label">总时长</div>
        <div class="value">{{ fmtDur(summary.duration_seconds ?? 0) }}</div>
        <div class="hint">first → last</div>
      </div>
      <div class="kpi">
        <div class="label">活跃时长</div>
        <div class="value">{{ fmtDur(summary.active_seconds ?? 0) }}</div>
        <div class="hint">生成时间</div>
      </div>
    </div>
  </section>
</template>
