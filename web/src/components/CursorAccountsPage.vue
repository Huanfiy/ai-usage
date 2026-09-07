<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { api, type BreakdownItem, type CursorAccountRow, type Query } from '../api'
import { fmtTime, fmtTokens, fmtUsd } from '../format'

const items = ref<CursorAccountRow[]>([])
const staleDays = ref(7)
const err = ref('')
const loading = ref(false)
const loaded = ref(false)

type AcctUsage = {
  loading: boolean
  error: string
  models: BreakdownItem[]
  totalTokens: number
  totalCost: number
}
const usage = ref<Record<string, AcctUsage>>({})
const hoverHash = ref('')

async function load() {
  loading.value = true
  try {
    const r = await api.cursorAccounts()
    items.value = r.items ?? []
    staleDays.value = r.stale_days ?? 7
    err.value = ''
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
    loaded.value = true
  }
}

// 活跃 = 未手动归档且快照未过期；其余折叠到「已归档」区
const active = computed(() => items.value.filter((a) => !a.archived_at && !a.stale))
const archived = computed(() => items.value.filter((a) => a.archived_at || a.stale))

const archOpen = ref(false)
const acting = ref('')

async function archive(a: CursorAccountRow) {
  acting.value = a.account_hash
  try {
    await api.archiveCursorAccount(a.account_hash)
    await load()
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  } finally {
    acting.value = ''
  }
}

async function restore(a: CursorAccountRow) {
  acting.value = a.account_hash
  try {
    await api.restoreCursorAccount(a.account_hash)
    await load()
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  } finally {
    acting.value = ''
  }
}

function archiveReason(a: CursorAccountRow): string {
  if (a.archived_at) return `手动归档 ${fmtTime(a.archived_at)}`
  const days = Math.max(1, Math.floor((Date.now() - new Date(a.fetched_at).getTime()) / 86_400_000))
  return `已 ${days} 天未上报`
}

// 已归档账号的历史总结：该账号有记录以来全部用量的费用估算与 token 数。
// 只在展开「已归档」时按需拉，结果按 hash 缓存
type Lifetime = { loading: boolean; error: string; totalTokens: number; totalCost: number }
const lifetime = ref<Record<string, Lifetime>>({})
const LIFETIME_FROM = '2020-01-01T00:00:00.000Z'

async function loadLifetime(a: CursorAccountRow) {
  const key = a.account_hash
  const cached = lifetime.value[key]
  if (cached && !cached.error && !cached.loading) return
  lifetime.value[key] = { loading: true, error: '', totalTokens: 0, totalCost: 0 }
  const q: Query = { from: LIFETIME_FROM, to: new Date().toISOString(), host: `acct:${key}` }
  try {
    const r = await api.breakdown(q, 'source')
    const all = r.items ?? []
    lifetime.value[key] = {
      loading: false,
      error: '',
      totalTokens: all.reduce((s, x) => s + (x.tokens || 0), 0),
      totalCost: all.reduce((s, x) => s + (x.cost_usd || 0), 0),
    }
  } catch (e) {
    lifetime.value[key] = {
      loading: false,
      error: e instanceof Error ? e.message : String(e),
      totalTokens: 0,
      totalCost: 0,
    }
  }
}

watch([archOpen, archived], ([open, list]) => {
  if (!open) return
  for (const a of list) void loadLifetime(a)
})

