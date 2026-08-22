<script setup lang="ts">
import { computed, ref } from 'vue'
import type { ActivityCell } from '../api'
import { fmtTokens } from '../format'

const props = defineProps<{ cells: ActivityCell[] }>()

const DAYS = ['日', '一', '二', '三', '四', '五', '六']

const COL_EMPTY = { r: 24, g: 30, b: 39 }
const COL_MINT = { r: 62, g: 224, b: 179 }

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

function cellFill(v: number, max: number): string {
  if (v <= 0 || max <= 0) return mix(COL_EMPTY, COL_EMPTY, 0)
  const t = Math.sqrt(v / max)
  return mix(COL_EMPTY, COL_MINT, 0.28 + 0.72 * t)
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
  const off = Math.round(-new Date().getTimezoneOffset() / 60)
  for (const c of props.cells) {
    const dow = ((Number(c.dow) % 7) + 7) % 7
    const hour = ((Number(c.hour) % 24) + 24) % 24
    let idx = dow * 24 + hour + off
    idx = ((idx % 168) + 168) % 168
    g[Math.floor(idx / 24)][idx % 24] += Number(c.tokens) || 0
  }
  const max = Math.max(0, ...g.flat())
  return { g, max }
})

const hourLabels = [0, 6, 12, 18]
const legendStops = [0, 0.33, 0.66, 1]

const hover = ref<{ di: number; hi: number; v: number } | null>(null)
const tipPos = ref({ x: 0, y: 0 })

function hourLabel(h: number): string {
  return String(h).padStart(2, '0') + ':00'
}

function onMove(ev: MouseEvent) {
  const svg = ev.currentTarget as SVGSVGElement
  const rect = svg.getBoundingClientRect()
  const x = ((ev.clientX - rect.left) / rect.width) * svgW
  const y = ((ev.clientY - rect.top) / rect.height) * svgH
  const hi = Math.floor((x - layout.padL) / step)
  const di = Math.floor((y - layout.padT) / step)
  if (hi < 0 || hi > 23 || di < 0 || di > 6) {
    hover.value = null
    return
  }
  const localX = x - layout.padL - hi * step
  const localY = y - layout.padT - di * step
  if (localX > layout.cell || localY > layout.cell) {
    hover.value = null
    return
  }
  hover.value = { di, hi, v: grid.value.g[di][hi] }
  const sx = rect.width / svgW
  const sy = rect.height / svgH
  tipPos.value = {
    x: (layout.padL + hi * step + layout.cell / 2) * sx,
    y: (layout.padT + di * step) * sy,
  }
}
</script>

<template>
  <div class="card heat-card" :class="{ 'is-hover': hover }">
    <div class="card-head">
      <h2>分时活跃</h2>
      <span class="heat-metric">Token</span>
    </div>
    <div class="heat-body">
      <svg
        class="heat"
        :viewBox="`0 0 ${svgW} ${svgH}`"
        role="img"
        aria-label="7×24 Token 分时热力图"
        @mousemove="onMove"
        @mouseleave="hover = null"
      >
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
            :fill="cellFill(v, grid.max)"
            :class="{ on: hover?.di === di && hover?.hi === hi }"
          />
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
          :fill="cellFill(t === 0 ? 0 : t, 1)"
        />
        <text class="axis" :x="layout.padL + 18 + 4 * (layout.cell + 2) + 4" :y="svgH - 6">多</text>
      </svg>
      <div v-if="hover" class="tip" :style="{ left: tipPos.x + 'px', top: tipPos.y + 'px' }">
        <p class="tip-h">周{{ DAYS[hover.di] }} {{ hourLabel(hover.hi) }}</p>
        <div class="tip-row">Token {{ fmtTokens(hover.v) }}</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.heat-card {
  min-width: 0;
  position: relative;
  z-index: 0;
}
.heat-card.is-hover {
  z-index: 5;
}
.heat-body {
  position: relative;
}
.heat {
  width: 100%;
  height: auto;
  display: block;
}
.heat rect.on {
  stroke: rgba(231, 237, 245, 0.55);
  stroke-width: 1;
}
.axis {
  fill: var(--muted);
  font-size: 9px;
}
.heat-metric {
  font-size: 11px;
  color: var(--muted);
  letter-spacing: 0.04em;
}
.tip {
  position: absolute;
  z-index: 2;
  transform: translate(-50%, calc(-100% - 8px));
  pointer-events: none;
  white-space: nowrap;
  background: rgba(11, 14, 18, 0.94);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 8px 12px;
  box-shadow: var(--shadow);
  backdrop-filter: blur(8px);
  font-size: 12px;
}
.tip-h {
  margin: 0 0 4px;
  font-weight: 600;
  color: var(--text);
}
.tip-row {
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
</style>
