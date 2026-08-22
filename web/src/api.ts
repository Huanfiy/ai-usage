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

export type HostRow = { host_id: string; hostname: string; last_seen: string; agent_version?: string | null }
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
  filters: (q: Query) => get<{ sources: string[]; models: string[]; projects: string[] }>('/v1/filters' + qs(q)),
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
  createToken: async (hostname?: string) => {
    const r = await fetch('/v1/tokens', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ hostname: hostname || 'unnamed' }),
    })
    if (!r.ok) throw new Error(`/v1/tokens ${r.status}`)
    return r.json() as Promise<{ token: string; host_id: string; token_prefix: string }>
  },
  revokeToken: async (hostId: string) => {
    const r = await fetch(`/v1/tokens/${hostId}`, { method: 'DELETE' })
    if (!r.ok) throw new Error(`/v1/tokens/${hostId} ${r.status}`)
  },
}
