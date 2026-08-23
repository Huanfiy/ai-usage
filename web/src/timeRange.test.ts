import assert from 'node:assert/strict'
import { describe, it } from 'node:test'
import {
  coveredDays,
  customTabLabel,
  defaultRange,
  firstOfMonthYmd,
  parseStoredRange,
  resolveCustom,
  serializeRange,
  toYmd,
} from './timeRange.ts'

const now = new Date(2026, 7, 24, 15, 30, 0)

describe('coveredDays', () => {
  it('counts the same local day as 1', () => {
    assert.equal(coveredDays(new Date(2026, 7, 24, 1), new Date(2026, 7, 24, 23)), 1)
  })

  it('counts inclusive calendar days', () => {
    assert.equal(coveredDays(new Date(2025, 0, 1), new Date(2025, 0, 7)), 7)
    assert.equal(coveredDays(new Date(2025, 0, 1), new Date(2025, 11, 31)), 365)
  })

  it('is order-independent', () => {
    assert.equal(coveredDays(new Date(2026, 7, 24), new Date(2025, 7, 1)), 389)
  })
})

describe('customTabLabel', () => {
  it('uses the D suffix of presets', () => {
    assert.equal(customTabLabel(new Date(2025, 7, 1), new Date(2026, 7, 24)), '389D')
  })
})

describe('firstOfMonthYmd', () => {
  it('returns the 1st of the viewed month', () => {
    assert.equal(firstOfMonthYmd(2025, 7, now), '2025-08-01')
    assert.equal(firstOfMonthYmd(2025, 0, now), '2025-01-01')
  })

  it('clamps a future 1st to today', () => {
    assert.equal(firstOfMonthYmd(2026, 11, now), '2026-08-24')
  })
})

describe('parseStoredRange', () => {
  it('restores a sliding preset from now', () => {
    const r = parseStoredRange({ preset: '30d' }, now)
    assert.ok(r)
    assert.equal(r.preset, '30d')
    assert.equal(r.to.getTime(), now.getTime())
    assert.equal(r.from.getTime(), now.getTime() - 30 * 24 * 3600 * 1000)
  })

  it('restores a custom range by ymd', () => {
    const r = parseStoredRange({ preset: 'custom', from: '2025-01-01', to: '2026-08-24' }, now)
    assert.ok(r)
    assert.equal(r.preset, 'custom')
    assert.equal(toYmd(r.from), '2025-01-01')
    assert.equal(toYmd(r.to), '2026-08-24')
  })

  it('rejects invalid payloads', () => {
    assert.equal(parseStoredRange(null, now), null)
    assert.equal(parseStoredRange({ preset: 'nope' }, now), null)
    assert.equal(parseStoredRange({ preset: 'custom', from: '2025-01-01' }, now), null)
    assert.equal(parseStoredRange({ preset: 'custom', from: 'bad', to: '2026-01-01' }, now), null)
  })
})

describe('serializeRange', () => {
  it('keeps only the preset for sliding windows', () => {
    assert.deepEqual(serializeRange({ ...defaultRange(now), preset: '90d' }), { preset: '90d' })
  })

  it('round-trips custom ymd through parse', () => {
    const applied = { ...resolveCustom('2025-08-01', '2026-08-24', now), preset: 'custom' as const }
    const parsed = parseStoredRange(serializeRange(applied), now)
    assert.ok(parsed)
    assert.equal(parsed.preset, 'custom')
    assert.equal(toYmd(parsed.from), '2025-08-01')
    assert.equal(toYmd(parsed.to), '2026-08-24')
  })
})
