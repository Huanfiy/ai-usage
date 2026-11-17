export type ChartPoint = { x: number; y: number }

/** Unclamped positions let SVG clip zoomed curves without folding them onto the axes. */
export function timeX(time: number, from: number, to: number, left: number, width: number, clamp = true): number {
  if (to <= from) return left + width / 2
  const ratio = (time - from) / (to - from)
  return left + (clamp ? Math.max(0, Math.min(1, ratio)) : ratio) * width
}

export type TimeWindow = { from: number; to: number }

export function fitTimeWindow(window: TimeWindow, bounds: TimeWindow, minSpan = 0): TimeWindow {
  const total = bounds.to - bounds.from
  if (total <= 0) return { ...bounds }
  const span = Math.min(total, Math.max(minSpan, window.to - window.from))
  const from = Math.max(bounds.from, Math.min(bounds.to - span, window.from))
  return { from, to: from + span }
}

/** Keep the time under the pointer fixed, except when reaching the range edges. */
export function zoomTimeWindow(window: TimeWindow, bounds: TimeWindow, ratio: number, factor: number, minSpan: number): TimeWindow {
  const current = fitTimeWindow(window, bounds, minSpan)
  if (!Number.isFinite(factor) || factor <= 0) return current
  const span = current.to - current.from
  const nextSpan = Math.min(bounds.to - bounds.from, Math.max(minSpan, span * factor))
  const anchor = Math.max(0, Math.min(1, ratio))
  const from = current.from + (span - nextSpan) * anchor
  return fitTimeWindow({ from, to: from + nextSpan }, bounds, minSpan)
}

export function panTimeWindow(window: TimeWindow, bounds: TimeWindow, offset: number): TimeWindow {
  return fitTimeWindow({ from: window.from + offset, to: window.to + offset }, bounds)
}

/** Normalize mouse wheels and trackpads; modifiers/native page zoom are handled by the caller. */
export function wheelZoomFactor(deltaY: number, deltaMode: number, pageHeight: number): number {
  const pixels = deltaY * (deltaMode === 1 ? 16 : deltaMode === 2 ? pageHeight : 1)
  return Math.exp(Math.max(-240, Math.min(240, pixels)) * 0.0025)
}

/** Horizontal Bezier handles avoid overshoot; smoothing never changes data points. */
export function smoothLine(points: ChartPoint[]): string {
  if (!points.length) return ''
  let path = `M${points[0].x},${points[0].y}`
  for (let i = 1; i < points.length; i++) {
    const a = points[i - 1]
    const b = points[i]
    const mid = (a.x + b.x) / 2
    path += ` C${mid},${a.y} ${mid},${b.y} ${b.x},${b.y}`
  }
  return path
}

/** Value on smoothLine's monotone Bezier segment, including its nonlinear X handles. */
function smoothValueAt(a: ChartPoint, b: ChartPoint, x: number): number {
  const ratio = (x - a.x) / (b.x - a.x)
  let lo = 0
  let hi = 1
  for (let i = 0; i < 40; i++) {
    const t = (lo + hi) / 2
    const curveX = 1.5 * t - 1.5 * t * t + t * t * t
    if (curveX < ratio) lo = t
    else hi = t
  }
  const t = (lo + hi) / 2
  const blend = t * t * (3 - 2 * t)
  return a.y + (b.y - a.y) * blend
}

/**
 * Maximum actually visible on a sorted smoothLine, not its offscreen peaks.
 * Include curve intersections at the viewport edges so rescaling cannot clip
 * a segment entering the plot, even when no original point is inside it.
 */
export function visibleCurveMax(points: ChartPoint[], from: number, to: number): number {
  if (to < from) return 0
  let max = 0
  for (let i = 0; i < points.length; i++) {
    const b = points[i]
    if (b.x >= from && b.x <= to) max = Math.max(max, b.y)
    if (i === 0) continue
    const a = points[i - 1]
    for (const edge of [from, to]) {
      if (a.x < edge && b.x > edge) max = Math.max(max, smoothValueAt(a, b, edge))
    }
  }
  return max
}

type ChartSize = { width: number; height: number }

/** Prefer a position away from the pointer, flip at edges, and stay inside the chart. */
export function tooltipPosition(pointer: ChartPoint, bounds: ChartSize, size: ChartSize): ChartPoint {
  const gap = 14
  const pad = 8
  const x = pointer.x + gap + size.width <= bounds.width - pad
    ? pointer.x + gap : pointer.x - gap - size.width
  const y = pointer.y - gap - size.height >= pad
    ? pointer.y - gap - size.height : pointer.y + gap
  return {
    x: Math.max(pad, Math.min(Math.max(pad, bounds.width - size.width - pad), x)),
    y: Math.max(pad, Math.min(Math.max(pad, bounds.height - size.height - pad), y)),
  }
}

export function intervalLabel(seconds: number): string {
  if (seconds < 3600) return `每 ${seconds / 60} 分钟`
  if (seconds < 86400) return `每 ${seconds / 3600} 小时`
  return `每 ${seconds / 86400} 天`
}

export function binWindow(time: number, seconds: number, from: number, to: number) {
  return { from: Math.max(time, from), to: Math.min(time + seconds * 1000, to) }
}
