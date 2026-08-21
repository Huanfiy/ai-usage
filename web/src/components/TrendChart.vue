<script setup lang="ts">
import { computed, ref } from 'vue'
import type { SeriesPoint } from '../api'
import { fmtMetric, fmtTime, fmtTokens, type Metric } from '../format'

const props = defineProps<{ points: SeriesPoint[] }>()

const metric = ref<Metric>('tokens')
const hover = ref(-1)

const COL = {
  output: '#3ee0b3',
  input: '#5b8def',
  cache: '#c9a35a',
  cost: '#e8b15a',
}

type Seg = { y: number; h: number; color: string }
type Bar = {
  i: number
  t: string
  x: number
  w: number
  output: number
  input: number
  cache: number
  cost: number
  segs: Seg[]
}

const view = computed(() => {
  const w = 640
  const h = 220
  const padL = 44
  const padR = 12
  const padT = 12
  const padB = 28
  const pts = props.points
  const n = pts.length
  if (!n) return null
  const innerW = w - padL - padR
  const innerH = h - padT - padB
  const rows = pts.map((p) => ({
    t: p.t,
    output: Number(p.output) || 0,
    input: Number(p.input) || 0,
    cache: (Number(p.cache_read) || 0) + (Number(p.cache_creation) || 0),
    cost: Number(p.cost_usd) || 0,
  }))
  const totals = rows.map((r) => (metric.value === 'tokens' ? r.output + r.input + r.cache : r.cost))
  const max = Math.max(...totals, 0)
  const yMax = max > 0 ? max : 1
  const gap = n > 48 ? 1 : n > 24 ? 2 : 3
  let barW = (innerW - gap * (n - 1)) / n
  let x0 = padL
  if (barW > 42) {
    barW = 42
    const total = n * barW + (n - 1) * gap
    x0 = padL + Math.max(0, (innerW - total) / 2)
  }
  const bars: Bar[] = rows.map((r, i) => {
    const x = x0 + i * (barW + gap)
    const segs: Seg[] = []
    if (metric.value === 'tokens') {
      let acc = 0
      for (const s of [
        { v: r.output, color: COL.output },
        { v: r.input, color: COL.input },
        { v: r.cache, color: COL.cache },
      ]) {
        const hSeg = (s.v / yMax) * innerH
        const y = padT + innerH - acc - hSeg
        if (hSeg > 0.4) segs.push({ y, h: hSeg, color: s.color })
        acc += hSeg
      }
    } else {
      const hSeg = (r.cost / yMax) * innerH
      if (hSeg > 0.4) segs.push({ y: padT + innerH - hSeg, h: hSeg, color: COL.cost })
    }
    return { i, t: r.t, x, w: barW, ...r, segs }
  })
  const tickIdx = [...new Set([0, Math.floor((n - 1) / 2), n - 1])]
  const span = new Date(pts[n - 1].t).getTime() - new Date(pts[0].t).getTime()
  return { w, h, padL, padR, padT, padB, innerH, bars, yMax, tickIdx, span }
})

function fmtTick(iso: string, span: number): string {
  const d = new Date(iso)
  if (span <= 36 * 3600 * 1000) {
    return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  }
  return d.toLocaleDateString(undefined, { month: 'numeric', day: 'numeric' })
}

const hovered = computed(() => {
  const v = view.value
  if (!v || hover.value < 0) return null
  return v.bars[hover.value] ?? null
})
</script>

<template>
  <div class="card">
    <div class="card-head">
      <h2>每日趋势</h2>
      <div class="tabs">
        <button :class="{ active: metric === 'tokens' }" @click="metric = 'tokens'">Token</button>
        <button :class="{ active: metric === 'cost' }" @click="metric = 'cost'">费用</button>
      </div>
    </div>
    <svg
      v-if="view"
      class="chart"
      :viewBox="`0 0 ${view.w} ${view.h}`"
      @mouseleave="hover = -1"
    >
      <line
        :x1="view.padL"
        :x2="view.w - view.padR"
        :y1="view.padT + view.innerH"
        :y2="view.padT + view.innerH"
        class="axis-line"
      />
      <text class="axis" :x="view.padL - 6" :y="view.padT + 4" text-anchor="end">
        {{ fmtMetric(metric, view.yMax) }}
      </text>
      <text class="axis" :x="view.padL - 6" :y="view.padT + view.innerH" text-anchor="end">0</text>
      <g v-for="b in view.bars" :key="b.t">
        <rect
          class="hit"
          :x="b.x"
          :y="view.padT"
          :width="Math.max(b.w, 2)"
          :height="view.innerH"
          @mouseenter="hover = b.i"
        />
        <rect
          v-for="(s, si) in b.segs"
          :key="si"
          :x="b.x"
          :y="s.y"
          :width="b.w"
          :height="s.h"
          :fill="s.color"
          rx="1"
        />
      </g>
      <text
        v-for="i in view.tickIdx"
        :key="'t' + i"
        class="axis"
        :x="view.bars[i].x + view.bars[i].w / 2"
        :y="view.h - 8"
        text-anchor="middle"
      >
        {{ fmtTick(view.bars[i].t, view.span) }}
      </text>
    </svg>
    <div v-else class="empty">窗口内没有 bucket</div>
    <div v-if="metric === 'tokens' && view" class="legend-row">
      <span><i :style="{ background: COL.output }" />输出</span>
      <span><i :style="{ background: COL.input }" />输入</span>
      <span><i :style="{ background: COL.cache }" />缓存</span>
    </div>
    <p v-if="hovered" class="chart-hint">
      {{ fmtTime(hovered.t) }}
      <template v-if="metric === 'tokens'">
        · 出 {{ fmtTokens(hovered.output) }} · 入 {{ fmtTokens(hovered.input) }} · 缓存
        {{ fmtTokens(hovered.cache) }}
      </template>
      <template v-else>· {{ fmtMetric('cost', hovered.cost) }}</template>
    </p>
  </div>
</template>

<style scoped>
.chart {
  width: 100%;
  height: 220px;
}
.axis {
  fill: var(--muted);
  font-size: 10px;
}
.axis-line {
  stroke: var(--line);
  stroke-width: 1;
}
.hit {
  fill: transparent;
  cursor: crosshair;
}
.legend-row {
  display: flex;
  gap: 14px;
  margin-top: 8px;
  font-size: 11px;
  color: var(--muted);
}
.legend-row i {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 2px;
  margin-right: 6px;
}
.chart-hint {
  margin: 8px 0 0;
  font-size: 11px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
</style>
