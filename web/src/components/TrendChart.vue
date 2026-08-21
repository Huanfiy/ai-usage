<script setup lang="ts">
import { computed, ref } from 'vue'
import type { SeriesPoint } from '../api'
import { fmtAxisTokens, fmtInt, fmtTime, fmtUsd } from '../format'

const props = defineProps<{ points: SeriesPoint[]; rangeLabel?: string }>()

const COL = {
  input: '#3b82f6',
  output: '#22c55e',
  cacheCreation: '#f97316',
  cacheRead: '#a855f7',
  cost: '#f43f5e',
}

const SERIES = [
  { key: 'input' as const, name: '输入 Tokens', color: COL.input, fill: 'url(#trendIn)' },
  { key: 'output' as const, name: '输出 Tokens', color: COL.output, fill: 'url(#trendOut)' },
  { key: 'cacheCreation' as const, name: '缓存创建', color: COL.cacheCreation, fill: 'url(#trendCc)' },
  { key: 'cacheRead' as const, name: '缓存命中', color: COL.cacheRead, fill: 'url(#trendCr)' },
]

type Row = {
  t: string
  input: number
  output: number
  cacheCreation: number
  cacheRead: number
  cost: number
  x: number
  yInTop: number
  yOutTop: number
  yCcTop: number
  yCrTop: number
  yInBot: number
  yOutBot: number
  yCcBot: number
  yCrBot: number
  yCost: number
}

const hover = ref(-1)
const tipPos = ref({ x: 0, y: 0 })

const W = 860
const H = 220
const padL = 48
const padR = 48
const padT = 12
const padB = 28

function bandPath(rows: Row[], top: (r: Row) => number, bot: (r: Row) => number): string {
  if (!rows.length) return ''
  let d = `M${rows[0].x},${top(rows[0])}`
  for (let i = 1; i < rows.length; i++) d += ` L${rows[i].x},${top(rows[i])}`
  for (let i = rows.length - 1; i >= 0; i--) d += ` L${rows[i].x},${bot(rows[i])}`
  return d + ' Z'
}

const view = computed(() => {
  const pts = props.points
  const n = pts.length
  if (!n) return null
  const innerW = W - padL - padR
  const innerH = H - padT - padB
  const rows0 = pts.map((p) => ({
    t: p.t,
    input: Number(p.input) || 0,
    output: Number(p.output) || 0,
    cacheCreation: Number(p.cache_creation) || 0,
    cacheRead: Number(p.cache_read) || 0,
    cost: Number(p.cost_usd) || 0,
  }))
  const tokMax = Math.max(
    1,
    ...rows0.map((r) => r.input + r.output + r.cacheCreation + r.cacheRead),
  )
  const costMax = Math.max(0.0001, ...rows0.map((r) => r.cost))
  const yTok = tokMax * 1.08
  const yCostMax = costMax * 1.08
  const xAt = (i: number) => (n === 1 ? padL + innerW / 2 : padL + (i / (n - 1)) * innerW)
  const yTokAt = (v: number) => padT + innerH - (v / yTok) * innerH
  const yCostAt = (v: number) => padT + innerH - (v / yCostMax) * innerH
  const yBase = padT + innerH
  const rows: Row[] = rows0.map((r, i) => {
    const inTop = r.input
    const outTop = inTop + r.output
    const ccTop = outTop + r.cacheCreation
    const crTop = ccTop + r.cacheRead
    return {
      ...r,
      x: xAt(i),
      yInBot: yBase,
      yInTop: yTokAt(inTop),
      yOutBot: yTokAt(inTop),
      yOutTop: yTokAt(outTop),
      yCcBot: yTokAt(outTop),
      yCcTop: yTokAt(ccTop),
      yCrBot: yTokAt(ccTop),
      yCrTop: yTokAt(crTop),
      yCost: yCostAt(r.cost),
    }
  })
  const line = (ys: (r: Row) => number) =>
    rows.map((r, i) => `${i ? 'L' : 'M'}${r.x},${ys(r)}`).join(' ')
  const gridN = 4
  const grid = Array.from({ length: gridN + 1 }, (_, i) => {
    const t = i / gridN
    const y = padT + innerH * (1 - t)
    return {
      y,
      tok: yTok * t,
      cost: yCostMax * t,
    }
  })
  const tickIdx = ticks(n)
  const span = new Date(pts[n - 1].t).getTime() - new Date(pts[0].t).getTime()
  return {
    rows,
    yBase,
    yTok,
    yCostMax,
    innerH,
    grid,
    tickIdx,
    span,
    areaIn: bandPath(rows, (r) => r.yInTop, (r) => r.yInBot),
    areaOut: bandPath(rows, (r) => r.yOutTop, (r) => r.yOutBot),
    areaCc: bandPath(rows, (r) => r.yCcTop, (r) => r.yCcBot),
    areaCr: bandPath(rows, (r) => r.yCrTop, (r) => r.yCrBot),
    lineIn: line((r) => r.yInTop),
    lineOut: line((r) => r.yOutTop),
    lineCc: line((r) => r.yCcTop),
    lineCr: line((r) => r.yCrTop),
    lineCost: line((r) => r.yCost),
  }
})

