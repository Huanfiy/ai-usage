<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
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
import FilterPill from './components/FilterPill.vue'
import Heatmap from './components/Heatmap.vue'
import KpiRow from './components/KpiRow.vue'
import SessionList from './components/SessionList.vue'
import SettingsPage from './components/SettingsPage.vue'
import TimeRangeBar from './components/TimeRangeBar.vue'
import TrendChart from './components/TrendChart.vue'
import { presetLabel, resolvePreset, type AppliedRange } from './timeRange'

type Page = 'dash' | 'settings'

const HIDE_KEY = 'ai-usage.hideProjects'

function pageFromPath(path = location.pathname): Page {
  return path.replace(/\/+$/, '').endsWith('/settings') ? 'settings' : 'dash'
}

function readHide(): boolean {
  try {
    return localStorage.getItem(HIDE_KEY) === '1'
  } catch {
    return false
  }
}

const page = ref<Page>(pageFromPath())
const host = ref('all')
const source = ref('')
const model = ref('')
const project = ref('')
const hideProjects = ref(readHide())
const err = ref('')
const loading = ref(false)

const initRange = resolvePreset('7d')
const applied = ref<AppliedRange>({ ...initRange, preset: '7d' })

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

const rangeLabel = computed(() => presetLabel(applied.value.preset, applied.value.from, applied.value.to))

const query = computed(() => ({
  from: applied.value.from.toISOString(),
  to: applied.value.to.toISOString(),
  host: host.value === 'all' ? undefined : host.value,
  source: source.value || undefined,
  model: model.value || undefined,
  project: hideProjects.value ? undefined : project.value || undefined,
  hide_projects: hideProjects.value,
}))

const sourceOpts = computed(() => options.value.sources.map((s) => ({ value: s, label: s })))
const modelOpts = computed(() => options.value.models.map((m) => ({ value: m, label: m })))
const projectOpts = computed(() => options.value.projects.map((p) => ({ value: p, label: p })))
const hostOpts = computed(() =>
  hosts.value.map((h) => ({ value: h.host_id, label: hostLabel(h.host_id, h.hostname) })),
)

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
    if (host.value !== 'all' && !hosts.value.some((h) => h.host_id === host.value)) {
      host.value = 'all'
    }
    options.value = fo
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
  }
}

function onRangeApply(r: AppliedRange) {
  applied.value = r
}

function go(next: Page) {
  const path = next === 'settings' ? '/settings' : '/'
  if (location.pathname !== path) history.pushState({}, '', path)
  page.value = next
}

function onPop() {
  page.value = pageFromPath()
}

watch(hideProjects, (v) => {
  try {
    localStorage.setItem(HIDE_KEY, v ? '1' : '0')
  } catch {
    /* ignore quota */
  }
  if (v) project.value = ''
})

watch([applied, host, source, model, project, hideProjects], refresh)
onMounted(() => {
  window.addEventListener('popstate', onPop)
  refresh()
})
onUnmounted(() => window.removeEventListener('popstate', onPop))

const hostLabels = computed(() => {
  const m: Record<string, string> = {}
  for (const h of hosts.value) m[h.host_id] = hostLabel(h.host_id, h.hostname)
  return m
})

function hostLabel(hostId: string, hostname: string): string {
  if (hostId.startsWith('acct:')) return `账号 · ${hostname}`
  return hostname
}
</script>

<template>
  <div class="app">
    <header class="top">
      <div class="brand">
        <h1>AI <span>Usage</span></h1>
        <p>本地解析 · 多主机聚合 · 费用为估算非账单</p>
      </div>
      <nav class="nav" aria-label="页面">
        <a href="/" :class="{ active: page === 'dash' }" @click.prevent="go('dash')">看板</a>
        <a href="/settings" :class="{ active: page === 'settings' }" @click.prevent="go('settings')">设置</a>
      </nav>
    </header>

    <p v-if="err" class="err">{{ err }}</p>

    <template v-if="page === 'dash'">
      <div class="toolbar">
        <TimeRangeBar @apply="onRangeApply" />
        <div class="filter-pills">
          <FilterPill v-model="source" label="工具" :options="sourceOpts">
            <svg viewBox="0 0 16 16">
              <path d="M4 5l3 3-3 3" />
              <path d="M8.5 11.5H12" />
            </svg>
          </FilterPill>
          <FilterPill v-model="model" label="模型" :options="modelOpts">
            <svg viewBox="0 0 16 16">
              <rect x="4" y="4" width="8" height="8" rx="1.4" />
              <path d="M8 2.5v1.5M8 12v1.5M2.5 8h1.5M12 8h1.5" />
            </svg>
          </FilterPill>
          <FilterPill v-if="!hideProjects" v-model="project" label="项目" :options="projectOpts">
            <svg viewBox="0 0 16 16">
              <path d="M2.5 5.2V12a1.2 1.2 0 0 0 1.2 1.2h8.6A1.2 1.2 0 0 0 13.5 12V6.2A1.2 1.2 0 0 0 12.3 5H8L6.6 3.6H3.7A1.2 1.2 0 0 0 2.5 4.8V5.2z" />
            </svg>
          </FilterPill>
          <FilterPill v-model="host" label="终端" all-value="all" :options="hostOpts">
            <svg viewBox="0 0 16 16">
              <rect x="2.5" y="3" width="11" height="8" rx="1.2" />
              <path d="M6 13.2h4M8 11v2.2" />
            </svg>
          </FilterPill>
        </div>
      </div>

      <KpiRow v-if="summary" :summary="summary" :loading="loading" />
      <section class="grid">
        <TrendChart :points="points" :range-label="rangeLabel" />
        <Heatmap :cells="activity" />
      </section>
      <section class="donuts">
        <DonutCard title="终端分布" :items="distributions.host" :labels="hostLabels" />
        <DonutCard title="工具分布" :items="distributions.source" />
        <DonutCard title="模型分布" :items="distributions.model" show-pricing />
        <DonutCard title="项目分布" :items="distributions.project" />
      </section>
      <SessionList :sessions="sessions" />
    </template>

    <SettingsPage
      v-else
      :hide-projects="hideProjects"
      :hosts="hosts"
      @update:hide-projects="hideProjects = $event"
      @changed="refresh"
    />
  </div>
</template>
