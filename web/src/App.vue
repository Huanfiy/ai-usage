<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { api, type BreakdownItem, type HostRow, type SeriesPoint, type SessionRow, type Summary } from './api'

const ranges = [
  { id: '24h', label: '24h', hours: 24 },
  { id: '7d', label: '7d', hours: 24 * 7 },
  { id: '30d', label: '30d', hours: 24 * 30 },
  { id: '90d', label: '90d', hours: 24 * 90 },
]

const range = ref('7d')
const host = ref('all')
const source = ref('')
const model = ref('')
const project = ref('')
const hideProjects = ref(false)
const by = ref<'tool' | 'model' | 'project' | 'host'>('tool')
const err = ref('')
const loading = ref(false)

const summary = ref<Summary | null>(null)
const points = ref<SeriesPoint[]>([])
const items = ref<BreakdownItem[]>([])
const sessions = ref<SessionRow[]>([])
const hosts = ref<HostRow[]>([])
const options = ref<{ sources: string[]; models: string[]; projects: string[] }>({
  sources: [],
  models: [],
  projects: [],
})
const newToken = ref('')

function windowRange() {
  const to = new Date()
  const hours = ranges.find((r) => r.id === range.value)?.hours ?? 24 * 7
  const from = new Date(to.getTime() - hours * 3600 * 1000)
  return { from: from.toISOString(), to: to.toISOString() }
}

const query = computed(() => ({
  ...windowRange(),
  host: host.value === 'all' ? undefined : host.value,
  source: source.value || undefined,
  model: model.value || undefined,
  project: hideProjects.value ? undefined : project.value || undefined,
  hide_projects: hideProjects.value,
}))

async function refresh() {
  loading.value = true
  err.value = ''
  try {
    const q = query.value
    const [s, ser, br, sess, hs, fo] = await Promise.all([
      api.summary(q),
      api.series(q),
      api.breakdown(q, by.value === 'tool' ? 'source' : by.value),
      api.sessions(q),
      api.hosts(),
      api.filters(q),
    ])
    summary.value = s
    points.value = ser.points
    items.value = br.items
    sessions.value = sess.items
    hosts.value = hs.items
    options.value = fo
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
  }
}

watch([range, host, source, model, project, hideProjects, by], refresh)
onMounted(refresh)

function fmtTokens(n: number) {
  if (n >= 1_000_000_000) return (n / 1_000_000_000).toFixed(2) + 'B'
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M'
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K'
  return String(n)
}
function fmtUsd(n: number) {
  if (n >= 100) return '$' + n.toFixed(0)
  if (n >= 1) return '$' + n.toFixed(2)
  return '$' + n.toFixed(4)
}
function fmtPct(n: number) {
  return (n * 100).toFixed(1) + '%'
}
function fmtDur(s: number) {
  if (s < 60) return s + 's'
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  return h > 0 ? `${h}h ${m}m` : `${m}m`
}
function fmtTime(iso: string) {
  const d = new Date(iso)
  return d.toLocaleString()
}

const chart = computed(() => {
  const pts = points.value
  if (!pts.length) return ''
  const w = 640
  const h = 200
  const pad = 28
  const max = Math.max(...pts.map((p) => p.tokens), 1)
  const coords = pts.map((p, i) => {
    const x = pad + (i * (w - pad * 2)) / Math.max(pts.length - 1, 1)
    const y = h - pad - (p.tokens / max) * (h - pad * 2)
    return [x, y] as const
  })
  const line = coords.map((c, i) => `${i ? 'L' : 'M'}${c[0].toFixed(1)},${c[1].toFixed(1)}`).join(' ')
  const area = `${line} L${coords[coords.length - 1][0].toFixed(1)},${h - pad} L${coords[0][0].toFixed(1)},${h - pad} Z`
  return { w, h, pad, line, area, max, first: pts[0]?.t, last: pts[pts.length - 1]?.t }
})

const maxBar = computed(() => Math.max(...items.value.map((i) => i.tokens), 1))

async function createHostToken() {
  const name = prompt('主机显示名（可重复）', 'new-host')
  if (name === null) return
  const r = await api.createToken(name)
  newToken.value = r.token
  await refresh()
}
</script>

