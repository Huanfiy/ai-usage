<script setup lang="ts">
import { computed, ref, useId, watch } from 'vue'
import type { UsageSeries } from '../api'
import { fmtAxisTokens, fmtInt, fmtTime, fmtTimeTick, fmtUsd } from '../format'
import {
  binWindow, fitTimeWindow, intervalLabel, panTimeWindow, smoothLine, tooltipPosition,
  timeX, visibleCurveMax, wheelZoomFactor, zoomTimeWindow, type ChartPoint, type TimeWindow,
} from '../trend'

const props = defineProps<{ series: UsageSeries | null; rangeLabel?: string; resetKey?: string }>()

const COL = { tokens: '#3ee0b3', cost: '#f43f5e' }
const chartId = `trend-${useId()}`
const svgRef = ref<SVGSVGElement | null>(null)
const bodyRef = ref<HTMLDivElement | null>(null)
const tooltipRef = ref<HTMLDivElement | null>(null)
const bodySize = ref({ width: 0, height: 0 })
const tooltipSize = ref({ width: 248, height: 88 })
const hoverTime = ref<number | null>(null)
const pointer = ref<ChartPoint | null>(null)
const zoomWindow = ref<TimeWindow | null>(null)
const drag = ref<{ pointerId: number; x: number; window: TimeWindow } | null>(null)
const resolution = computed(() => props.series ? intervalLabel(props.series.interval_seconds) : '')

// Match the SVG coordinate system to its actual size, avoiding letterboxed space.
const W = computed(() => bodySize.value.width || 860)
const H = computed(() => bodySize.value.height || 220)
const padL = 48
const padR = 16
const padT = 16
const padB = 30
const innerW = computed(() => Math.max(1, W.value - padL - padR))
const innerH = computed(() => Math.max(1, H.value - padT - padB))
const yBase = computed(() => H.value - padB)

const fullRange = computed<TimeWindow | null>(() => {
  if (!props.series?.points.length) return null
  const from = new Date(props.series.from).getTime()
  const to = new Date(props.series.to).getTime()
  return Number.isFinite(from) && Number.isFinite(to) && to > from ? { from, to } : null
})
// Keep at least four real bins on screen; zoom does not invent finer samples.
const minSpan = computed(() => (props.series?.interval_seconds ?? 1800) * 1000 * 4)
const viewport = computed(() => fullRange.value && (zoomWindow.value ?? fullRange.value))
const isZoomed = computed(() => zoomWindow.value !== null)

const view = computed(() => {
  const series = props.series
  const full = fullRange.value
  const win = viewport.value
  if (!series || !full || !win) return null
  const rows0 = series.points.map((p) => ({
    t: p.t,
    time: new Date(p.t).getTime(),
    // Use the backend total, including cache and reasoning, just like the KPI.
    tokens: Number(p.tokens) || 0,
    cost: Number(p.cost_usd) || 0,
  }))
  // Fit the Token axis to the visible curve, including intersections at zoom edges.
  const tokenMax = visibleCurveMax(rows0.map((r) => ({ x: Math.max(r.time, full.from), y: r.tokens })), win.from, win.to)
  const yTok = Math.max(1, tokenMax) * 1.08
  const rows = rows0.map((r) => ({
    ...r,
    // Clip the original partial bin to the query, but not to the zoom viewport.
    x: timeX(Math.max(r.time, full.from), win.from, win.to, padL, innerW.value, false),
    yTokens: yBase.value - r.tokens / yTok * innerH.value,
  }))
  const grid = Array.from({ length: 5 }, (_, i) => ({
    y: yBase.value - innerH.value * i / 4, tok: yTok * i / 4,
  }))
  const span = win.to - win.from
  const tickSpacing = span <= 36 * 3600 * 1000 ? 100 : 64
  const tickCount = Math.max(2, Math.min(6, Math.floor(innerW.value / tickSpacing) + 1, Math.ceil(span / (series.interval_seconds * 1000)) + 1))
  const axisTicks = Array.from({ length: tickCount }, (_, i) => {
    const t = win.from + i / (tickCount - 1) * span
    return { t: new Date(t).toISOString(), x: timeX(t, win.from, win.to, padL, innerW.value) }
  })
  return {
    rows, grid, axisTicks, span,
    visibleRows: rows.filter((r) => r.x >= padL && r.x <= W.value - padR),
    lineTokens: smoothLine(rows.map((r) => ({ x: r.x, y: r.yTokens }))),
  }
})

