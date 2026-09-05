<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { HostRow, SessionRow, TokenTotals } from '../api'
import { fmtDur, fmtSessionSpan, fmtTokens } from '../format'

const props = defineProps<{ sessions: SessionRow[]; hosts?: HostRow[] }>()

const PAGE_SIZES = [10, 20, 50]
const pageSize = ref(10)
const page = ref(1)
const tokenTip = ref<string | null>(null)
const colTip = ref<string | null>(null)

const PARTS: { key: keyof TokenTotals; name: string; color: string }[] = [
  { key: 'input', name: '入', color: '#3b82f6' },
  { key: 'output', name: '出', color: '#22c55e' },
  { key: 'cache_read', name: '读', color: '#a855f7' },
  { key: 'cache_creation', name: '创建', color: '#f97316' },
  { key: 'reasoning', name: '推理', color: '#e8b15a' },
]

const COL_TIPS: {
  key: string
  label: string
  title: string
  body: string
  end?: boolean
  parts?: boolean
}[] = [
  {
    key: 'duration',
    label: '时长',
    title: '时长',
    body: '首条到末条消息的墙钟跨度。中间空闲也算进去，所以通常长于活跃。',
  },
  {
    key: 'active',
    label: '活跃',
    title: '活跃',
    body: '各轮用户消息之后，模型实际在出回复的时间合计。等人下一条不算。',
  },
  {
    key: 'messages',
    label: '消息',
    title: '消息',
    body: '用户消息 / 全部消息。左边是用户轮次，右边含助手与其它角色事件。',
    end: true,
  },
  {
    key: 'tokens',
    label: 'Token',
    title: 'Token',
    body: '本会话五项合计。单元格可再悬停看数量。Codex / Grok 子会话、Pi 派生会话或排除来源的会话可能为 0。',
    end: true,
    parts: true,
  },
]

const tzByHost = computed(() => {
  const m = new Map<string, string>()
  for (const h of props.hosts ?? []) {
    if (h.timezone) m.set(h.host_id, h.timezone)
  }
  return m
})

const total = computed(() => props.sessions.length)
const totalPages = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)))

const pageItems = computed(() => {
  const start = (page.value - 1) * pageSize.value
  return props.sessions.slice(start, start + pageSize.value)
})

const fromN = computed(() => (total.value === 0 ? 0 : (page.value - 1) * pageSize.value + 1))
const toN = computed(() => Math.min(total.value, page.value * pageSize.value))

const pageNums = computed(() => {
  const t = totalPages.value
  const cur = page.value
  if (t <= 7) return Array.from({ length: t }, (_, i) => i + 1)
  let start = Math.max(1, cur - 3)
  let end = Math.min(t, start + 6)
  start = Math.max(1, end - 6)
  return Array.from({ length: end - start + 1 }, (_, i) => start + i)
})

watch(pageSize, () => {
  page.value = 1
})

watch(
  () => props.sessions,
  () => {
    if (page.value > totalPages.value) page.value = totalPages.value
  },
)

function goto(p: number) {
  page.value = Math.min(totalPages.value, Math.max(1, p))
}

function sessionKey(s: SessionRow): string {
  return s.host_id + s.session_hash
}

function sessionTz(s: SessionRow): string | null {
  return tzByHost.value.get(s.host_id) ?? null
}

function tokenParts(s: SessionRow) {
  const t = s.tokens
  if (!t) return []
  return PARTS.map((p) => ({ ...p, value: t[p.key] ?? 0 }))
}

function tipBelow(s: SessionRow): boolean {
  return pageItems.value.findIndex((x) => sessionKey(x) === sessionKey(s)) < 2
}
</script>

