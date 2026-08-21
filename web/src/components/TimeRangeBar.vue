<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import {
  PRESETS,
  fmtMdy,
  resolveCustom,
  resolvePreset,
  toYmd,
  type AppliedRange,
  type PresetId,
} from '../timeRange'

const emit = defineEmits<{ apply: [AppliedRange] }>()

const wrap = ref<HTMLElement | null>(null)
const preset = ref<PresetId>('7d')
const open = ref(false)
const init = resolvePreset('7d')
const fromYmd = ref(toYmd(init.from))
const toYmdVal = ref(toYmd(init.to))

function syncDates(from: Date, to: Date) {
  fromYmd.value = toYmd(from)
  toYmdVal.value = toYmd(to)
}

function applyRange(r: AppliedRange) {
  preset.value = r.preset
  syncDates(r.from, r.to)
  open.value = false
  emit('apply', r)
}

function selectPreset(id: PresetId) {
  if (id === 'custom') {
    open.value = !open.value
    return
  }
  const { from, to } = resolvePreset(id)
  applyRange({ from, to, preset: id })
}

function applyCustom() {
  const { from, to } = resolveCustom(fromYmd.value, toYmdVal.value)
  applyRange({ from, to, preset: 'custom' })
}

function isActive(id: PresetId): boolean {
  if (open.value) return id === 'custom'
  return preset.value === id
}

function onDoc(ev: PointerEvent) {
  if (!open.value) return
  const el = wrap.value
  if (el && !el.contains(ev.target as Node)) open.value = false
}

onMounted(() => document.addEventListener('pointerdown', onDoc))
onUnmounted(() => document.removeEventListener('pointerdown', onDoc))
</script>

<template>
  <div ref="wrap" class="range-wrap">
    <div class="seg" role="tablist" aria-label="时间范围">
      <button
        v-for="p in PRESETS"
        :key="p.id"
        type="button"
        role="tab"
        :aria-selected="isActive(p.id)"
        :aria-expanded="p.id === 'custom' ? open : undefined"
        :class="{ active: isActive(p.id) }"
        @click="selectPreset(p.id)"
      >
        {{ p.label }}
      </button>
    </div>
    <div v-if="open" class="custom-pop" role="dialog" aria-label="自定义时间范围">
      <label class="date-box">
        <span class="date-text">{{ fmtMdy(fromYmd) }}</span>
        <svg class="cal" viewBox="0 0 24 24" aria-hidden="true">
          <rect x="3" y="5" width="18" height="16" rx="2" />
          <path d="M16 3v4M8 3v4M3 11h18" />
        </svg>
        <input type="date" :value="fromYmd" @input="fromYmd = ($event.target as HTMLInputElement).value" />
      </label>
      <span class="hyphen">-</span>
      <label class="date-box">
        <span class="date-text">{{ fmtMdy(toYmdVal) }}</span>
        <svg class="cal" viewBox="0 0 24 24" aria-hidden="true">
          <rect x="3" y="5" width="18" height="16" rx="2" />
          <path d="M16 3v4M8 3v4M3 11h18" />
        </svg>
        <input type="date" :value="toYmdVal" @input="toYmdVal = ($event.target as HTMLInputElement).value" />
      </label>
      <button type="button" class="apply-btn" @click="applyCustom">应用</button>
    </div>
  </div>
</template>

<style scoped>
.range-wrap {
  position: relative;
  flex: 0 0 auto;
}
.seg {
  display: flex;
  align-items: center;
  background: var(--bg-elev-2);
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 3px;
}
.seg button {
  background: transparent;
  border: none;
  color: var(--muted);
  padding: 6px 12px;
  border-radius: 999px;
  font-size: 13px;
  white-space: nowrap;
  line-height: 1.2;
}
.seg button:hover {
  color: var(--text);
}
.seg button.active {
  background: #fff;
  color: #111;
  font-weight: 600;
}
.custom-pop {
  position: absolute;
  z-index: 20;
  top: calc(100% + 8px);
  left: 0;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px;
  background: var(--bg-elev);
  border: 1px solid var(--line);
  border-radius: 14px;
  box-shadow: var(--shadow);
}
.date-box {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  min-width: 152px;
  background: var(--bg-elev-2);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 8px 12px;
  color: var(--text);
  font-size: 13px;
  font-variant-numeric: tabular-nums;
  cursor: pointer;
}
.date-box input[type='date'] {
  position: absolute;
  inset: 0;
  opacity: 0;
  cursor: pointer;
  border: none;
  background: transparent;
  padding: 0;
}
.cal {
  width: 15px;
  height: 15px;
  flex: 0 0 15px;
  stroke: var(--text);
  fill: none;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.hyphen {
  color: var(--muted);
  font-size: 14px;
}
.apply-btn {
  background: #fff;
  color: #111;
  border: none;
  border-radius: 999px;
  padding: 8px 18px;
  font-size: 13px;
  font-weight: 600;
  line-height: 1.2;
}
.apply-btn:hover {
  filter: brightness(0.94);
}
</style>
