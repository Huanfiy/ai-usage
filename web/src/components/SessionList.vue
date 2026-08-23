<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { SessionRow } from '../api'
import { fmtDur, fmtTime, fmtTokens } from '../format'

const props = defineProps<{ sessions: SessionRow[] }>()

const PAGE_SIZES = [10, 20, 50]
const pageSize = ref(10)
const page = ref(1)

const total = computed(() => props.sessions.length)
const totalPages = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)))

const pageItems = computed(() => {
  const start = (page.value - 1) * pageSize.value
  return props.sessions.slice(start, start + pageSize.value)
})

const fromN = computed(() => (total.value === 0 ? 0 : (page.value - 1) * pageSize.value + 1))
const toN = computed(() => Math.min(total.value, page.value * pageSize.value))

const pageNums = computed(() => {
  const t = totalPages.value
  const cur = page.value
  if (t <= 7) return Array.from({ length: t }, (_, i) => i + 1)
  let start = Math.max(1, cur - 3)
  let end = Math.min(t, start + 6)
  start = Math.max(1, end - 6)
  return Array.from({ length: end - start + 1 }, (_, i) => start + i)
})

watch(pageSize, () => {
  page.value = 1
})

watch(
  () => props.sessions,
  () => {
    if (page.value > totalPages.value) page.value = totalPages.value
  },
)

function goto(p: number) {
  page.value = Math.min(totalPages.value, Math.max(1, p))
}

function tokenHint(s: SessionRow): string {
  const t = s.tokens
  if (!t) return ''
  return `入 ${fmtTokens(t.input)} · 出 ${fmtTokens(t.output)} · 读 ${fmtTokens(t.cache_read)} · 创建 ${fmtTokens(t.cache_creation)} · 推理 ${fmtTokens(t.reasoning)}`
}
</script>

<template>
  <section class="card">
    <div class="card-head">
      <h2>会话列表</h2>
      <label class="page-size">
        每页
        <select v-model.number="pageSize">
          <option v-for="n in PAGE_SIZES" :key="n" :value="n">{{ n }}</option>
        </select>
      </label>
    </div>
    <table v-if="pageItems.length">
      <thead>
        <tr>
          <th>工具</th>
          <th>项目</th>
          <th>开始</th>
          <th>结束</th>
          <th>时长</th>
          <th>活跃</th>
          <th>消息</th>
          <th>Token</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="s in pageItems" :key="s.host_id + s.session_hash">
          <td class="mono">{{ s.source }}</td>
          <td>{{ s.project }}</td>
          <td>{{ fmtTime(s.first_message_at) }}</td>
          <td>{{ fmtTime(s.last_message_at) }}</td>
          <td>{{ fmtDur(s.duration_seconds) }}</td>
          <td>{{ fmtDur(s.active_seconds) }}</td>
          <td>{{ s.user_message_count }} / {{ s.message_count }}</td>
          <td :title="tokenHint(s)">{{ fmtTokens(s.tokens?.total ?? 0) }}</td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">窗口内没有会话</div>
    <div v-if="total" class="pager">
      <span class="pager-meta">第 {{ fromN }}–{{ toN }} 条，共 {{ total }} 条</span>
      <div class="pager-btns">
        <button type="button" :disabled="page <= 1" @click="goto(page - 1)">上一页</button>
        <button
          v-for="n in pageNums"
          :key="n"
          type="button"
          :class="{ active: n === page }"
          @click="goto(n)"
        >
          {{ n }}
        </button>
        <button type="button" :disabled="page >= totalPages" @click="goto(page + 1)">下一页</button>
      </div>
    </div>
  </section>
</template>