function ticks(n: number): number[] {
  if (n <= 1) return [0]
  const count = Math.min(6, n)
  const out: number[] = []
  for (let i = 0; i < count; i++) out.push(Math.round((i * (n - 1)) / (count - 1)))
  return [...new Set(out)]
}

function fmtTick(iso: string, span: number): string {
  const d = new Date(iso)
  if (span <= 36 * 3600 * 1000) {
    return d.toLocaleString(undefined, { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })
  }
  return d.toLocaleDateString(undefined, { month: '2-digit', day: '2-digit' })
}

const hovered = computed(() => {
  const v = view.value
  if (!v || hover.value < 0) return null
  return v.rows[hover.value] ?? null
})

function onMove(ev: MouseEvent) {
  const v = view.value
  const svg = ev.currentTarget as SVGSVGElement
  if (!v || !svg) return
  const rect = svg.getBoundingClientRect()
  const x = ((ev.clientX - rect.left) / rect.width) * W
  let best = 0
  let bestD = Infinity
  for (let i = 0; i < v.rows.length; i++) {
    const d = Math.abs(v.rows[i].x - x)
    if (d < bestD) {
      bestD = d
      best = i
    }
  }
  hover.value = best
  const row = v.rows[best]
  const localX = (row.x / W) * rect.width
  tipPos.value = {
    x: Math.min(rect.width - 220, Math.max(8, localX + 12)),
    y: 28,
  }
}
</script>