<template>
  <section class="card" :class="{ 'has-tip': tokenTip || colTip }">
    <div class="card-head">
      <h2>会话列表</h2>
      <label class="page-size">
        每页
        <select v-model.number="pageSize">
          <option v-for="n in PAGE_SIZES" :key="n" :value="n">{{ n }}</option>
        </select>
      </label>
    </div>
    <table v-if="pageItems.length">
      <thead>
        <tr>
          <th>工具</th>
          <th>项目</th>
          <th>时间</th>
          <th v-for="c in COL_TIPS" :key="c.key">
            <span
              class="col-label"
              @mouseenter="colTip = c.key"
              @mouseleave="colTip = null"
            >
              {{ c.label }}
              <div v-if="colTip === c.key" class="tip below col" :class="{ end: c.end }">
                <p class="tip-h">{{ c.title }}</p>
                <p class="tip-p">{{ c.body }}</p>
                <template v-if="c.parts">
                  <div v-for="p in PARTS" :key="p.key" class="tip-row">
                    <i :style="{ background: p.color }" />
                    <span>{{ p.name }}</span>
                  </div>
                </template>
              </div>
            </span>
          </th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="s in pageItems" :key="sessionKey(s)">
          <td class="mono">{{ s.source }}</td>
          <td>{{ s.project }}</td>
          <td class="mono time">{{ fmtSessionSpan(s.first_message_at, s.last_message_at, sessionTz(s)) }}</td>
          <td>{{ fmtDur(s.duration_seconds) }}</td>
          <td>{{ fmtDur(s.active_seconds) }}</td>
          <td>{{ s.user_message_count }} / {{ s.message_count }}</td>
          <td
            class="token-cell"
            @mouseenter="tokenTip = sessionKey(s)"
            @mouseleave="tokenTip = null"
          >
            {{ fmtTokens(s.tokens?.total ?? 0) }}
            <div v-if="tokenTip === sessionKey(s)" class="tip" :class="{ below: tipBelow(s) }">
              <p class="tip-h">Token {{ fmtTokens(s.tokens?.total ?? 0) }}</p>
              <div v-for="p in tokenParts(s)" :key="p.key" class="tip-row">
                <i :style="{ background: p.color }" />
                <span>{{ p.name }}</span>
                <b>{{ fmtTokens(p.value) }}</b>
              </div>
            </div>
          </td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">窗口内没有会话</div>
    <div v-if="total" class="pager">
      <span class="pager-meta">第 {{ fromN }}–{{ toN }} 条，共 {{ total }} 条</span>
      <div class="pager-btns">
        <button type="button" :disabled="page <= 1" @click="goto(page - 1)">上一页</button>
        <button
          v-for="n in pageNums"
          :key="n"
          type="button"
          :class="{ active: n === page }"
          @click="goto(n)"
        >
          {{ n }}
        </button>
        <button type="button" :disabled="page >= totalPages" @click="goto(page + 1)">下一页</button>
      </div>
    </div>
  </section>
</template>

<style scoped>
.card {
  position: relative;
  z-index: 0;
}
.card.has-tip {
  z-index: 5;
}
.time {
  white-space: nowrap;
}
.token-cell {
  position: relative;
  cursor: default;
}
.col-label {
  position: relative;
  display: inline-block;
  border-bottom: 1px dashed color-mix(in srgb, var(--muted) 60%, transparent);
  cursor: help;
}
.tip.below {
  bottom: auto;
  top: calc(100% + 6px);
}
.tip.col {
  min-width: 196px;
  max-width: 240px;
}
.tip.col:not(.end) {
  left: 0;
  right: auto;
}
.tip {
  position: absolute;
  right: 0;
  bottom: calc(100% + 6px);
  z-index: 4;
  min-width: 148px;
  pointer-events: none;
  background: rgba(11, 14, 18, 0.94);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 10px 12px;
  box-shadow: var(--shadow);
  backdrop-filter: blur(8px);
  font-size: 12px;
}
.tip-h {
  margin: 0 0 6px;
  font-weight: 600;
  color: var(--text);
}
.tip-p {
  margin: 0;
  color: var(--muted);
  line-height: 1.5;
  font-weight: 400;
}
.tip-p + .tip-row {
  margin-top: 8px;
}
.tip-row {
  display: grid;
  grid-template-columns: 8px 1fr auto;
  gap: 6px;
  align-items: center;
  margin-top: 3px;
  font-variant-numeric: tabular-nums;
  color: var(--muted);
}
.tip-row i {
  width: 8px;
  height: 8px;
  border-radius: 99px;
}
.tip-row b {
  font-weight: 600;
  color: var(--text);
}
</style>
