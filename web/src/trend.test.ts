import assert from 'node:assert/strict'
import { describe, it } from 'node:test'
import {
  binWindow, fitTimeWindow, intervalLabel, panTimeWindow, smoothLine, tooltipPosition,
  timeX, visibleCurveMax, wheelZoomFactor, zoomTimeWindow,
} from './trend.ts'

describe('trend time axis', () => {
  it('uses elapsed time rather than equal spacing of sparse points', () => {
    const at = (hour: number) => Date.UTC(2026, 0, 15, hour)
    const x = (hour: number) => timeX(at(hour), at(0), at(24), 48, 720)
    assert.equal(x(0), 48)
    assert.equal(x(10), 348)
    assert.equal(x(11) - x(10), 30)
    assert.equal(x(15) - x(11), 120)
    assert.equal(x(24), 768)
    assert.equal(x(-1), 48)
    assert.equal(x(25), 768)
    assert.equal(timeX(0, 0, 0, 48, 720), 408)
  })

  it('clips tooltip windows at both edges without changing the bin values', () => {
    assert.deepEqual(binWindow(0, 1800, 600_000, 1_200_000), { from: 600_000, to: 1_200_000 })
    assert.deepEqual(binWindow(1_800_000, 1800, 600_000, 4_000_000), { from: 1_800_000, to: 3_600_000 })
  })

  it('labels the actual resolution, not the selected range', () => {
    assert.equal(intervalLabel(1800), '每 30 分钟')
    assert.equal(intervalLabel(3600), '每 1 小时')
    assert.equal(intervalLabel(10800), '每 3 小时')
    assert.equal(intervalLabel(172800), '每 2 天')
  })
})

describe('trend viewport', () => {
  const hour = 3600_000
  const from = Date.UTC(2026, 0, 15)
  const bounds = { from, to: from + 24 * hour }
  const minSpan = 2 * hour

  it('zooms around the pointed time instead of the middle of the chart', () => {
    for (const ratio of [0, 0.1, 0.5, 0.9, 1]) {
      const next = zoomTimeWindow(bounds, bounds, ratio, 0.5, minSpan)
      assert.equal(next.to - next.from, 12 * hour)
      const anchorBefore = bounds.from + ratio * (bounds.to - bounds.from)
      const anchorAfter = next.from + ratio * (next.to - next.from)
      assert.ok(Math.abs(anchorBefore - anchorAfter) < 0.001)
    }
  })

  it('respects the smallest real-data window and returns exactly to full range', () => {
    let win = { ...bounds }
    for (let i = 0; i < 100; i++) win = zoomTimeWindow(win, bounds, 0.7, 0.5, minSpan)
    assert.equal(win.to - win.from, minSpan)
    assert.ok(win.from >= bounds.from && win.to <= bounds.to)
    const atLimit = zoomTimeWindow(win, bounds, 0.1, 0.5, minSpan)
    assert.deepEqual(atLimit, win)
    for (let i = 0; i < 100; i++) win = zoomTimeWindow(win, bounds, 0.2, 2, minSpan)
    assert.deepEqual(win, bounds)
  })

  it('clamps panning at both edges without changing the zoom level', () => {
    const win = { from: from + 4 * hour, to: from + 8 * hour }
    assert.deepEqual(panTimeWindow(win, bounds, hour), { from: from + 5 * hour, to: from + 9 * hour })
    assert.deepEqual(panTimeWindow(win, bounds, -100 * hour), { from, to: from + 4 * hour })
    assert.deepEqual(panTimeWindow(win, bounds, 100 * hour), { from: bounds.to - 4 * hour, to: bounds.to })
  })

  it('keeps the viewed dates on polling and constrains them when the bounds move past them', () => {
    const win = { from: from + 4 * hour, to: from + 8 * hour }
    assert.deepEqual(fitTimeWindow(win, { from: from + hour, to: bounds.to + hour }, minSpan), win)
    assert.deepEqual(fitTimeWindow(win, { from: from + 6 * hour, to: bounds.to + 6 * hour }, minSpan), {
      from: from + 6 * hour, to: from + 10 * hour,
    })
  })

  it('normalizes pixel, line and page wheel events and limits large deltas', () => {
    assert.equal(wheelZoomFactor(120, 0, 200), wheelZoomFactor(7.5, 1, 200))
    assert.equal(wheelZoomFactor(120, 0, 200), wheelZoomFactor(0.6, 2, 200))
    assert.ok(wheelZoomFactor(-120, 0, 200) < 1)
    assert.ok(wheelZoomFactor(120, 0, 200) > 1)
    assert.equal(wheelZoomFactor(0, 0, 200), 1)
    assert.equal(wheelZoomFactor(10000, 0, 200), wheelZoomFactor(240, 0, 200))
  })

  it('leaves tiny/empty windows and invalid zoom factors safe', () => {
    const tiny = { from, to: from + hour }
    assert.deepEqual(zoomTimeWindow(tiny, tiny, 0.5, 0.1, minSpan), tiny)
    assert.deepEqual(fitTimeWindow(tiny, { from, to: from }), { from, to: from })
    for (const factor of [0, -1, NaN, Infinity]) {
      assert.deepEqual(zoomTimeWindow(bounds, bounds, 0.5, factor, minSpan), bounds)
    }
  })

  it('keeps offscreen points outside the plot for clipping rather than folding curves onto an axis', () => {
    assert.equal(timeX(0, 10, 20, 48, 720, false), -672)
    assert.equal(timeX(30, 10, 20, 48, 720, false), 1488)
    assert.equal(timeX(15, 10, 20, 48, 720, false), 408)
  })
})

