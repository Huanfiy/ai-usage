<script setup lang="ts">
import { computed, ref } from 'vue'
import type { ActivityCell } from '../api'
import { fmtMetric, type Metric } from '../format'

const props = defineProps<{ cells: ActivityCell[] }>()

const metric = ref<Metric>('tokens')

const DAYS = ['日', '一', '二', '三', '四', '五', '六']

const COL_EMPTY = { r: 24, g: 30, b: 39 }
const COL_MINT = { r: 62, g: 224, b: 179 }
const COL_AMBER = { r: 232, g: 177, b: 90 }

function hex(c: number): string {
  return Math.round(c).toString(16).padStart(2, '0')
}

function mix(
  from: { r: number; g: number; b: number },
  to: { r: number; g: number; b: number },
  t: number,
): string {
  const u = Math.min(1, Math.max(0, t))
  const r = from.r + (to.r - from.r) * u
  const g = from.g + (to.g - from.g) * u
  const b = from.b + (to.b - from.b) * u
  return `#${hex(r)}${hex(g)}${hex(b)}`
}

function cellFill(v: number, max: number, cost: boolean): string {
  if (v <= 0 || max <= 0) return mix(COL_EMPTY, COL_EMPTY, 0)
  const t = Math.sqrt(v / max)
  return mix(COL_EMPTY, cost ? COL_AMBER : COL_MINT, 0.28 + 0.72 * t)
}

const layout = {
  padL: 22,
  padT: 16,
  padR: 8,
  padB: 22,
  cell: 14,
  gap: 3,
}

const step = layout.cell + layout.gap
const svgW = layout.padL + 24 * step - layout.gap + layout.padR
const svgH = layout.padT + 7 * step - layout.gap + layout.padB

const grid = computed(() => {
  const g = Array.from({ length: 7 }, () => Array.from({ length: 24 }, () => 0))
  // UTC (dow, hour) → local by a fixed offset; DST not applied.
  const off = Math.round(-new Date().getTimezoneOffset() / 60)
  for (const c of props.cells) {
    const dow = ((Number(c.dow) % 7) + 7) % 7
    const hour = ((Number(c.hour) % 24) + 24) % 24
    let idx = dow * 24 + hour + off
    idx = ((idx % 168) + 168) % 168
    const v = metric.value === 'tokens' ? Number(c.tokens) || 0 : Number(c.cost_usd) || 0
    g[Math.floor(idx / 24)][idx % 24] += v
  }
  const max = Math.max(0, ...g.flat())
  return { g, max }
})

const hourLabels = [0, 6, 12, 18]
const legendStops = [0, 0.33, 0.66, 1]
</script>

<template>
  <div class="card">
    <div class="card-head">
      <h2>分时活跃</h2>
      <div class="tabs">
        <button :class="{ active: metric === 'tokens' }" @click="metric = 'tokens'">Token</button>
        <button :class="{ active: metric === 'cost' }" @click="metric = 'cost'">费用</button>
      </div>
    </div>
    <svg class="heat" :viewBox="`0 0 ${svgW} ${svgH}`" role="img" aria-label="7×24 分时热力图">
      <text
        v-for="h in hourLabels"
        :key="'h' + h"
        class="axis"
        :x="layout.padL + h * step + layout.cell / 2"
        :y="12"
        text-anchor="middle"
      >
        {{ h }}
      </text>
      <text
        v-for="(d, di) in DAYS"
        :key="'d' + di"
        class="axis"
        :x="layout.padL - 6"
        :y="layout.padT + di * step + layout.cell * 0.78"
        text-anchor="end"
      >
        {{ d }}
      </text>
      <g v-for="(row, di) in grid.g" :key="'r' + di">
        <rect
          v-for="(v, hi) in row"
          :key="di + '-' + hi"
          :x="layout.padL + hi * step"
          :y="layout.padT + di * step"
          :width="layout.cell"
          :height="layout.cell"
          rx="2"
          :fill="cellFill(v, grid.max, metric === 'cost')"
        >
          <title>{{ DAYS[di] }} {{ hi }}:00 · {{ fmtMetric(metric, v) }}</title>
        </rect>
      </g>
      <text class="axis" :x="layout.padL" :y="svgH - 6">少</text>
      <rect
        v-for="(t, i) in legendStops"
        :key="'lg' + i"
        :x="layout.padL + 18 + i * (layout.cell + 2)"
        :y="svgH - 16"
        :width="layout.cell"
        :height="10"
        rx="2"
        :fill="cellFill(t === 0 ? 0 : t, 1, metric === 'cost')"
      />
      <text class="axis" :x="layout.padL + 18 + 4 * (layout.cell + 2) + 4" :y="svgH - 6">多</text>
    </svg>
  </div>
</template>

<style scoped>
.heat {
  width: 100%;
  height: auto;
  display: block;
}
.axis {
  fill: var(--muted);
  font-size: 9px;
}
</style>
