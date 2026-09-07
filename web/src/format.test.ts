import assert from 'node:assert/strict'
import { describe, it } from 'node:test'
import { fmtSessionSpan, fmtTime, fmtTimeTick, parseUtcOffsetMinutes } from './format.ts'

describe('parseUtcOffsetMinutes', () => {
  it('parses east and west offsets', () => {
    assert.equal(parseUtcOffsetMinutes('+08:00'), 480)
    assert.equal(parseUtcOffsetMinutes('-05:30'), -330)
    assert.equal(parseUtcOffsetMinutes('UTC+8'), null)
  })
})

describe('fmtSessionSpan', () => {
  it('same day omits the second date', () => {
    assert.equal(
      fmtSessionSpan('2026-08-23T17:20:33Z', '2026-08-23T17:20:58Z', '+08:00'),
      '8/24 01:20:33-01:20:58',
    )
  })

  it('cross-day repeats both dates', () => {
    assert.equal(
      fmtSessionSpan('2026-08-23T15:20:33Z', '2026-08-23T17:20:58Z', '+08:00'),
      '8/23 23:20:33 ~ 8/24 01:20:58',
    )
  })
})

describe('24 小时制时间', () => {
  it('fmtTime 固定输出补零的 24 小时制', () => {
    for (const iso of [
      '2026-01-15T00:05:06Z',
      '2026-01-15T12:05:06Z',
      '2026-01-15T13:05:06Z',
      '2026-01-15T23:05:06Z',
    ]) {
      assert.match(fmtTime(iso), /^\d{4}\/\d{2}\/\d{2} ([01]\d|2[0-3]):[0-5]\d:[0-5]\d$/)
    }
  })

  it('趋势刻度在短窗口显示 24 小时制', () => {
    assert.match(fmtTimeTick('2026-01-15T13:05:06Z', true), /^\d{2}\/\d{2} ([01]\d|2[0-3]):[0-5]\d$/)
    assert.match(fmtTimeTick('2026-01-15T13:05:06Z', false), /^\d{2}\/\d{2}$/)
  })
})
