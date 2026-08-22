<script setup lang="ts">
import { computed, ref } from 'vue'
import type { BreakdownItem, ModelPrice } from '../api'
import { fmtMetric, fmtPct, fmtPerMillion, type Metric } from '../format'

const props = defineProps<{
  title: string
  items: BreakdownItem[]
  labels?: Record<string, string>
  showPricing?: boolean
}>()

const metric = ref<Metric>('tokens')
const hoveredKey = ref<string | null>(null)

const PALETTE = ['#3ee0b3', '#5b8def', '#e8b15a', '#c084fc', '#e07a7a', '#2dd4bf', '#f0abfc']
const OTHER_COLOR = '#64748b'
const MAX_SLICES = 8

const RATE_ROWS: Array<{
  key: 'input' | 'output' | 'cache_write' | 'cache_read' | 'reasoning'
  name: string
  color: string
}> = [
  { key: 'input', name: '输入', color: '#3b82f6' },
  { key: 'output', name: '输出', color: '#22c55e' },
  { key: 'cache_write', name: '缓存创建', color: '#f97316' },
  { key: 'cache_read', name: '缓存命中', color: '#a855f7' },
  { key: 'reasoning', name: '推理', color: '#e8b15a' },
]

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

function rateOf(p: ModelPrice, key: (typeof RATE_ROWS)[number]['key']): number | null {
  if (key === 'input') return p.input
  if (key === 'output') return p.output
  if (key === 'cache_read') return p.cache_read ?? p.input * 0.1
  if (key === 'cache_write') return p.cache_write ?? p.input * 1.25
  return p.reasoning ?? null
}

type Slice = {
  key: string
  label: string
  value: number
  pct: number
  color: string
  d: string
  pricing?: ModelPrice | null
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
      pricing: it.pricing,
    }
  })
})

const total = computed(() => slices.value.reduce((s, i) => s + i.value, 0))
const hasData = computed(() => slices.value.some((s) => s.value > 0))
const hovering = computed(() => hoveredKey.value != null)
const hovered = computed(() => slices.value.find((s) => s.key === hoveredKey.value) ?? null)

const hoveredRates = computed(() => {
  const p = hovered.value?.pricing
  if (!p) return []
  return RATE_ROWS.flatMap((row) => {
    const value = rateOf(p, row.key)
    if (value == null) return []
    return [{ ...row, value }]
  })
})
</script>

<template>
  <div class="card" :class="{ 'is-hover': hovering }">
    <div class="card-head">
      <h2>{{ title }}</h2>
      <div class="tabs">
        <button :class="{ active: metric === 'tokens' }" @click="metric = 'tokens'">Token</button>
        <button :class="{ active: metric === 'cost' }" @click="metric = 'cost'">费用</button>
      </div>
    </div>
    <div v-if="hasData" class="donut-body" @mouseleave="hoveredKey = null">
      <svg class="donut" :class="{ hovering }" viewBox="0 0 140 140" aria-hidden="true">
        <path
          v-for="s in slices"
          :key="s.key"
          :d="s.d"
          :fill="s.color"
          :class="{ on: hoveredKey === s.key }"
          @mouseenter="hoveredKey = s.key"
        />
        <text class="donut-total" x="70" y="68" text-anchor="middle">{{ fmtMetric(metric, total) }}</text>
        <text class="donut-unit" x="70" y="84" text-anchor="middle">{{ metric === 'cost' ? '费用' : 'Token' }}</text>
      </svg>
      <ul class="legend" :class="{ hovering }">
        <li
          v-for="s in slices"
          :key="s.key"
          :class="{ on: hoveredKey === s.key }"
          @mouseenter="hoveredKey = s.key"
        >
          <span class="swatch" :style="{ background: s.color }" />
          <span class="legend-key" :title="s.label">{{ s.label }}</span>
          <span class="legend-val">{{ fmtMetric(metric, s.value) }}</span>
          <span class="legend-pct">{{ fmtPct(s.pct) }}</span>
          <div v-if="showPricing && hoveredKey === s.key" class="tip">
            <p class="tip-h">{{ s.label }}</p>
            <div class="tip-sum">
              {{ fmtMetric(metric, s.value) }}
              <span>{{ fmtPct(s.pct) }}</span>
            </div>
            <template v-if="hoveredRates.length">
              <p class="tip-k">模型定价 · 每百万 Token</p>
              <div v-for="row in hoveredRates" :key="row.key" class="tip-row">
                <i :style="{ background: row.color }" />
                <span>{{ row.name }}</span>
                <b>{{ fmtPerMillion(row.value) }}</b>
              </div>
            </template>
            <p v-else-if="s.key === '其他'" class="tip-muted">多项合计，无单一报价</p>
            <p v-else class="tip-muted">无报价 · 已计入 Token、未计入费用</p>
          </div>
        </li>
      </ul>
    </div>
    <div v-else class="empty">无分布数据</div>
  </div>
</template>

<style scoped>
.card {
  position: relative;
  z-index: 0;
}
.card.is-hover {
  z-index: 5;
}
.donut-body {
  position: relative;
  display: flex;
  align-items: center;
  gap: 14px;
  min-height: 160px;
}
.donut {
  width: 140px;
  height: 140px;
  flex: 0 0 140px;
  overflow: visible;
}
.donut path {
  cursor: pointer;
  transition: opacity 0.15s ease-out, filter 0.15s ease-out;
}
.donut.hovering path {
  opacity: 0.28;
}
.donut.hovering path.on {
  opacity: 1;
  filter: brightness(1.12);
}
.donut-total {
  fill: var(--text);
  font-size: 13px;
  font-variant-numeric: tabular-nums;
  font-weight: 600;
  pointer-events: none;
}
.donut-unit {
  fill: var(--muted);
  font-size: 9px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  pointer-events: none;
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
  position: relative;
  display: grid;
  grid-template-columns: 8px minmax(0, 1fr) auto auto;
  gap: 6px;
  align-items: center;
  cursor: pointer;
  transition: opacity 0.15s ease-out;
}
.legend.hovering li {
  opacity: 0.28;
}
.legend.hovering li.on {
  opacity: 1;
  z-index: 1;
}
.legend li:nth-last-child(-n + 2) .tip {
  top: auto;
  bottom: calc(100% + 6px);
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
.tip {
  position: absolute;
  left: 0;
  top: calc(100% + 6px);
  z-index: 4;
  min-width: 188px;
  max-width: min(260px, 100%);
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
  margin: 0;
  font-weight: 600;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tip-sum {
  margin-top: 4px;
  font-variant-numeric: tabular-nums;
  color: var(--text);
}
.tip-sum span {
  margin-left: 8px;
  color: var(--muted);
}
.tip-k {
  margin: 8px 0 4px;
  font-size: 10px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
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
.tip-muted {
  margin: 8px 0 0;
  color: var(--muted);
  font-size: 11px;
}
</style>