function lifetimeText(a: CursorAccountRow): string {
  const l = lifetime.value[a.account_hash]
  if (!l || l.loading) return '累计 …'
  if (l.error) return '累计 —'
  if (!l.totalTokens) return '累计 无用量记录'
  return `累计 ${fmtUsd(l.totalCost)} · ${fmtTokens(l.totalTokens)}`
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
function pctText(n: number | null | undefined): string {
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

// 到重置时刻的倒计时：≥1 天 d/h，<1 天 h/m；已过则「待重置」
function countdown(iso: string | null | undefined): { text: string; soon: boolean } | null {
  if (!iso) return null
  const ms = new Date(iso).getTime() - Date.now()
  if (Number.isNaN(ms)) return null
  if (ms <= 0) return { text: '待重置', soon: true }
  const h = Math.floor(ms / 3_600_000)
  const d = Math.floor(h / 24)
  const text =
    d >= 1
      ? `${d}d ${h % 24}h`
      : h >= 1
        ? `${h}h ${Math.floor((ms % 3_600_000) / 60_000)}m`
        : `${Math.max(1, Math.floor(ms / 60_000))}m`
  return { text, soon: ms < 86_400_000 }
}

const USAGE_DAYS = 30

// 悬浮时按需拉取该账号（acct:<hash>）近 30 天的模型分布与费用估算
async function onEnter(a: CursorAccountRow) {
  hoverHash.value = a.account_hash
  const key = a.account_hash
  const cached = usage.value[key]
  if (cached && !cached.error && !cached.loading) return
  usage.value[key] = { loading: true, error: '', models: [], totalTokens: 0, totalCost: 0 }
  const to = new Date()
  const from = new Date(to.getTime() - USAGE_DAYS * 86_400_000)
  const q: Query = {
    from: from.toISOString(),
    to: to.toISOString(),
    host: `acct:${key}`,
  }
  try {
    const r = await api.breakdown(q, 'model')
    const all = r.items ?? []
    usage.value[key] = {
      loading: false,
      error: '',
      models: all.slice(0, 10),
      totalTokens: all.reduce((s, x) => s + (x.tokens || 0), 0),
      totalCost: all.reduce((s, x) => s + (x.cost_usd || 0), 0),
    }
  } catch (e) {
    usage.value[key] = {
      loading: false,
      error: e instanceof Error ? e.message : String(e),
      models: [],
      totalTokens: 0,
      totalCost: 0,
    }
  }
}

function onLeave() {
  hoverHash.value = ''
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
        RPC），展示的是当前状态，不随看板时间范围筛选变化。超过
        {{ staleDays }} 天未上报的账号自动折叠到「已归档」。
      </p>

      <div v-if="loaded && !items.length" class="empty">
        尚无 Cursor 账号快照。到采集端本机面板（默认 http://127.0.0.1:3848）加入 Cursor
        账号后，将随下一轮 Cursor 同步出现在这里。
      </div>

      <div v-else-if="loaded && !active.length" class="empty">全部账号已归档。</div>

      <div v-else class="acct-grid">
        <article
          v-for="a in active"
          :key="a.account_hash"
          class="acct"
          @mouseenter="onEnter(a)"
          @mouseleave="onLeave"
        >
          <div class="acct-head">
            <div class="acct-email" :title="a.account_label">{{ a.account_label }}</div>
            <div class="acct-chips">
              <span v-if="a.membership" class="tag on">{{ a.membership }}</span>
            </div>
          </div>

          <div class="meters">
            <div class="meter">
              <div class="meter-row">
                <span>API</span>
                <span
                  class="reset"
                  :class="{ soon: countdown(a.billing_cycle_end)?.soon }"
                  :title="a.billing_cycle_end ? `账期重置于 ${fmtTime(a.billing_cycle_end)}` : undefined"
                >{{ countdown(a.billing_cycle_end) ? `重置 ${countdown(a.billing_cycle_end)!.text}` : '' }}</span>
                <b class="val">{{ pctText(a.api_percent) }}</b>
              </div>
              <div class="bar" :class="barClass(a.api_percent)"><i :style="{ width: barWidth(a.api_percent) }" /></div>
            </div>
            <div class="meter">
              <div class="meter-row">
                <span>Auto</span>
                <span
                  class="reset"
                  :class="{ soon: countdown(a.billing_cycle_end)?.soon }"
                  :title="a.billing_cycle_end ? `账期重置于 ${fmtTime(a.billing_cycle_end)}` : undefined"
                >{{ countdown(a.billing_cycle_end) ? `重置 ${countdown(a.billing_cycle_end)!.text}` : '' }}</span>
                <b class="val">{{ pctText(a.auto_percent) }}</b>
              </div>
              <div class="bar" :class="barClass(a.auto_percent)"><i :style="{ width: barWidth(a.auto_percent) }" /></div>
            </div>
            <div v-if="a.bot_percent != null" class="meter">
              <div class="meter-row">
                <span>Bot</span>
                <span
                  class="reset"
                  :class="{ soon: countdown(a.bot_next_reset)?.soon }"
                  :title="a.bot_next_reset ? `Bot 周期重置于 ${fmtTime(a.bot_next_reset)}` : undefined"
                >{{ countdown(a.bot_next_reset) ? `重置 ${countdown(a.bot_next_reset)!.text}` : '' }}</span>
                <b class="val">{{ pctText(a.bot_percent) }}</b>
              </div>
              <div class="bar" :class="barClass(a.bot_percent)"><i :style="{ width: barWidth(a.bot_percent) }" /></div>
            </div>
            <div v-else class="bot-note" title="Bot 用量走 Cursor 原生 RPC，只认 IDE 原生 access token；web 凭证或未拉到时无数据">
              Bot 无数据（web 凭证或暂未拉到）
            </div>
          </div>

          <div
            v-if="usd(a.total_used_cents)"
            class="meta"
            title="本账期总消耗：包含额度内已用（plan.used）+ 超出后累计的附赠消耗（breakdown.bonus），随账期重置"
          >已用量 {{ usd(a.total_used_cents) }}</div>
          <div class="foot">
            <span>快照 {{ fmtTime(a.fetched_at) }}</span>
            <button
              type="button"
              class="archive-btn"
              :disabled="acting === a.account_hash"
              title="归档该账号卡片；出现新用量时自动恢复"
              @click.stop="archive(a)"
            >归档</button>
          </div>

          <div v-if="hoverHash === a.account_hash" class="usage-pop">
            <div class="pop-title">近 {{ USAGE_DAYS }} 天模型用量 · 费用为估算</div>
            <div v-if="usage[a.account_hash]?.loading" class="pop-hint">加载中…</div>
            <div v-else-if="usage[a.account_hash]?.error" class="pop-hint pop-err">
              {{ usage[a.account_hash].error }}
            </div>
            <template v-else>
              <div v-if="!usage[a.account_hash]?.models.length" class="pop-hint">窗口内无用量</div>
              <template v-else>
                <div v-for="m in usage[a.account_hash].models" :key="m.key" class="pop-row">
                  <span class="pop-model" :title="m.key">{{ m.key }}</span>
                  <span class="pop-tokens">{{ fmtTokens(m.tokens) }}</span>
                  <b class="pop-cost">{{ fmtUsd(m.cost_usd) }}</b>
                </div>
                <div class="pop-row pop-total">
                  <span class="pop-model">合计</span>
                  <span class="pop-tokens">{{ fmtTokens(usage[a.account_hash].totalTokens) }}</span>
                  <b class="pop-cost">{{ fmtUsd(usage[a.account_hash].totalCost) }}</b>
                </div>
              </template>
            </template>
          </div>
        </article>
      </div>

      <div v-if="archived.length" class="archived-sec">
        <button type="button" class="arch-toggle" @click="archOpen = !archOpen">
          {{ archOpen ? '▾' : '▸' }} 已归档 · {{ archived.length }}
        </button>
        <div v-if="archOpen" class="arch-grid">
          <article v-for="a in archived" :key="a.account_hash" class="arch-card">
            <div class="arch-main">
              <span class="arch-email" :title="a.account_label">{{ a.account_label }}</span>
              <span v-if="a.membership" class="tag">{{ a.membership }}</span>
            </div>
            <div class="arch-meta">
              <span>{{ archiveReason(a) }}</span>
              <span>快照 {{ fmtTime(a.fetched_at) }}</span>
            </div>
            <div
              class="arch-sum"
              :class="{ err: lifetime[a.account_hash]?.error }"
              :title="lifetime[a.account_hash]?.error || '该账号有记录以来全部用量的费用估算与 token 数，非账单'"
            >{{ lifetimeText(a) }}</div>
            <button
              v-if="a.archived_at"
              type="button"
              class="arch-restore"
              :disabled="acting === a.account_hash"
              @click="restore(a)"
            >恢复</button>
          </article>
        </div>
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
  position: relative;
}
.acct:hover {
  border-color: #3d4b5e;
}
.usage-pop {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  right: 0;
  z-index: 10;
  background: var(--bg-elev);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 10px 12px;
  box-shadow: var(--shadow);
  font-size: 12px;
}
.pop-title {
  color: var(--muted);
  font-size: 11px;
  letter-spacing: 0.04em;
  margin-bottom: 6px;
}
.pop-hint {
  color: var(--muted);
  font-size: 12px;
}
.pop-err {
  color: var(--rose);
}
.pop-row {
  display: flex;
  align-items: baseline;
  gap: 10px;
  padding: 2px 0;
}
.pop-model {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
}
.pop-tokens {
  color: var(--muted);
  font-variant-numeric: tabular-nums;
  flex-shrink: 0;
}
.pop-cost {
  color: var(--amber);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  min-width: 56px;
  text-align: right;
  flex-shrink: 0;
}
.pop-total {
  border-top: 1px solid var(--line);
  margin-top: 4px;
  padding-top: 6px;
}
.pop-total .pop-model {
  color: var(--muted);
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
  display: grid;
  grid-template-columns: 36px 1fr auto;
  align-items: baseline;
  gap: 8px;
  font-size: 11px;
  color: var(--muted);
  margin-bottom: 4px;
}
.meter-row .reset {
  text-align: left;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}
.meter-row .reset.soon {
  color: var(--amber);
}
.meter-row .val {
  color: var(--text);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  text-align: right;
  white-space: nowrap;
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
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.archive-btn {
  border: 1px solid var(--line);
  background: transparent;
  color: var(--muted);
  padding: 1px 8px;
  border-radius: 999px;
  font-size: 11px;
  cursor: pointer;
  opacity: 0;
  transition: opacity 0.12s;
}
.acct:hover .archive-btn {
  opacity: 1;
}
.archive-btn:hover:not(:disabled) {
  border-color: #3d4b5e;
  color: var(--text);
}
.archive-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.archived-sec {
  margin-top: 14px;
  border-top: 1px solid var(--line);
  padding-top: 10px;
}
.arch-toggle {
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  cursor: pointer;
  padding: 0;
}
.arch-toggle:hover {
  color: var(--text);
}
.arch-grid {
  margin-top: 10px;
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
  gap: 8px;
}
.arch-card {
  background: var(--bg-elev-2);
  border: 1px dashed var(--line);
  border-radius: 10px;
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  opacity: 0.85;
}
.arch-main {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 8px;
}
.arch-email {
  font-size: 13px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
  flex: 1;
}
.arch-meta {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  font-size: 11px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
.arch-sum {
  font-size: 12px;
  color: var(--text);
  font-variant-numeric: tabular-nums;
}
.arch-sum.err {
  color: var(--muted);
}
.arch-restore {
  align-self: flex-start;
  border: 1px solid var(--line);
  background: transparent;
  color: var(--text);
  padding: 2px 10px;
  border-radius: 999px;
  font-size: 11px;
  cursor: pointer;
}
.arch-restore:hover:not(:disabled) {
  border-color: #3d4b5e;
  background: #1c2430;
}
.arch-restore:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
