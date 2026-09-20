export type TokenTotals = {
  input: number
  output: number
  cache_read: number
  cache_creation: number
  reasoning: number
  total: number
}

export type Summary = {
  from: string
  to: string
  tokens: TokenTotals
  cost_usd: number
  cost_coverage: number
  cache_hit_rate: number
  sessions: number
  hosts: number
  sources: number
  message_count: number
  user_message_count: number
  duration_seconds: number
  active_seconds: number
}

export type SeriesPoint = {
  t: string
  tokens: number
  cost_usd: number
  input: number
  output: number
  cache_read: number
  cache_creation: number
}

export type ModelPrice = {
  input: number
  output: number
  cache_read?: number | null
  cache_write?: number | null
  reasoning?: number | null
}

export type BreakdownItem = {
  key: string
  tokens: number
  cost_usd: number
  share: number
  pricing?: ModelPrice | null
}

export type Distributions = {
  host: BreakdownItem[]
  source: BreakdownItem[]
  model: BreakdownItem[]
  project: BreakdownItem[]
}

export type ActivityCell = { dow: number; hour: number; tokens: number; cost_usd: number }
export type Activity = { cells: ActivityCell[] }

export type HostRow = {
  host_id: string
  hostname: string
  last_seen: string
  agent_version?: string | null
  timezone?: string | null
}
export type CursorAccountRow = {
  account_hash: string
  account_label: string
  membership?: string | null
  /** 当前账期起点；旧采集端不上报，缺失时前端按月账期从 billing_cycle_end 反推 */
  billing_cycle_start?: string | null
  billing_cycle_end?: string | null
  api_percent?: number | null
  auto_percent?: number | null
  bot_percent?: number | null
  bot_period_start?: string | null
  bot_next_reset?: string | null
  bot_available?: boolean | null
  /** 本账期总消耗（美分）：plan.used + breakdown.bonus */
  total_used_cents?: number | null
  fetched_at: string
  updated_at: string
  archived_at?: string | null
  stale: boolean
}

export type SessionRow = {
  host_id: string
  source: string
  project: string
  session_hash: string
  first_message_at: string
  last_message_at: string
  duration_seconds: number
  active_seconds: number
  message_count: number
  user_message_count: number
  tokens?: TokenTotals
}

export type PricingStatus = {
  updated_at: string | null
  models: number
  cached: boolean
  updating: boolean
}

export type Query = {
  from: string
  to: string
  host?: string
  source?: string
  model?: string
  project?: string
  hide_projects?: boolean
}

function qs(q: Query & Record<string, string | number | boolean | undefined>): string {
  const p = new URLSearchParams()
  for (const [k, v] of Object.entries(q)) {
    if (v === undefined || v === '' || v === false) continue
    p.set(k, String(v))
  }
  const s = p.toString()
  return s ? `?${s}` : ''
}

async function get<T>(path: string): Promise<T> {
  const r = await fetch(path)
  const ct = r.headers.get('content-type') || ''
  if (!r.ok) throw new Error(`${path} ${r.status}`)
  if (!ct.includes('json')) throw new Error(`${path} ${r.status} 响应不是 JSON`)
  return r.json() as Promise<T>
}

export const api = {
  health: () => get<{ ok: boolean; version: string }>('/v1/health'),
  summary: (q: Query) => get<Summary>('/v1/summary' + qs(q)),
  series: (q: Query) => get<{ points: SeriesPoint[] }>('/v1/series' + qs(q)),
  breakdown: (q: Query, by: string) => get<{ items: BreakdownItem[] }>('/v1/breakdown' + qs({ ...q, by })),
  distributions: (q: Query) => get<Distributions>('/v1/distributions' + qs(q)),
  activity: (q: Query) => get<Activity>('/v1/activity' + qs(q)),
  sessions: (q: Query) => get<{ items: SessionRow[] }>('/v1/sessions' + qs({ ...q, limit: 200 })),
  hosts: () => get<{ items: HostRow[] }>('/v1/hosts'),
  cursorAccounts: () =>
    get<{ items: CursorAccountRow[]; stale_days: number }>('/v1/cursor-accounts'),
  archiveCursorAccount: async (hash: string) => {
    const r = await fetch(`/v1/cursor-accounts/${encodeURIComponent(hash)}/archive`, {
      method: 'POST',
    })
    if (!r.ok) throw new Error(`/v1/cursor-accounts/${hash}/archive ${r.status}`)
  },
  restoreCursorAccount: async (hash: string) => {
    const r = await fetch(`/v1/cursor-accounts/${encodeURIComponent(hash)}/restore`, {
      method: 'POST',
    })
    if (!r.ok) throw new Error(`/v1/cursor-accounts/${hash}/restore ${r.status}`)
  },
  filters: (q: Query) => get<{ sources: string[]; models: string[]; projects: string[] }>('/v1/filters' + qs(q)),
  pricing: () => get<PricingStatus>('/v1/pricing'),
  updatePricing: async () => {
    const r = await fetch('/v1/pricing/update', { method: 'POST' })
    const body = await r.json().catch(() => ({}))
    if (!r.ok) throw new Error(body.error || `/v1/pricing/update ${r.status}`)
    return body as PricingStatus & { fetched: number }
  },
  tokens: () =>
    get<{
      items: Array<{
        token_prefix: string
        host_id: string
        label: string | null
        created_at: string
        revoked_at: string | null
        hostname: string
      }>
    }>('/v1/tokens'),
  joins: () =>
    get<{
      items: Array<{
        join_id: string
        confirm_pin: string
        hostname: string
        agent_version: string | null
        created_at: string
        expires_at: string
      }>
    }>('/v1/joins'),
  approveJoin: async (joinId: string) => {
    const r = await fetch(`/v1/joins/${encodeURIComponent(joinId)}/approve`, { method: 'POST' })
    if (!r.ok) throw new Error(`/v1/joins/${joinId}/approve ${r.status}`)
  },
  denyJoin: async (joinId: string) => {
    const r = await fetch(`/v1/joins/${encodeURIComponent(joinId)}/deny`, { method: 'POST' })
    if (!r.ok) throw new Error(`/v1/joins/${joinId}/deny ${r.status}`)
  },
  revokeToken: async (hostId: string) => {
    const r = await fetch(`/v1/tokens/${hostId}`, { method: 'DELETE' })
    if (!r.ok) throw new Error(`/v1/tokens/${hostId} ${r.status}`)
  },
  deleteHost: async (hostId: string) => {
    const r = await fetch(`/v1/hosts/${encodeURIComponent(hostId)}`, { method: 'DELETE' })
    if (!r.ok) throw new Error(`/v1/hosts/${hostId} ${r.status}`)
  },
}
