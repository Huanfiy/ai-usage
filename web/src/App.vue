<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import {
  api,
  type ActivityCell,
  type BreakdownItem,
  type Distributions,
  type HostRow,
  type SeriesPoint,
  type SessionRow,
  type Summary,
} from './api'
import DonutCard from './components/DonutCard.vue'
import Heatmap from './components/Heatmap.vue'
import KpiRow from './components/KpiRow.vue'
import TrendChart from './components/TrendChart.vue'
import { fmtDur, fmtTime } from './format'

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
const err = ref('')
const loading = ref(false)

const summary = ref<Summary | null>(null)
const points = ref<SeriesPoint[]>([])
const distributions = ref<Distributions>({ host: [], source: [], model: [], project: [] })
const activity = ref<ActivityCell[]>([])
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

function asList(v: BreakdownItem[] | undefined): BreakdownItem[] {
  return Array.isArray(v) ? v : []
}

async function refresh() {
  loading.value = true
  err.value = ''
  try {
    const q = query.value
    const [s, ser, dist, act, sess, hs, fo] = await Promise.all([
      api.summary(q),
      api.series(q),
      api.distributions(q),
      api.activity(q),
      api.sessions(q),
      api.hosts(),
      api.filters(q),
    ])
    summary.value = s
    points.value = ser.points ?? []
    distributions.value = {
      host: asList(dist.host),
      source: asList(dist.source),
      model: asList(dist.model),
      project: asList(dist.project),
    }
    activity.value = act.cells ?? []
    sessions.value = sess.items
    hosts.value = hs.items
    options.value = fo
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
  }
}

watch([range, host, source, model, project, hideProjects], refresh)
onMounted(refresh)

const hostLabels = computed(() => {
  const m: Record<string, string> = {}
  for (const h of hosts.value) m[h.host_id] = h.hostname
  return m
})

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
      <label class="chk">隐私
        <span style="display:flex;align-items:center;gap:8px;padding:8px 10px;background:var(--bg-elev);border:1px solid var(--line);border-radius:8px;text-transform:none;letter-spacing:0">
          <input type="checkbox" v-model="hideProjects" /> 隐藏项目名
        </span>
      </label>
    </div>

    <p v-if="err" class="err">{{ err }}</p>

    <KpiRow v-if="summary" :summary="summary" :loading="loading" />

    <section class="grid">
      <TrendChart :points="points" />
      <Heatmap :cells="activity" />
    </section>

    <section class="donuts">
      <DonutCard title="终端分布" :items="distributions.host" :labels="hostLabels" />
      <DonutCard title="工具分布" :items="distributions.source" />
      <DonutCard title="模型分布" :items="distributions.model" />
      <DonutCard title="项目分布" :items="distributions.project" />
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