const hovered = computed(() => view.value?.visibleRows.find((r) => r.time === hoverTime.value) ?? null)
const marker = computed(() => hovered.value ?? (view.value?.rows.length === 1 ? view.value.rows[0] : null))
const hoverWindow = computed(() => {
  const full = fullRange.value
  if (!hovered.value || !props.series || !full) return null
  // Values always describe their actual bin, not a fraction created by zooming.
  const win = binWindow(hovered.value.time, props.series.interval_seconds, full.from, full.to)
  return {
    label: `${fmtTime(new Date(win.from).toISOString())} – ${fmtTimeTick(new Date(win.to).toISOString(), true)}`,
  }
})

watch([bodyRef, tooltipRef], ([body, tip], _, onCleanup) => {
  if (!body) return
  const measure = () => {
    const { width, height } = body.getBoundingClientRect()
    if (width !== bodySize.value.width || height !== bodySize.value.height) {
      bodySize.value = { width, height }
      stopDrag()
      clearHover()
    }
    if (tip) tooltipSize.value = { width: tip.offsetWidth, height: tip.offsetHeight }
  }
  const observer = new ResizeObserver(measure)
  observer.observe(body)
  if (tip) observer.observe(tip)
  measure()
  onCleanup(() => observer.disconnect())
}, { flush: 'post' })

const tooltipOffset = computed(() => {
  const matrix = svgRef.value?.getScreenCTM()
  const body = bodyRef.value
  if (!pointer.value || !matrix || !body) return { x: 8, y: 8 }
  const p = new DOMPoint(pointer.value.x, pointer.value.y).matrixTransform(matrix)
  const rect = body.getBoundingClientRect()
  return tooltipPosition({ x: p.x - rect.left, y: p.y - rect.top }, bodySize.value, tooltipSize.value)
})

function clearHover() {
  hoverTime.value = null
  pointer.value = null
}

function applyWindow(window: TimeWindow) {
  const full = fullRange.value
  if (!full) return
  const next = fitTimeWindow(window, full, minSpan.value)
  zoomWindow.value = Math.abs(next.from - full.from) < 1 && Math.abs(next.to - full.to) < 1 ? null : next
  clearHover()
}

function stopDrag() {
  const id = drag.value?.pointerId
  drag.value = null
  if (id !== undefined && svgRef.value?.hasPointerCapture(id)) svgRef.value.releasePointerCapture(id)
}

function resetView() {
  stopDrag()
  zoomWindow.value = null
  clearHover()
}

// A range/filter change resets the view; polling only constrains it to new bounds.
watch(() => [props.resetKey, props.rangeLabel], resetView)
watch(() => props.series, () => {
  clearHover()
  if (!fullRange.value) resetView()
  else if (zoomWindow.value) applyWindow(zoomWindow.value)
})

function plotPoint(ev: MouseEvent): ChartPoint | null {
  const matrix = svgRef.value?.getScreenCTM()
  return matrix ? new DOMPoint(ev.clientX, ev.clientY).matrixTransform(matrix.inverse()) : null
}

function inPlot(p: ChartPoint | null): p is ChartPoint {
  return !!p && p.x >= padL && p.x <= W.value - padR && p.y >= padT && p.y <= yBase.value
}

function pickHover(p: ChartPoint | null) {
  clearHover()
  if (!inPlot(p) || !view.value) return
  pointer.value = p
  let best = Infinity
  for (const row of view.value.visibleRows) {
    const distance = Math.abs(row.x - p.x)
    if (distance < best) {
      best = distance
      hoverTime.value = row.time
    }
  }
}