describe('trend adaptive Y axes', () => {
  const points = [
    { x: 0, y: 10_000 }, { x: 10, y: 100 }, { x: 20, y: 200 }, { x: 30, y: 100_000 },
  ]

  it('ignores offscreen peaks when zooming, and restores them when zooming out', () => {
    assert.equal(visibleCurveMax(points, 10, 20), 200)
    assert.equal(visibleCurveMax(points, 0, 30), 100_000)
    assert.equal(visibleCurveMax(points, 0, 10), 10_000)
  })

  it('includes the visible part of curves crossing both viewport edges', () => {
    assert.ok(Math.abs(visibleCurveMax(points, 5, 20) - 5050) < 1e-5)
    assert.ok(Math.abs(visibleCurveMax(points, 10, 25) - 50100) < 1e-5)
    const nearLow = visibleCurveMax(points, 9, 20)
    assert.ok(nearLow > 200 && nearLow < 1000)
  })

  it('matches the rendered Bezier curve even with no sample inside the window', () => {
    const segment = [{ x: 0, y: 0 }, { x: 10, y: 1000 }]
    // Bezier parameters t=0.25 and t=0.75: X is not linear in t.
    assert.ok(Math.abs(visibleCurveMax(segment, 2.96875, 7.03125) - 843.75) < 1e-5)
  })

  it('handles empty, zero, single-point and out-of-range windows', () => {
    assert.equal(visibleCurveMax([], 0, 10), 0)
    assert.equal(visibleCurveMax([{ x: 0, y: 0 }, { x: 10, y: 0 }], 2, 8), 0)
    assert.equal(visibleCurveMax([{ x: 5, y: 30 }], 0, 10), 30)
    assert.equal(visibleCurveMax(points, 40, 50), 0)
    assert.equal(visibleCurveMax(points, 20, 10), 0)
  })

  it('scales Tokens and cost independently without modifying either data series', () => {
    const costs = [{ x: 0, y: 100 }, { x: 10, y: 0.2 }, { x: 20, y: 0.05 }]
    const saved = structuredClone(costs)
    assert.equal(visibleCurveMax(points, 10, 20), 200)
    assert.equal(visibleCurveMax(costs, 10, 20), 0.2)
    assert.deepEqual(costs, saved)
  })
})

describe('trend tooltip', () => {
  const size = { width: 248, height: 88 }

  it('sits away from the pointer and flips at the right and top edges', () => {
    const bounds = { width: 600, height: 200 }
    assert.deepEqual(tooltipPosition({ x: 100, y: 150 }, bounds, size), { x: 114, y: 48 })
    assert.deepEqual(tooltipPosition({ x: 580, y: 150 }, bounds, size), { x: 318, y: 48 })
    assert.deepEqual(tooltipPosition({ x: 100, y: 20 }, bounds, size), { x: 114, y: 34 })
  })

  it('stays within the plot container on narrow screens and near every edge', () => {
    for (const width of [300, 600]) {
      const bounds = { width, height: 200 }
      for (const x of [0, width / 2, width]) {
        for (const y of [0, 100, 200]) {
          const p = tooltipPosition({ x, y }, bounds, size)
          assert.ok(p.x >= 8 && p.x + size.width <= width - 8)
          assert.ok(p.y >= 8 && p.y + size.height <= bounds.height - 8)
        }
      }
    }
  })
})

describe('trend curves', () => {
  it('handles empty and single-point series', () => {
    assert.equal(smoothLine([]), '')
    assert.equal(smoothLine([{ x: 10, y: 20 }]), 'M10,20')
  })

  it('passes through every original point without mutating the series', () => {
    const points = [{ x: 0, y: 100 }, { x: 20, y: 10 }, { x: 100, y: 100 }]
    const saved = structuredClone(points)
    assert.equal(smoothLine(points), 'M0,100 C10,100 10,10 20,10 C60,10 60,100 100,100')
    assert.deepEqual(points, saved)
  })

  it('does not overshoot peaks or lift zero gaps', () => {
    // Evaluate the actual generated Bezier control points, not just a snapshot.
    const top = [{ x: 0, y: 100 }, { x: 1, y: 10 }, { x: 3, y: 100 }, { x: 10, y: 100 }]
    const segments = (path: string) => [...path.matchAll(/C([\d., -]+)/g)]
      .map((m) => m[1].trim().split(/[ ,]+/).map(Number))
    const a = segments(smoothLine(top))
    const bezier = (p0: number, p1: number, p2: number, p3: number, t: number) =>
      (1 - t) ** 3 * p0 + 3 * (1 - t) ** 2 * t * p1 + 3 * (1 - t) * t ** 2 * p2 + t ** 3 * p3
    assert.equal(a.length, top.length - 1)
    for (let i = 0; i < a.length; i++) {
      for (let j = 0; j <= 100; j++) {
        const t = j / 100
        const ya = bezier(top[i].y, a[i][1], a[i][3], a[i][5], t)
        assert.ok(ya >= Math.min(top[i].y, top[i + 1].y) - 1e-9)
        assert.ok(ya <= Math.max(top[i].y, top[i + 1].y) + 1e-9)
        if (i === 2) assert.ok(Math.abs(ya - 100) < 1e-9)
      }
    }
  })
})
