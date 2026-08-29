<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { api, type CursorAccountRow } from '../api'
import { fmtTime } from '../format'

const items = ref<CursorAccountRow[]>([])
const err = ref('')
const loading = ref(false)
const loaded = ref(false)

async function load() {
  loading.value = true
  try {
    const r = await api.cursorAccounts()
    items.value = r.items ?? []
    err.value = ''
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
    loaded.value = true
  }
}

function onVisibility() {
  if (!document.hidden) void load()
}

onMounted(() => {
  void load()
  document.addEventListener('visibilitychange', onVisibility)
})
onUnmounted(() => {
  document.removeEventListener('visibilitychange', onVisibility)
})

// 套餐百分比是服务端原始数值（0–100 语义），不做换算
function pct(n: number | null | undefined): string {
  if (n == null || Number.isNaN(n)) return '—'
  return `${Math.round(n * 10) / 10}%`
}

function barWidth(n: number | null | undefined): string {
  if (n == null || Number.isNaN(n)) return '0%'
  return `${Math.max(0, Math.min(100, n))}%`
}

function barClass(n: number | null | undefined): string {
  if (n == null) return ''
  if (n >= 90) return 'hot'
  if (n >= 70) return 'warn'
  return ''
}

function usd(cents: number | null | undefined): string | null {
  if (cents == null || Number.isNaN(Number(cents))) return null
  return `$${(Number(cents) / 100).toFixed(2)}`
}

function metaBits(a: CursorAccountRow): string {
  const bits: string[] = []
  const used = usd(a.plan_used)
  if (used) bits.push(`已用 ${used}`)
  const bonus = usd(a.bonus_cents)
  if (a.bonus_cents) bits.push(`赠额 ${bonus}`)
  if (a.billing_cycle_end) bits.push(`账期至 ${String(a.billing_cycle_end).slice(0, 10)}`)
  if (a.bot_next_reset) bits.push(`Bot 重置 ${String(a.bot_next_reset).slice(0, 10)}`)
  return bits.join(' · ')
}
</script>

<template>
  <div class="cursor-page">
    <p v-if="err" class="err">{{ err }}</p>

    <section class="card">
      <div class="card-head">
        <h2>Cursor 账号套餐</h2>
        <button type="button" class="reload" :disabled="loading" @click="load">
          {{ loading ? '刷新中…' : '刷新' }}
        </button>
      </div>
      <p class="lead">
        快照由各采集端在 Cursor 同步周期拉取并上报（API / Auto 来自 usage-summary，Bot 来自原生
        RPC），展示的是当前状态，不随看板时间范围筛选变化。
      </p>

      <div v-if="loaded && !items.length" class="empty">
        尚无 Cursor 账号快照。到采集端本机面板（默认 http://127.0.0.1:3848）加入 Cursor
        账号后，将随下一轮 Cursor 同步出现在这里。
      </div>

      <div v-else class="acct-grid">
        <article v-for="a in items" :key="a.account_hash" class="acct">
          <div class="acct-head">
            <div class="acct-email" :title="a.account_label">{{ a.account_label }}</div>
            <div class="acct-chips">
              <span v-if="a.membership" class="tag on">{{ a.membership }}</span>
              <span
                v-if="a.subscription_status"
                class="tag"
                :class="{ on: a.subscription_status === 'active' }"
              >{{ a.subscription_status }}</span>
            </div>
          </div>

          <div class="meters">
            <div class="meter">
              <div class="meter-row"><span>API</span><b>{{ pct(a.api_percent) }}</b></div>
              <div class="bar" :class="barClass(a.api_percent)"><i :style="{ width: barWidth(a.api_percent) }" /></div>
            </div>
            <div class="meter">
              <div class="meter-row"><span>Auto</span><b>{{ pct(a.auto_percent) }}</b></div>
              <div class="bar" :class="barClass(a.auto_percent)"><i :style="{ width: barWidth(a.auto_percent) }" /></div>
            </div>
            <div v-if="a.bot_percent != null" class="meter">
              <div class="meter-row"><span>Bot</span><b>{{ pct(a.bot_percent) }}</b></div>
              <div class="bar" :class="barClass(a.bot_percent)"><i :style="{ width: barWidth(a.bot_percent) }" /></div>
            </div>
            <div v-else class="bot-note" title="Bot 用量走 Cursor 原生 RPC，只认 IDE 原生 access token；web 凭证或未拉到时无数据">
              Bot 无数据（web 凭证或暂未拉到）
            </div>
          </div>

          <div v-if="metaBits(a)" class="meta">{{ metaBits(a) }}</div>
          <div class="foot">快照 {{ fmtTime(a.fetched_at) }}</div>
        </article>
      </div>
    </section>
  </div>
</template>

<style scoped>
.lead {
  margin: 0 0 12px;
  color: var(--muted);
  font-size: 12px;
}
.reload {
  border: 1px solid var(--line);
  background: transparent;
  color: var(--text);
  padding: 5px 12px;
  border-radius: 999px;
  font-size: 12px;
  cursor: pointer;
}
.reload:hover:not(:disabled) {
  border-color: #3d4b5e;
  background: #1c2430;
}
.reload:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.acct-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 10px;
}
.acct {
  background: var(--bg-elev-2);
  border: 1px solid var(--line);
  border-radius: 12px;
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.acct-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 8px;
}
.acct-email {
  font-size: 14px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
  flex: 1;
}
.acct-chips {
  display: flex;
  gap: 6px;
  flex-shrink: 0;
}
.tag {
  border: 1px solid var(--line);
  background: var(--bg-elev);
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 11px;
  color: var(--muted);
}
.tag.on {
  border-color: var(--mint);
  color: var(--mint);
  background: var(--mint-dim);
}
.meters {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.meter-row {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  gap: 8px;
  font-size: 11px;
  color: var(--muted);
  margin-bottom: 4px;
}
.meter-row b {
  color: var(--text);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.bar {
  height: 6px;
  background: #1c2430;
  border-radius: 99px;
  overflow: hidden;
}
.bar > i {
  display: block;
  height: 100%;
  width: 0;
  background: var(--mint);
}
.bar.warn > i {
  background: var(--amber);
}
.bar.hot > i {
  background: var(--rose);
}
.bot-note {
  color: var(--muted);
  font-size: 11px;
}
.meta {
  color: var(--muted);
  font-size: 12px;
}
.foot {
  margin-top: auto;
  color: var(--muted);
  font-size: 11px;
  font-variant-numeric: tabular-nums;
}
</style>
