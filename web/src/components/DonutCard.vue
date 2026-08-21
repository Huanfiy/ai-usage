<script setup lang="ts">
import { computed, ref } from 'vue'
import type { BreakdownItem } from '../api'
import { fmtMetric, fmtPct, type Metric } from '../format'

const props = defineProps<{
  title: string
  items: BreakdownItem[]
  labels?: Record<string, string>
}>()

const metric = ref<Metric>('tokens')

const PALETTE = ['#3ee0b3', '#5b8def', '#e8b15a', '#c084fc', '#e07a7a', '#2dd4bf', '#f0abfc']
const OTHER_COLOR = '#64748b'
const MAX_SLICES = 8

function valueOf(it: BreakdownItem, m: Metric): number {
  return m === 'cost' ? Number(it.cost_usd) || 0 : Number(it.tokens) || 0
}

function polar(cx: number, cy: number, r: number, deg: number) {
  const rad = ((deg - 90) * Math.PI) / 180
  return { x: cx + r * Math.cos(rad), y: cy + r * Math.sin(rad) }
}

function donutArc(cx: number, cy: number, r0: number, r1: number, a0: number, a1: number): string {
  const sweep = a1 - a0
  if (sweep <= 0.001) return ''
  if (sweep >= 359.999) {
    return donutArc(cx, cy, r0, r1, a0, a0 + 180) + ' ' + donutArc(cx, cy, r0, r1, a0 + 180, a0 + 360)
  }
  const large = sweep > 180 ? 1 : 0
  const p0 = polar(cx, cy, r1, a0)
  const p1 = polar(cx, cy, r1, a1)
  const p2 = polar(cx, cy, r0, a1)
  const p3 = polar(cx, cy, r0, a0)
  return [
    `M${p0.x.toFixed(3)},${p0.y.toFixed(3)}`,
    `A${r1},${r1} 0 ${large} 1 ${p1.x.toFixed(3)},${p1.y.toFixed(3)}`,
    `L${p2.x.toFixed(3)},${p2.y.toFixed(3)}`,
    `A${r0},${r0} 0 ${large} 0 ${p3.x.toFixed(3)},${p3.y.toFixed(3)}`,
    'Z',
  ].join(' ')
}

function labelOf(key: string): string {
  if (key === '其他') return key
  return props.labels?.[key] || key
}

type Slice = {
  key: string
  label: string
  value: number
  pct: number
  color: string
  d: string
}

const slices = computed((): Slice[] => {
  const m = metric.value
  const sorted = [...props.items].sort((a, b) => valueOf(b, m) - valueOf(a, m))
  let collapsed: BreakdownItem[]
  if (sorted.length > MAX_SLICES) {
    const head = sorted.slice(0, MAX_SLICES - 1)
    const tail = sorted.slice(MAX_SLICES - 1)
    collapsed = [
      ...head,
      {
        key: '其他',
        tokens: tail.reduce((s, i) => s + (Number(i.tokens) || 0), 0),
        cost_usd: tail.reduce((s, i) => s + (Number(i.cost_usd) || 0), 0),
        share: 0,
      },
    ]
  } else {
    collapsed = sorted
  }
  const total = collapsed.reduce((s, i) => s + valueOf(i, m), 0)
  const cx = 70
  const cy = 70
  const r1 = 64
  const r0 = 42
  const gap = collapsed.length > 1 ? 1.2 : 0
  let angle = 0
  return collapsed.map((it, i) => {
    const value = valueOf(it, m)
    const pct = total > 0 ? value / total : 0
    const sweep = pct * 360
    const pad = sweep > gap * 2 ? gap / 2 : 0
    const d = donutArc(cx, cy, r0, r1, angle + pad, angle + sweep - pad)
    angle += sweep
    return {
      key: it.key,
      label: labelOf(it.key),
      value,
      pct,
      color: it.key === '其他' ? OTHER_COLOR : PALETTE[i % PALETTE.length],
      d,
    }
  })
})

const total = computed(() => slices.value.reduce((s, i) => s + i.value, 0))
const hasData = computed(() => slices.value.some((s) => s.value > 0))
</script>

<template>
  <div class="card">
    <div class="card-head">
      <h2>{{ title }}</h2>
      <div class="tabs">
        <button :class="{ active: metric === 'tokens' }" @click="metric = 'tokens'">Token</button>
        <button :class="{ active: metric === 'cost' }" @click="metric = 'cost'">费用</button>
      </div>
    </div>
    <div v-if="hasData" class="donut-body">
      <svg class="donut" viewBox="0 0 140 140" aria-hidden="true">
        <path v-for="s in slices" :key="s.key" :d="s.d" :fill="s.color" />
        <text class="donut-total" x="70" y="68" text-anchor="middle">{{ fmtMetric(metric, total) }}</text>
        <text class="donut-unit" x="70" y="84" text-anchor="middle">{{ metric === 'cost' ? '费用' : 'Token' }}</text>
      </svg>
      <ul class="legend">
        <li v-for="s in slices" :key="s.key">
          <span class="swatch" :style="{ background: s.color }" />
          <span class="legend-key" :title="s.label">{{ s.label }}</span>
          <span class="legend-val">{{ fmtMetric(metric, s.value) }}</span>
          <span class="legend-pct">{{ fmtPct(s.pct) }}</span>
        </li>
      </ul>
    </div>
    <div v-else class="empty">无分布数据</div>
  </div>
</template>

<style scoped>
.donut-body {
  display: flex;
  align-items: center;
  gap: 14px;
  min-height: 160px;
}
.donut {
  width: 140px;
  height: 140px;
  flex: 0 0 140px;
}
.donut-total {
  fill: var(--text);
  font-size: 13px;
  font-variant-numeric: tabular-nums;
  font-weight: 600;
}
.donut-unit {
  fill: var(--muted);
  font-size: 9px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}
.legend {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 5px;
  flex: 1;
  min-width: 0;
  font-size: 11px;
}
.legend li {
  display: grid;
  grid-template-columns: 8px minmax(0, 1fr) auto auto;
  gap: 6px;
  align-items: center;
}
.swatch {
  width: 8px;
  height: 8px;
  border-radius: 2px;
}
.legend-key {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
}
.legend-val,
.legend-pct {
  font-variant-numeric: tabular-nums;
  color: var(--muted);
}
.legend-pct {
  min-width: 42px;
  text-align: right;
}
</style>