function onMove(ev: PointerEvent) {
  const p = plotPoint(ev)
  const full = fullRange.value
  if (drag.value && p && full && ev.pointerId === drag.value.pointerId) {
    const { x, window } = drag.value
    applyWindow(panTimeWindow(window, full, (x - p.x) / innerW.value * (window.to - window.from)))
  } else if (!drag.value) pickHover(p)
}

function onWheel(ev: WheelEvent) {
  const p = plotPoint(ev)
  const full = fullRange.value
  const win = viewport.value
  // Outside the plot, native page scrolling/zooming must continue to work.
  if (!inPlot(p) || !full || !win || drag.value || ev.ctrlKey || ev.metaKey || !ev.deltaY || !ev.cancelable) return
  if (full.to - full.from <= minSpan.value) return
  ev.preventDefault()
  const height = svgRef.value?.getBoundingClientRect().height ?? H.value
  applyWindow(zoomTimeWindow(win, full, (p.x - padL) / innerW.value, wheelZoomFactor(ev.deltaY, ev.deltaMode, height), minSpan.value))
  pickHover(p)
}

function onDown(ev: PointerEvent) {
  const p = plotPoint(ev)
  if (ev.button !== 0 || !ev.isPrimary || !isZoomed.value || !inPlot(p) || !viewport.value) return
  ev.preventDefault()
  svgRef.value?.focus({ preventScroll: true })
  svgRef.value?.setPointerCapture(ev.pointerId)
  drag.value = { pointerId: ev.pointerId, x: p.x, window: { ...viewport.value } }
  clearHover()
}

function onEnd(ev: PointerEvent) {
  if (drag.value?.pointerId !== ev.pointerId) return
  stopDrag()
  if (ev.type === 'pointerup') pickHover(plotPoint(ev))
}

function onDoubleClick(ev: MouseEvent) {
  if (inPlot(plotPoint(ev))) resetView()
}

function onKey(ev: KeyboardEvent) {
  const full = fullRange.value
  const win = viewport.value
  if (!full || !win || ev.ctrlKey || ev.metaKey || ev.altKey) return
  if (ev.key === '+' || ev.key === '=' || ev.key === '-') {
    ev.preventDefault()
    applyWindow(zoomTimeWindow(win, full, 0.5, ev.key === '-' ? 1.25 : 0.8, minSpan.value))
  } else if (isZoomed.value && (ev.key === 'ArrowLeft' || ev.key === 'ArrowRight')) {
    ev.preventDefault()
    applyWindow(panTimeWindow(win, full, (win.to - win.from) * (ev.key === 'ArrowLeft' ? -0.2 : 0.2)))
  } else if (ev.key === 'Home' || ev.key === 'Escape') {
    ev.preventDefault()
    resetView()
  }
}
</script>

