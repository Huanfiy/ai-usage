<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { api, type BreakdownItem, type CursorAccountRow, type Query } from '../api'
import { fmtTime, fmtTokens, fmtUsd } from '../format'

const items = ref<CursorAccountRow[]>([])
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

// 右侧数值：「用量/额度 · 百分比」；没有金额时只显示百分比
function meterValue(pct: number | null | undefined, used?: number | null, limit?: number | null): string {
  const parts: string[] = []
  const u = usd(used)
  const l = usd(limit)
  if (u && l) parts.push(`${u}/${l}`)
  else if (u) parts.push(u)
  parts.push(pctText(pct))
  return parts.join(' · ')
}

// 信用余额（cursor.com 账单页 Credits）：与附赠池不同，有到期日、跨账期扣减
function hasCredit(a: CursorAccountRow): boolean {
  return a.credit_total_cents != null && a.credit_total_cents > 0
}

const creditOpen = ref('')
function toggleCredit(a: CursorAccountRow) {
  creditOpen.value = creditOpen.value === a.account_hash ? '' : a.account_hash
}

function creditUsedPct(a: CursorAccountRow): number | null {
  const total = a.credit_total_cents ?? 0
  if (total <= 0) return null
  return 100 - ((a.credit_remaining_cents ?? 0) / total) * 100
}

function creditTitle(a: CursorAccountRow): string {
  const prefix = a.credit_label ? `${a.credit_label}：` : ''
  return `${prefix}${usd(a.credit_remaining_cents)} / ${usd(a.credit_total_cents)}，自动抵扣用量，对应 cursor.com 账单页 Credits`
}

function creditSoon(a: CursorAccountRow): boolean {
  if (!a.credit_expires_at) return false
  const ms = new Date(a.credit_expires_at).getTime() - Date.now()
  return !Number.isNaN(ms) && ms < 3 * 86_400_000
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
        RPC，信用余额来自 credit-grants），展示的是当前状态，不随看板时间范围筛选变化。
      </p>

      <div v-if="loaded && !items.length" class="empty">
        尚无 Cursor 账号快照。到采集端本机面板（默认 http://127.0.0.1:3848）加入 Cursor
        账号后，将随下一轮 Cursor 同步出现在这里。
      </div>

      <div v-else class="acct-grid">
        <article
          v-for="a in items"
          :key="a.account_hash"
          class="acct"
          @mouseenter="onEnter(a)"
          @mouseleave="onLeave"
        >
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
              <div class="meter-row">
                <span>API</span>
                <span
                  class="reset"
                  :class="{ soon: countdown(a.billing_cycle_end)?.soon }"
                  :title="a.billing_cycle_end ? `账期重置于 ${fmtTime(a.billing_cycle_end)}` : undefined"
                >{{ countdown(a.billing_cycle_end) ? `重置 ${countdown(a.billing_cycle_end)!.text}` : '' }}</span>
                <b class="val" title="套餐包含额度：已用 / 额度（plan.used / plan.limit）">{{ meterValue(a.api_percent, a.plan_used, a.plan_limit) }}</b>
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
                <b class="val" title="Auto 池：包含额度 + 附赠池的合计，用量按 autoPercentUsed 换算">{{ meterValue(a.auto_percent, a.auto_used, a.auto_limit) }}</b>
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
                <b class="val">{{ meterValue(a.bot_percent) }}</b>
              </div>
              <div class="bar" :class="barClass(a.bot_percent)"><i :style="{ width: barWidth(a.bot_percent) }" /></div>
            </div>
            <div v-else class="bot-note" title="Bot 用量走 Cursor 原生 RPC，只认 IDE 原生 access token；web 凭证或未拉到时无数据">
              Bot 无数据（web 凭证或暂未拉到）
            </div>
          </div>

          <div
            v-if="a.bonus_cents"
            class="meta"
            title="附赠池：本账期内随套餐附赠的额外用量，随账期重置；与信用余额不同"
          >附赠池 {{ usd(a.bonus_cents) }}</div>
          <div v-if="hasCredit(a)" class="credit">
            <button
              type="button"
              class="tag credit-btn"
              :class="{ soon: creditSoon(a) }"
              :title="creditTitle(a)"
              @click.stop="toggleCredit(a)"
            >{{ creditOpen === a.account_hash ? '收起信用余额' : '信用余额' }}</button>
            <div v-if="creditOpen === a.account_hash" class="credit-box">
              <div class="credit-head">
                <span class="credit-name">{{ a.credit_label || 'Credit' }}</span>
                <b class="credit-val">{{ usd(a.credit_remaining_cents) }} / {{ usd(a.credit_total_cents) }}</b>
              </div>
              <div class="bar" :class="barClass(creditUsedPct(a))"><i :style="{ width: barWidth(creditUsedPct(a)) }" /></div>
              <div v-if="a.credit_expires_at" class="credit-exp" :class="{ soon: creditSoon(a) }">
                到期 {{ fmtTime(a.credit_expires_at) }}
              </div>
            </div>
          </div>
          <div class="foot">快照 {{ fmtTime(a.fetched_at) }}</div>

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
.meta.soon {
  color: var(--amber);
}
.credit {
  display: flex;
  flex-direction: column;
  gap: 8px;
  align-items: flex-start;
}
.credit-btn {
  cursor: pointer;
}
.credit-btn:hover {
  border-color: #3d4b5e;
  color: var(--text);
}
.credit-btn.soon {
  border-color: #5a4630;
  color: var(--amber);
}
.credit-box {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--bg-elev);
}
.credit-head {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  gap: 10px;
}
.credit-name {
  font-size: 12px;
  color: var(--text);
}
.credit-val {
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}
.credit-exp {
  font-size: 11px;
  color: var(--muted);
}
.credit-exp.soon {
  color: var(--amber);
}
.foot {
  margin-top: auto;
  color: var(--muted);
  font-size: 11px;
  font-variant-numeric: tabular-nums;
}
</style>
