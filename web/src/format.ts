export type Metric = 'tokens' | 'cost'

export function fmtTokens(n: number): string {
  const x = Number(n) || 0
  if (x >= 1_000_000_000) return (x / 1_000_000_000).toFixed(2) + 'B'
  if (x >= 1_000_000) return (x / 1_000_000).toFixed(2) + 'M'
  if (x >= 1_000) return (x / 1_000).toFixed(1) + 'K'
  return String(Math.round(x))
}

export function fmtInt(n: number): string {
  return Math.round(Number(n) || 0).toLocaleString('zh-CN')
}

export function fmtAxisTokens(n: number): string {
  const x = Number(n) || 0
  if (x >= 1_000_000) return (x / 1_000_000).toFixed(x >= 10_000_000 ? 0 : 1) + 'M'
  if (x >= 1000) return Math.round(x / 1000) + 'k'
  return String(Math.round(x))
}

export function fmtUsd(n: number): string {
  const x = Number(n) || 0
  if (x >= 100) return '$' + x.toFixed(0)
  if (x >= 1) return '$' + x.toFixed(2)
  return '$' + x.toFixed(4)
}

export function fmtPct(n: number): string {
  return ((Number(n) || 0) * 100).toFixed(1) + '%'
}

export function fmtDur(s: number): string {
  const x = Math.max(0, Number(s) || 0)
  if (x < 60) return Math.round(x) + 's'
  const d = Math.floor(x / 86400)
  const h = Math.floor((x % 86400) / 3600)
  const m = Math.floor((x % 3600) / 60)
  if (d > 0) return h > 0 ? `${d}d ${h}h` : `${d}d`
  return h > 0 ? `${h}h ${m}m` : `${m}m`
}

export function fmtTime(iso: string): string {
  if (!iso) return ''
  return new Date(iso).toLocaleString()
}

/** `+08:00` / `-05:30` → minutes east of UTC. Invalid → null. */
export function parseUtcOffsetMinutes(raw?: string | null): number | null {
  if (!raw) return null
  const m = /^([+-])(\d{2}):(\d{2})$/.exec(raw.trim())
  if (!m) return null
  const h = Number(m[2])
  const min = Number(m[3])
  if (h > 14 || min > 59) return null
  const sign = m[1] === '-' ? -1 : 1
  return sign * (h * 60 + min)
}

type Wall = { y: number; m: number; d: number; h: number; min: number; s: number }

function pad2(n: number): string {
  return String(n).padStart(2, '0')
}

function wallTime(iso: string, offsetMin: number): Wall | null {
  const t = new Date(iso).getTime()
  if (!Number.isFinite(t)) return null
  const x = new Date(t + offsetMin * 60_000)
  return {
    y: x.getUTCFullYear(),
    m: x.getUTCMonth() + 1,
    d: x.getUTCDate(),
    h: x.getUTCHours(),
    min: x.getUTCMinutes(),
    s: x.getUTCSeconds(),
  }
}

function md(p: Wall): string {
  return `${p.m}/${p.d}`
}

function hms(p: Wall): string {
  return `${pad2(p.h)}:${pad2(p.min)}:${pad2(p.s)}`
}

/** Same calendar day: `8/24 01:20:33-01:20:58`. Cross-day: `8/24 01:20:33 ~ 8/25 01:20:58`. */
export function fmtSessionSpan(fromIso: string, toIso: string, tz?: string | null): string {
  const offset = parseUtcOffsetMinutes(tz) ?? -new Date().getTimezoneOffset()
  const a = wallTime(fromIso, offset)
  const b = wallTime(toIso, offset)
  if (!a || !b) return ''
  if (a.y === b.y && a.m === b.m && a.d === b.d) return `${md(a)} ${hms(a)}-${hms(b)}`
  return `${md(a)} ${hms(a)} ~ ${md(b)} ${hms(b)}`
}

export function fmtMetric(metric: Metric, n: number): string {
  return metric === 'cost' ? fmtUsd(n) : fmtTokens(n)
}

/** Per-token USD → `$x.xx / 1M`. */
export function fmtPerMillion(perToken: number): string {
  const x = (Number(perToken) || 0) * 1_000_000
  if (x <= 0) return '$0 / 1M'
  if (x >= 100) return '$' + x.toFixed(0) + ' / 1M'
  if (x >= 1) return '$' + x.toFixed(2) + ' / 1M'
  const s = x.toFixed(4).replace(/0+$/, '').replace(/\.$/, '')
  return '$' + s + ' / 1M'
}
