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

export function fmtMetric(metric: Metric, n: number): string {
  return metric === 'cost' ? fmtUsd(n) : fmtTokens(n)
}