<template>
  <div class="card trend-card">
    <div class="trend-head">
      <div class="trend-heading">
        <h2>用量趋势</h2>
        <div v-if="view" class="trend-legend">
          <span><i :style="{ borderColor: COL.tokens }" />总 Tokens</span>
        </div>
      </div>
      <div class="trend-actions">
        <p class="range-label">{{ [rangeLabel, resolution].filter(Boolean).join(' · ') }}</p>
        <button class="reset-zoom" type="button" :disabled="!isZoomed" @click="resetView">重置</button>
      </div>
    </div>
    <div v-if="view" ref="bodyRef" class="trend-body">
      <svg
        ref="svgRef"
        class="chart"
        :class="{ zoomed: isZoomed, dragging: drag !== null }"
        :viewBox="`0 0 ${W} ${H}`"
        role="img"
        aria-label="用量趋势；滚轮缩放，放大后拖动平移；键盘加减号缩放、左右键平移、Home 重置"
        tabindex="0"
        @pointermove="onMove"
        @pointerleave="!drag && clearHover()"
        @pointerdown="onDown"
        @pointerup="onEnd"
        @pointercancel="onEnd"
        @lostpointercapture="drag = null"
        @wheel="onWheel"
        @dblclick="onDoubleClick"
        @keydown="onKey"
      >
        <defs>
          <clipPath :id="`${chartId}-plot`">
            <rect :x="padL" :y="padT" :width="innerW" :height="innerH" />
          </clipPath>
        </defs>
        <line v-for="(g, i) in view.grid" :key="'g' + i" :x1="padL" :x2="W - padR" :y1="g.y" :y2="g.y" class="grid-line" />
        <text v-for="(g, i) in view.grid" :key="'yt' + i" class="axis" :x="padL - 8" :y="g.y + 4" text-anchor="end">
          {{ fmtAxisTokens(g.tok) }}
        </text>
        <g :clip-path="`url(#${chartId}-plot)`">
          <path :d="view.lineTokens" fill="none" :stroke="COL.tokens" stroke-width="2" />
          <line v-if="hovered" :x1="hovered.x" :x2="hovered.x" :y1="padT" :y2="yBase" class="hover-line" />
          <g v-if="marker">
            <circle :cx="marker.x" :cy="marker.yTokens" r="3.2" :fill="COL.tokens" />
          </g>
        </g>
        <text v-for="(tick, i) in view.axisTicks" :key="'t' + i" class="axis time-tick" :x="tick.x" :y="H - 10" :text-anchor="i === view.axisTicks.length - 1 ? 'end' : 'middle'">
          {{ fmtTimeTick(tick.t, view.span <= 36 * 3600 * 1000) }}
        </text>
      </svg>
      <div
        v-if="hovered"
        ref="tooltipRef"
        class="trend-tooltip"
        role="tooltip"
        :style="{ left: tooltipOffset.x + 'px', top: tooltipOffset.y + 'px' }"
      >
        <p class="tooltip-time"><time :datetime="hovered.t">{{ hoverWindow?.label }}</time></p>
        <div class="tooltip-row" :style="{ color: COL.tokens }">
          <span>Tokens</span><strong>{{ fmtInt(hovered.tokens) }}</strong>
        </div>
        <div class="tooltip-row" :style="{ color: COL.cost }">
          <span>成本</span><strong>{{ fmtUsd(hovered.cost) }}</strong>
        </div>
      </div>
    </div>
    <div v-else class="empty trend-empty">窗口内没有趋势数据</div>
  </div>
</template>

<style scoped>
.trend-card {
  padding: 14px 16px;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.trend-head {
  display: flex;
  flex-shrink: 0;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: 8px 12px;
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
.trend-heading,
.trend-legend {
  display: flex;
  align-items: center;
  gap: 14px;
}
.trend-legend {
  gap: 12px;
  color: var(--muted);
  font-size: 11px;
}
.trend-legend span {
  display: inline-flex;
  align-items: center;
  gap: 5px;
}
.trend-legend i {
  width: 12px;
  border-top: 2px solid;
}
.trend-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-left: auto;
}
.range-label {
  margin: 0;
  font-size: 12px;
  color: var(--muted);
}
.reset-zoom {
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--bg-elev-2);
  padding: 3px 7px;
  font-size: 11px;
}
.reset-zoom:disabled { opacity: 0.4; }
.reset-zoom:hover:not(:disabled) { border-color: var(--mint); }
.chart {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  display: block;
  touch-action: pan-y pinch-zoom;
  user-select: none;
}
.chart.zoomed { cursor: grab; }
.chart.dragging { cursor: grabbing; }
.chart:focus-visible { outline: 1px solid var(--mint); border-radius: 6px; }
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
.trend-body {
  position: relative;
  flex: 1;
  min-height: 220px;
}
.trend-tooltip {
  position: absolute;
  z-index: 2;
  width: 248px;
  max-width: calc(100% - 16px);
  padding: 9px 11px;
  border: 1px solid rgba(139, 151, 168, 0.3);
  border-radius: 8px;
  background: rgba(11, 14, 18, 0.62);
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.15);
  pointer-events: none;
  font-size: 12px;
}
.tooltip-time {
  margin: 0 0 6px;
  font-size: 11px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.tooltip-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-top: 4px;
  font-variant-numeric: tabular-nums;
}
.tooltip-row strong { font-weight: 600; }
.trend-empty {
  min-height: 160px;
  display: flex;
  align-items: center;
  justify-content: center;
}
</style>