<template>
  <div class="app">
    <header class="top">
      <div class="brand">
        <h1>AI <span>Usage</span></h1>
        <p>本地解析 · 多主机聚合 · 费用为估算非账单</p>
      </div>
      <div class="hosts">
        <button class="chip" :class="{ active: host === 'all' }" @click="host = 'all'">全部主机</button>
        <button
          v-for="h in hosts"
          :key="h.host_id"
          class="chip"
          :class="{ active: host === h.host_id }"
          @click="host = h.host_id"
        >
          {{ h.hostname }}
        </button>
      </div>
    </header>

    <div class="filters">
      <label>时间
        <select v-model="range">
          <option v-for="r in ranges" :key="r.id" :value="r.id">{{ r.label }}</option>
        </select>
      </label>
      <label>工具
        <select v-model="source">
          <option value="">全部</option>
          <option v-for="s in options.sources" :key="s" :value="s">{{ s }}</option>
        </select>
      </label>
      <label>模型
        <select v-model="model">
          <option value="">全部</option>
          <option v-for="m in options.models" :key="m" :value="m">{{ m }}</option>
        </select>
      </label>
      <label>项目
        <select v-model="project" :disabled="hideProjects">
          <option value="">全部</option>
          <option v-for="p in options.projects" :key="p" :value="p">{{ p }}</option>
        </select>
      </label>
      <label>分解
        <select v-model="by">
          <option value="tool">工具</option>
          <option value="model">模型</option>
          <option value="project">项目</option>
          <option value="host">主机</option>
        </select>
      </label>
      <label class="chk">隐私
        <span style="display:flex;align-items:center;gap:8px;padding:8px 10px;background:var(--bg-elev);border:1px solid var(--line);border-radius:8px;text-transform:none;letter-spacing:0">
          <input type="checkbox" v-model="hideProjects" /> 隐藏项目名
        </span>
      </label>
    </div>

    <p v-if="err" class="err">{{ err }}</p>

    <section class="kpis" v-if="summary">
      <div class="kpi">
        <div class="label">估算费用</div>
        <div class="value amber">{{ fmtUsd(summary.cost_usd) }}</div>
        <div class="hint">覆盖率 {{ fmtPct(summary.cost_coverage) }}</div>
      </div>
      <div class="kpi">
        <div class="label">Token</div>
        <div class="value mint">{{ fmtTokens(summary.tokens.total) }}</div>
        <div class="hint">入 {{ fmtTokens(summary.tokens.input) }} · 出 {{ fmtTokens(summary.tokens.output) }}</div>
      </div>
      <div class="kpi">
        <div class="label">缓存命中</div>
        <div class="value">{{ fmtPct(summary.cache_hit_rate) }}</div>
        <div class="hint">读 {{ fmtTokens(summary.tokens.cache_read) }} · 写 {{ fmtTokens(summary.tokens.cache_creation) }}</div>
      </div>
      <div class="kpi">
        <div class="label">会话</div>
        <div class="value">{{ summary.sessions }}</div>
        <div class="hint">{{ loading ? '刷新中…' : '按 last_message 计入窗口' }}</div>
      </div>
      <div class="kpi">
        <div class="label">主机 / 工具</div>
        <div class="value">{{ summary.hosts }} / {{ summary.sources }}</div>
        <div class="hint">推理 {{ fmtTokens(summary.tokens.reasoning) }}</div>
      </div>
    </section>

    <section class="grid">
      <div class="card">
        <h2>用量趋势</h2>
        <svg v-if="chart" class="chart" :viewBox="`0 0 ${chart.w} ${chart.h}`" preserveAspectRatio="none">
          <defs>
            <linearGradient id="mintFill" x1="0" x2="0" y1="0" y2="1">
              <stop offset="0%" stop-color="#3ee0b3" stop-opacity="0.35" />
              <stop offset="100%" stop-color="#3ee0b3" stop-opacity="0.02" />
            </linearGradient>
          </defs>
          <path class="area" :d="chart.area" />
          <path class="line" :d="chart.line" />
          <text class="axis" :x="28" :y="chart.h - 8">{{ fmtTime(chart.first || '') }}</text>
          <text class="axis" :x="chart.w - 28" :y="chart.h - 8" text-anchor="end">{{ fmtTime(chart.last || '') }}</text>
        </svg>
        <div v-else class="empty">窗口内没有 bucket</div>
      </div>
      <div class="card">
        <h2>分解</h2>
        <div class="tabs">
          <button :class="{ active: by === 'tool' }" @click="by = 'tool'">工具</button>
          <button :class="{ active: by === 'model' }" @click="by = 'model'">模型</button>
          <button :class="{ active: by === 'project' }" @click="by = 'project'">项目</button>
          <button :class="{ active: by === 'host' }" @click="by = 'host'">主机</button>
        </div>
        <div class="bars" v-if="items.length">
          <div class="bar-row" v-for="it in items.slice(0, 8)" :key="it.key">
            <span class="mono">{{ it.key }}</span>
            <div class="bar-track"><div class="bar-fill" :style="{ width: (100 * it.tokens / maxBar) + '%' }" /></div>
            <span class="num">{{ fmtTokens(it.tokens) }}</span>
          </div>
        </div>
        <div v-else class="empty">无分解数据</div>
      </div>
    </section>

    <section class="card">
      <h2>会话</h2>
      <table v-if="sessions.length">
        <thead>
          <tr>
            <th>工具</th>
            <th>项目</th>
            <th>开始</th>
            <th>结束</th>
            <th>时长</th>
            <th>活跃</th>
            <th>消息</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="s in sessions" :key="s.host_id + s.session_hash">
            <td class="mono">{{ s.source }}</td>
            <td>{{ s.project }}</td>
            <td>{{ fmtTime(s.first_message_at) }}</td>
            <td>{{ fmtTime(s.last_message_at) }}</td>
            <td>{{ fmtDur(s.duration_seconds) }}</td>
            <td>{{ fmtDur(s.active_seconds) }}</td>
            <td>{{ s.user_message_count }} / {{ s.message_count }}</td>
          </tr>
        </tbody>
      </table>
      <div v-else class="empty">窗口内没有会话</div>
    </section>

    <section class="card" style="margin-top:12px">
      <h2>主机与 token</h2>
      <table v-if="hosts.length">
        <thead>
          <tr><th>显示名</th><th>host_id</th><th>上次同步</th><th>agent</th></tr>
        </thead>
        <tbody>
          <tr v-for="h in hosts" :key="h.host_id">
            <td>{{ h.hostname }}</td>
            <td class="mono">{{ h.host_id }}</td>
            <td>{{ fmtTime(h.last_seen) }}</td>
            <td class="mono">{{ h.agent_version || '—' }}</td>
          </tr>
        </tbody>
      </table>
      <div class="settings">
        <button class="chip" @click="createHostToken">新建 ingest token</button>
        <span v-if="newToken" class="mono">新 token（只显示一次）：{{ newToken }}</span>
      </div>
    </section>
  </div>
</template>
