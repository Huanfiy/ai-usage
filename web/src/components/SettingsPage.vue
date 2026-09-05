<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { api, type HostRow, type PricingStatus } from '../api'
import { fmtTime } from '../format'

const props = defineProps<{
  hideProjects: boolean
  hosts: HostRow[]
}>()

const emit = defineEmits<{
  'update:hideProjects': [boolean]
  changed: []
}>()

type TokenItem = {
  token_prefix: string
  host_id: string
  label: string | null
  created_at: string
  revoked_at: string | null
  hostname: string
}

type JoinItem = {
  join_id: string
  confirm_pin: string
  hostname: string
  agent_version: string | null
  created_at: string
  expires_at: string
}

const tokens = ref<TokenItem[]>([])
const joins = ref<JoinItem[]>([])
const pricing = ref<PricingStatus | null>(null)
const pricingMsg = ref('')
const pricingBusy = ref(false)
const err = ref('')

const priceBusy = computed(() => pricingBusy.value || pricing.value?.updating === true)

const hostSeen = computed(() => {
  const m: Record<string, string> = {}
  for (const h of props.hosts) m[h.host_id] = h.last_seen
  return m
})

async function load() {
  const [r, j, p] = await Promise.all([api.tokens(), api.joins(), api.pricing()])
  tokens.value = (r.items ?? []) as TokenItem[]
  joins.value = (j.items ?? []) as JoinItem[]
  pricing.value = p
}

async function updatePricing() {
  pricingBusy.value = true
  pricingMsg.value = ''
  err.value = ''
  try {
    const r = await api.updatePricing()
    pricing.value = r
    pricingMsg.value = `已拉取 ${r.fetched} 条模型报价，费用已按新价目重算`
    emit('changed')
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  } finally {
    pricingBusy.value = false
  }
}

onMounted(() => {
  void load()
})
const poll = setInterval(() => {
  void load()
}, 4000)
onUnmounted(() => clearInterval(poll))

async function approve(id: string) {
  err.value = ''
  try {
    await api.approveJoin(id)
    await load()
    emit('changed')
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  }
}

async function deny(id: string) {
  err.value = ''
  try {
    await api.denyJoin(id)
    await load()
    emit('changed')
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  }
}

async function revoke(hostId: string) {
  if (!confirm('吊销该 ingest token？对应主机将无法继续上报。')) return
  err.value = ''
  try {
    await api.revokeToken(hostId)
    await load()
    emit('changed')
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  }
}

async function remove(hostId: string) {
  if (!confirm('删除该主机及其用量？此操作不可恢复。Cursor 账号用量会保留。')) return
  err.value = ''
  try {
    await api.deleteHost(hostId)
    await load()
    emit('changed')
  } catch (e) {
    err.value = e instanceof Error ? e.message : String(e)
  }
}
</script>

<template>
  <div class="settings-page">
    <p v-if="err" class="err">{{ err }}</p>

    <section class="card" style="margin-bottom: 12px">
      <h2>隐私</h2>
      <div class="switch-row">
        <div>
          <div class="switch-title">隐藏项目名</div>
          <p class="switch-hint">看板筛选、分布图与会话列表不再展示项目路径。</p>
        </div>
        <button
          type="button"
          class="switch"
          :class="{ on: hideProjects }"
          role="switch"
          :aria-checked="hideProjects"
          aria-label="隐藏项目名"
          @click="emit('update:hideProjects', !hideProjects)"
        >
          <i />
        </button>
      </div>
    </section>

    <section class="card" style="margin-bottom: 12px">
      <h2>价目表</h2>
      <div class="switch-row">
        <div>
          <div class="switch-title">
            {{ pricing?.cached ? '已刷新的上游价目表' : '内置价目快照' }}
          </div>
          <p class="switch-hint">
            <template v-if="pricing">
              {{ pricing.models }} 个模型 ·
              {{ pricing.updated_at ? `更新于 ${fmtTime(pricing.updated_at)}` : '无更新时间' }}
            </template>
            <template v-else>读取中…</template>
            <br />
            费用在查询时按价目折算，刷新后历史数据一并重算。新模型需上游 LiteLLM 已收录。
          </p>
        </div>
        <button type="button" class="chip" :disabled="priceBusy" @click="updatePricing">
          {{ priceBusy ? '更新中…' : '更新价目表' }}
        </button>
      </div>
      <p v-if="pricingMsg" class="switch-hint" style="margin-bottom: 0">{{ pricingMsg }}</p>
    </section>

    <section class="card" style="margin-bottom: 12px">
      <h2>待审批接入</h2>
      <p class="switch-hint" style="margin-top: 0">对照采集端面板上的确认码后批准。本机也需要点一次。</p>
      <table v-if="joins.length">
        <thead>
          <tr>
            <th>主机名</th>
            <th>确认码</th>
            <th>申请时间</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="j in joins" :key="j.join_id">
            <td>{{ j.hostname }}</td>
            <td class="mono">{{ j.confirm_pin }}</td>
            <td>{{ fmtTime(j.created_at) }}</td>
            <td>
              <button type="button" class="chip" @click="approve(j.join_id)">批准</button>
              <button type="button" class="chip" @click="deny(j.join_id)">拒绝</button>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-else class="empty">没有等待批准的申请</div>
    </section>

    <section class="card">
      <h2>主机接入</h2>
      <table v-if="tokens.length">
        <thead>
          <tr>
            <th>显示名</th>
            <th>前缀</th>
            <th>上次同步</th>
            <th>状态</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="t in tokens" :key="t.host_id + t.token_prefix">
            <td>{{ t.hostname }}</td>
            <td class="mono">{{ t.token_prefix }}</td>
            <td>{{ hostSeen[t.host_id] ? fmtTime(hostSeen[t.host_id]) : '—' }}</td>
            <td>{{ t.revoked_at ? '已吊销' : '有效' }}</td>
            <td>
              <button v-if="!t.revoked_at" type="button" class="chip" @click="revoke(t.host_id)">吊销</button>
              <button v-else type="button" class="chip" @click="remove(t.host_id)">删除</button>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-else class="empty">还没有已接入的主机</div>
    </section>
  </div>
</template>
