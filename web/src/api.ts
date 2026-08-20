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
}

export type SeriesPoint = { t: string; tokens: number; cost_usd: number }
export type BreakdownItem = { key: string; tokens: number; cost_usd: number; share: number }
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
  if (!r.ok) throw new Error(`${path} ${r.status}`)
  return r.json() as Promise<T>
}

export const api = {
  health: () => get<{ ok: boolean; version: string }>('/v1/health'),
  summary: (q: Query) => get<Summary>('/v1/summary' + qs(q)),
  series: (q: Query) => get<{ points: SeriesPoint[] }>('/v1/series' + qs(q)),
  breakdown: (q: Query, by: string) => get<{ items: BreakdownItem[] }>('/v1/breakdown' + qs({ ...q, by })),
  sessions: (q: Query) => get<{ items: SessionRow[] }>('/v1/sessions' + qs({ ...q, limit: 80 })),
  hosts: () => get<{ items: HostRow[] }>('/v1/hosts'),
  filters: (q: Query) => get<{ sources: string[]; models: string[]; projects: string[] }>('/v1/filters' + qs(q)),
  tokens: () => get<{ items: Array<Record<string, string | null>> }>('/v1/tokens'),
  createToken: (hostname?: string) =>
    fetch('/v1/tokens', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ hostname: hostname || 'unnamed' }),
    }).then((r) => r.json()),
  revokeToken: (hostId: string) => fetch(`/v1/tokens/${hostId}`, { method: 'DELETE' }),
}
