<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api, type HostRow } from '../api'
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

const tokens = ref<TokenItem[]>([])
const hostname = ref('')
const newToken = ref('')
const err = ref('')

const hostSeen = computed(() => {
  const m: Record<string, string> = {}
  for (const h of props.hosts) m[h.host_id] = h.last_seen
  return m
})

async function loadTokens() {
  const r = await api.tokens()
  tokens.value = (r.items ?? []) as TokenItem[]
}

onMounted(loadTokens)

async function createHostToken() {
  err.value = ''
  const name = hostname.value.trim() || 'unnamed'
  try {
    const r = await api.createToken(name)
    newToken.value = r.token
    hostname.value = ''
    await loadTokens()
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
    if (newToken.value) newToken.value = ''
    await loadTokens()
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
    if (newToken.value) newToken.value = ''
    await loadTokens()
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
      <div v-else class="empty">还没有签发 token</div>
      <div class="settings">
        <input v-model="hostname" placeholder="主机显示名" @keydown.enter="createHostToken" />
        <button type="button" class="chip" @click="createHostToken">新建 ingest token</button>
        <span v-if="newToken" class="mono">新 token（只显示一次）：{{ newToken }}</span>
      </div>
    </section>
  </div>
</template>