<template>
  <div class="card trend-card">
    <div class="trend-head">
      <h2>每日趋势</h2>
      <p v-if="rangeLabel" class="range-label">{{ rangeLabel }}</p>
    </div>
    <div v-if="view" class="trend-body">
      <svg
        class="chart"
        :viewBox="`0 0 ${W} ${H}`"
        role="img"
        aria-label="用量趋势"
        @mousemove="onMove"
        @mouseleave="hover = -1"
      >
        <defs>
          <linearGradient id="trendIn" x1="0" y1="0" x2="0" y2="1">
            <stop offset="5%" stop-color="#3b82f6" stop-opacity="0.45" />
            <stop offset="95%" stop-color="#3b82f6" stop-opacity="0.12" />
          </linearGradient>
          <linearGradient id="trendOut" x1="0" y1="0" x2="0" y2="1">
            <stop offset="5%" stop-color="#22c55e" stop-opacity="0.45" />
            <stop offset="95%" stop-color="#22c55e" stop-opacity="0.12" />
          </linearGradient>
          <linearGradient id="trendCc" x1="0" y1="0" x2="0" y2="1">
            <stop offset="5%" stop-color="#f97316" stop-opacity="0.45" />
            <stop offset="95%" stop-color="#f97316" stop-opacity="0.12" />
          </linearGradient>
          <linearGradient id="trendCr" x1="0" y1="0" x2="0" y2="1">
            <stop offset="5%" stop-color="#a855f7" stop-opacity="0.4" />
            <stop offset="95%" stop-color="#a855f7" stop-opacity="0.1" />
          </linearGradient>
        </defs>
        <line
          v-for="(g, i) in view.grid"
          :key="'g' + i"
          :x1="padL"
          :x2="W - padR"
          :y1="g.y"
          :y2="g.y"
          class="grid-line"
        />
        <text
          v-for="(g, i) in view.grid"
          :key="'yt' + i"
          class="axis"
          :x="padL - 8"
          :y="g.y + 4"
          text-anchor="end"
        >
          {{ fmtAxisTokens(g.tok) }}
        </text>
        <text
          v-for="(g, i) in view.grid"
          :key="'yc' + i"
          class="axis"
          :x="W - padR + 8"
          :y="g.y + 4"
          text-anchor="start"
        >
          {{ fmtUsd(g.cost) }}
        </text>
        <path :d="view.areaIn" :fill="SERIES[0].fill" />
        <path :d="view.areaOut" :fill="SERIES[1].fill" />
        <path :d="view.areaCc" :fill="SERIES[2].fill" />
        <path :d="view.areaCr" :fill="SERIES[3].fill" />
        <path :d="view.lineIn" fill="none" :stroke="COL.input" stroke-width="2" />
        <path :d="view.lineOut" fill="none" :stroke="COL.output" stroke-width="2" />
        <path :d="view.lineCc" fill="none" :stroke="COL.cacheCreation" stroke-width="2" />
        <path :d="view.lineCr" fill="none" :stroke="COL.cacheRead" stroke-width="2" />
        <path
          :d="view.lineCost"
          fill="none"
          :stroke="COL.cost"
          stroke-width="2"
          stroke-dasharray="4 4"
        />
        <line
          v-if="hovered"
          :x1="hovered.x"
          :x2="hovered.x"
          :y1="padT"
          :y2="view.yBase"
          class="hover-line"
        />
        <circle v-if="hovered" :cx="hovered.x" :cy="hovered.yInTop" r="3.2" :fill="COL.input" />
        <circle v-if="hovered" :cx="hovered.x" :cy="hovered.yOutTop" r="3.2" :fill="COL.output" />
        <circle v-if="hovered" :cx="hovered.x" :cy="hovered.yCcTop" r="3.2" :fill="COL.cacheCreation" />
        <circle v-if="hovered" :cx="hovered.x" :cy="hovered.yCrTop" r="3.2" :fill="COL.cacheRead" />
        <circle v-if="hovered" :cx="hovered.x" :cy="hovered.yCost" r="3.2" :fill="COL.cost" />
        <text
          v-for="i in view.tickIdx"
          :key="'t' + i"
          class="axis"
          :x="view.rows[i].x"
          :y="H - 10"
          text-anchor="middle"
        >
          {{ fmtTick(view.rows[i].t, view.span) }}
        </text>
      </svg>
      <div v-if="hovered" class="tip" :style="{ left: tipPos.x + 'px', top: tipPos.y + 'px' }">
        <p class="tip-h">{{ fmtTime(hovered.t) }}</p>
        <div class="tip-row" :style="{ color: COL.input }">
          <i :style="{ background: COL.input }" />输入 Tokens: {{ fmtInt(hovered.input) }}
        </div>
        <div class="tip-row" :style="{ color: COL.output }">
          <i :style="{ background: COL.output }" />输出 Tokens: {{ fmtInt(hovered.output) }}
        </div>
        <div class="tip-row" :style="{ color: COL.cacheCreation }">
          <i :style="{ background: COL.cacheCreation }" />缓存创建: {{ fmtInt(hovered.cacheCreation) }}
        </div>
        <div class="tip-row" :style="{ color: COL.cacheRead }">
          <i :style="{ background: COL.cacheRead }" />缓存命中: {{ fmtInt(hovered.cacheRead) }}
        </div>
        <div class="tip-row" :style="{ color: COL.cost }">
          <i :style="{ background: COL.cost }" />成本: {{ fmtUsd(hovered.cost) }}
        </div>
      </div>
    </div>
    <div v-else class="empty trend-empty">窗口内没有趋势数据</div>
    <div v-if="view" class="legend-row">
      <span v-for="s in SERIES" :key="s.key"><i :style="{ background: s.color }" />{{ s.name }}</span>
      <span><i class="dash" :style="{ background: COL.cost }" />成本</span>
    </div>
  </div>
</template>

<style scoped>
.trend-card {
  padding: 14px 16px;
  min-width: 0;
}
.trend-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
}
.trend-head h2 {
  margin: 0;
  font-size: 15px;
  color: var(--text);
  letter-spacing: 0.02em;
  text-transform: none;
  font-weight: 600;
}
.range-label {
  margin: 0;
  font-size: 13px;
  color: var(--muted);
}
.trend-body {
  position: relative;
}
.chart {
  width: 100%;
  height: 200px;
  display: block;
}
.axis {
  fill: var(--muted);
  font-size: 11px;
}
.grid-line {
  stroke: var(--line);
  stroke-width: 1;
  stroke-dasharray: 3 3;
  opacity: 0.55;
}
.hover-line {
  stroke: var(--text);
  stroke-width: 1;
  opacity: 0.25;
}
.legend-row {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
  margin-top: 10px;
  font-size: 12px;
  color: var(--muted);
}
.legend-row i {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 99px;
  margin-right: 6px;
}
.legend-row i.dash {
  border-radius: 1px;
  height: 3px;
  width: 12px;
  vertical-align: middle;
}
.trend-empty {
  min-height: 160px;
  display: flex;
  align-items: center;
  justify-content: center;
}
.tip {
  position: absolute;
  z-index: 2;
  min-width: 188px;
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
  margin: 0 0 8px;
  font-weight: 600;
  color: var(--text);
}
.tip-row {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 3px;
  font-variant-numeric: tabular-nums;
}
.tip-row i {
  width: 8px;
  height: 8px;
  border-radius: 99px;
  flex: 0 0 8px;
}
</style>
