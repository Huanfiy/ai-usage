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

/** Sliding query window: presets follow now; custom ending today extends `to`. */
export function liveRange(applied: AppliedRange, now = new Date()): { from: Date; to: Date } {
  if (applied.preset !== 'custom') return resolvePreset(applied.preset, now)
  const from = applied.from
  let to = applied.to
  if (toYmd(to) === toYmd(now) && to.getTime() < now.getTime()) to = new Date(now)
  return { from, to }
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

export const RANGE_STORAGE_KEY = 'ai-usage.timeRange'

export type StoredRange = {
  preset: PresetId
  from?: string
  to?: string
}

export function isPresetId(v: unknown): v is PresetId {
  return typeof v === 'string' && PRESETS.some((p) => p.id === v)
}

export function defaultRange(now = new Date()): AppliedRange {
  return { ...resolvePreset('7d', now), preset: '7d' }
}

/** Inclusive local calendar days spanned by `[from, to]`. */
export function coveredDays(from: Date, to: Date): number {
  const a = startOfLocalDay(from).getTime()
  const b = startOfLocalDay(to).getTime()
  return Math.round(Math.abs(b - a) / 86_400_000) + 1
}

export function customTabLabel(from: Date, to: Date): string {
  return `${coveredDays(from, to)}D`
}

export function firstOfMonthYmd(year: number, monthIndex: number, today = new Date()): string {
  const ymd = toYmd(new Date(year, monthIndex, 1))
  const limit = toYmd(today)
  return ymd > limit ? limit : ymd
}

export function serializeRange(r: AppliedRange): StoredRange {
  if (r.preset === 'custom') return { preset: 'custom', from: toYmd(r.from), to: toYmd(r.to) }
  return { preset: r.preset }
}

export function parseStoredRange(raw: unknown, now = new Date()): AppliedRange | null {
  if (!raw || typeof raw !== 'object') return null
  const v = raw as Record<string, unknown>
  if (!isPresetId(v.preset)) return null
  if (v.preset !== 'custom') return { ...resolvePreset(v.preset, now), preset: v.preset }
  if (typeof v.from !== 'string' || typeof v.to !== 'string') return null
  if (!parseYmd(v.from) || !parseYmd(v.to)) return null
  return { ...resolveCustom(v.from, v.to, now), preset: 'custom' }
}

export function loadStoredRange(now = new Date()): AppliedRange {
  try {
    const raw = localStorage.getItem(RANGE_STORAGE_KEY)
    if (!raw) return defaultRange(now)
    return parseStoredRange(JSON.parse(raw), now) ?? defaultRange(now)
  } catch {
    return defaultRange(now)
  }
}

export function saveStoredRange(r: AppliedRange): void {
  try {
    localStorage.setItem(RANGE_STORAGE_KEY, JSON.stringify(serializeRange(r)))
  } catch {
    /* ignore quota */
  }
}
