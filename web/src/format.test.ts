import assert from 'node:assert/strict'
import { describe, it } from 'node:test'
import { fmtSessionSpan, parseUtcOffsetMinutes } from './format.ts'

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
