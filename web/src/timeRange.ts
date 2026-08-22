export const PRESETS = [
  { id: 'today', label: '今天' },
  { id: '24h', label: '24H' },
  { id: '7d', label: '7D' },
  { id: '30d', label: '30D' },
  { id: '90d', label: '90D' },
  { id: 'custom', label: '自定义' },
] as const

export type PresetId = (typeof PRESETS)[number]['id']

export type AppliedRange = {
  from: Date
  to: Date
  preset: PresetId
}

export function startOfLocalDay(d: Date): Date {
  const x = new Date(d)
  x.setHours(0, 0, 0, 0)
  return x
}

export function endOfLocalDay(d: Date): Date {
  const x = new Date(d)
  x.setHours(23, 59, 59, 999)
  return x
}

export function toYmd(d: Date): string {
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${y}-${m}-${day}`
}

export function parseYmd(ymd: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd)
  if (!m) return null
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]))
  return Number.isNaN(d.getTime()) ? null : d
}

export function fmtYmd(ymd: string): string {
  const d = parseYmd(ymd)
  if (!d) return ymd
  return `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日`
}

export function resolvePreset(id: Exclude<PresetId, 'custom'>, now = new Date()): { from: Date; to: Date } {
  const to = new Date(now)
  if (id === 'today') return { from: startOfLocalDay(to), to }
  const hours: Record<string, number> = { '24h': 24, '7d': 24 * 7, '30d': 24 * 30, '90d': 24 * 90 }
  const h = hours[id] ?? 24 * 7
  return { from: new Date(to.getTime() - h * 3600 * 1000), to }
}

export function resolveCustom(fromYmd: string, toYmd: string, now = new Date()): { from: Date; to: Date } {
  let a = parseYmd(fromYmd) ?? startOfLocalDay(now)
  let b = parseYmd(toYmd) ?? startOfLocalDay(now)
  if (a > b) {
    const tmp = a
    a = b
    b = tmp
  }
  const from = startOfLocalDay(a)
  let to = endOfLocalDay(b)
  if (to > now) to = new Date(now)
  if (to < from) to = endOfLocalDay(a)
  return { from, to }
}

export function presetLabel(id: PresetId, from: Date, to: Date): string {
  if (id === 'custom') return `${fmtYmd(toYmd(from))} – ${fmtYmd(toYmd(to))}`
  return PRESETS.find((p) => p.id === id)?.label ?? id
}
