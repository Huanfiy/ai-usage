<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import {
  PRESETS,
  fmtYmd,
  parseYmd,
  resolveCustom,
  resolvePreset,
  toYmd,
  type AppliedRange,
  type PresetId,
} from '../timeRange'

const WEEKDAYS = ['一', '二', '三', '四', '五', '六', '日']
const MONTHS = ['1月', '2月', '3月', '4月', '5月', '6月', '7月', '8月', '9月', '10月', '11月', '12月']
const YEAR_SPAN = 12

type CalMode = 'day' | 'month' | 'year'

const emit = defineEmits<{ apply: [AppliedRange] }>()

const wrap = ref<HTMLElement | null>(null)
const preset = ref<PresetId>('7d')
const open = ref(false)
const picker = ref<'from' | 'to'>('from')
const mode = ref<CalMode>('day')
const init = resolvePreset('7d')
const fromYmd = ref(toYmd(init.from))
const toYmdVal = ref(toYmd(init.to))
const view = ref({ y: init.from.getFullYear(), m: init.from.getMonth() })
const yearStart = ref(init.from.getFullYear() - 5)

const todayYmd = computed(() => toYmd(new Date()))
const maxYear = computed(() => new Date().getFullYear())
const viewMonthLabel = computed(() => `${view.value.m + 1}月`)
const years = computed(() => Array.from({ length: YEAR_SPAN }, (_, i) => yearStart.value + i))
const canNextMonth = computed(() => {
  const n = new Date()
  return (
    view.value.y < n.getFullYear() ||
    (view.value.y === n.getFullYear() && view.value.m < n.getMonth())
  )
})
const canNextHead = computed(() => {
  if (mode.value === 'year') return yearStart.value + YEAR_SPAN - 1 < maxYear.value
  if (mode.value === 'month') return view.value.y < maxYear.value
  return canNextMonth.value
})
const rangeEnds = computed(() => {
  const a = fromYmd.value
  const b = toYmdVal.value
  return a <= b ? { lo: a, hi: b } : { lo: b, hi: a }
})
const cells = computed(() => {
  const { y, m } = view.value
  const firstDow = (new Date(y, m, 1).getDay() + 6) % 7
  const dim = new Date(y, m + 1, 0).getDate()
  const count = Math.ceil((firstDow + dim) / 7) * 7
  const origin = 1 - firstDow
  const today = todayYmd.value
  const { lo, hi } = rangeEnds.value
  const out = []
  for (let i = 0; i < count; i++) {
    const d = new Date(y, m, origin + i)
    const ymd = toYmd(d)
    out.push({
      ymd,
      day: d.getDate(),
      inMonth: d.getMonth() === m,
      disabled: ymd > today,
      today: ymd === today,
      bound: ymd === lo || ymd === hi,
      inRange: ymd > lo && ymd < hi,
    })
  }
  return out
})

function syncView(ymd: string) {
  const d = parseYmd(ymd)
  if (!d) return
  view.value = { y: d.getFullYear(), m: d.getMonth() }
}

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
    const next = !open.value
    open.value = next
    mode.value = 'day'
    if (next) {
      picker.value = 'from'
      syncView(fromYmd.value)
    }
    return
  }
  const { from, to } = resolvePreset(id)
  applyRange({ from, to, preset: id })
}

function applyCustom() {
  const { from, to } = resolveCustom(fromYmd.value, toYmdVal.value)
  applyRange({ from, to, preset: 'custom' })
}

function focusField(which: 'from' | 'to') {
  picker.value = which
  mode.value = 'day'
  syncView(which === 'from' ? fromYmd.value : toYmdVal.value)
}

function syncYearPage() {
  const last = maxYear.value - YEAR_SPAN + 1
  yearStart.value = Math.min(view.value.y - 5, last)
}

function clampView(y: number, m: number): { y: number; m: number } {
  const n = new Date()
  if (y > n.getFullYear()) return { y: n.getFullYear(), m: n.getMonth() }
  if (y === n.getFullYear() && m > n.getMonth()) return { y, m: n.getMonth() }
  return { y, m }
}

function shiftMonth(delta: number) {
  if (delta > 0 && !canNextMonth.value) return
  const d = new Date(view.value.y, view.value.m + delta, 1)
  view.value = clampView(d.getFullYear(), d.getMonth())
}

function shiftHead(delta: number) {
  if (mode.value === 'year') {
    const last = maxYear.value - YEAR_SPAN + 1
    const next = yearStart.value + delta * YEAR_SPAN
    yearStart.value = delta > 0 ? Math.min(next, last) : next
    return
  }
  if (mode.value === 'month') {
    const y = view.value.y + delta
    if (y > maxYear.value) return
    view.value = clampView(y, view.value.m)
    return
  }
  shiftMonth(delta)
}

function toggleYear() {
  mode.value = mode.value === 'year' ? 'day' : 'year'
  if (mode.value === 'year') syncYearPage()
}

function toggleMonth() {
  mode.value = mode.value === 'month' ? 'day' : 'month'
}

function pickYear(y: number) {
  if (y > maxYear.value) return
  view.value = clampView(y, view.value.m)
  mode.value = 'day'
}

function pickMonth(m: number) {
  if (monthDisabled(m)) return
  view.value = { y: view.value.y, m }
  mode.value = 'day'
}

function monthDisabled(m: number): boolean {
  const n = new Date()
  return view.value.y === n.getFullYear() && m > n.getMonth()
}

function pickDay(ymd: string, disabled: boolean) {
  if (disabled) return
  if (picker.value === 'from') fromYmd.value = ymd
  else toYmdVal.value = ymd
  const d = parseYmd(ymd)
  if (d && d.getMonth() !== view.value.m) {
    view.value = { y: d.getFullYear(), m: d.getMonth() }
  }
}

function goToday() {
  const n = new Date()
  const ymd = toYmd(n)
  view.value = { y: n.getFullYear(), m: n.getMonth() }
  mode.value = 'day'
  if (picker.value === 'to') toYmdVal.value = ymd
  else fromYmd.value = ymd
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

function onKey(ev: KeyboardEvent) {
  if (ev.key !== 'Escape' || !open.value) return
  if (mode.value !== 'day') {
    mode.value = 'day'
    return
  }
  open.value = false
}

onMounted(() => {
  document.addEventListener('pointerdown', onDoc)
  document.addEventListener('keydown', onKey)
})
onUnmounted(() => {
  document.removeEventListener('pointerdown', onDoc)
  document.removeEventListener('keydown', onKey)
})
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
    <div v-if="open" class="custom-panel">
      <div class="custom-card" role="dialog" aria-label="自定义时间范围">
      <div class="custom-row">
        <button
          type="button"
          class="date-box"
          :class="{ active: picker === 'from' }"
          aria-label="开始日期"
          :aria-pressed="picker === 'from'"
          @click="focusField('from')"
        >
          <span class="date-text">{{ fmtYmd(fromYmd) }}</span>
          <svg class="cal" viewBox="0 0 24 24" aria-hidden="true">
            <rect x="3" y="5" width="18" height="16" rx="2" />
            <path d="M16 3v4M8 3v4M3 11h18" />
          </svg>
        </button>
        <span class="hyphen">-</span>
        <button
          type="button"
          class="date-box"
          :class="{ active: picker === 'to' }"
          aria-label="结束日期"
          :aria-pressed="picker === 'to'"
          @click="focusField('to')"
        >
          <span class="date-text">{{ fmtYmd(toYmdVal) }}</span>
          <svg class="cal" viewBox="0 0 24 24" aria-hidden="true">
            <rect x="3" y="5" width="18" height="16" rx="2" />
            <path d="M16 3v4M8 3v4M3 11h18" />
          </svg>
        </button>
        <button type="button" class="apply-btn" aria-label="应用" @click="applyCustom">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M5 12.5 10 17.5 19 7" />
          </svg>
        </button>
      </div>
      <div class="cal-card" :aria-label="`${view.y}年${viewMonthLabel}`">
        <div class="cal-head">
          <button type="button" class="cal-nav" aria-label="上一步" @click="shiftHead(-1)">
            <svg viewBox="0 0 12 12" aria-hidden="true">
              <path d="M7.5 3 4 6l3.5 3" />
            </svg>
          </button>
          <div class="cal-titles">
            <button
              type="button"
              class="cal-title"
              :class="{ on: mode === 'year' }"
              aria-haspopup="grid"
              :aria-expanded="mode === 'year'"
              @click="toggleYear"
            >
              {{ view.y }}年
              <svg class="cal-caret" viewBox="0 0 12 12" aria-hidden="true">
                <path d="M3 4.5 6 8l3-3.5" />
              </svg>
            </button>
            <button
              type="button"
              class="cal-title"
              :class="{ on: mode === 'month' }"
              aria-haspopup="grid"
              :aria-expanded="mode === 'month'"
              @click="toggleMonth"
            >
              {{ viewMonthLabel }}
            </button>
          </div>
          <button
            type="button"
            class="cal-nav"
            aria-label="下一步"
            :disabled="!canNextHead"
            @click="shiftHead(1)"
          >
            <svg viewBox="0 0 12 12" aria-hidden="true">
              <path d="M4.5 3 8 6l-3.5 3" />
            </svg>
          </button>
        </div>
        <div v-if="mode === 'year'" class="cal-pick" role="grid" aria-label="选择年份">
          <button
            v-for="y in years"
            :key="y"
            type="button"
            :class="{ on: y === view.y }"
            :disabled="y > maxYear"
            @click="pickYear(y)"
          >
            {{ y }}
          </button>
        </div>
        <div v-else-if="mode === 'month'" class="cal-pick" role="grid" aria-label="选择月份">
          <button
            v-for="(label, m) in MONTHS"
            :key="label"
            type="button"
            :class="{ on: m === view.m }"
            :disabled="monthDisabled(m)"
            @click="pickMonth(m)"
          >
            {{ label }}
          </button>
        </div>
        <template v-else>
          <div class="cal-dows">
            <span v-for="w in WEEKDAYS" :key="w">{{ w }}</span>
          </div>
          <div class="cal-grid">
            <button
              v-for="c in cells"
              :key="c.ymd"
              type="button"
              class="cal-day"
              :class="{
                muted: !c.inMonth,
                today: c.today,
                bound: c.bound,
                'in-range': c.inRange,
              }"
              :disabled="c.disabled"
              :aria-label="c.ymd"
              :aria-pressed="c.bound"
              @click="pickDay(c.ymd, c.disabled)"
            >
              {{ c.day }}
            </button>
          </div>
        </template>
        <div class="cal-foot">
          <button type="button" @click="goToday">今天</button>
        </div>
      </div>
      </div>
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
  transition: background 40ms ease-out, color 40ms ease-out;
}
.seg button:hover {
  color: var(--text);
  background: rgba(255, 255, 255, 0.06);
}
.seg button.active {
  background: #fff;
  color: #111;
  font-weight: 600;
}
.seg button.active:hover {
  background: #f3f6fa;
}
.seg button.active:active {
  background: #d8dee6;
}
.custom-panel {
  position: absolute;
  z-index: 40;
  top: calc(100% + 8px);
  left: 0;
}
.custom-card {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 10px 10px 8px;
  background: #161c24;
  border: 1px solid #334155;
  border-radius: 14px;
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.55);
  user-select: none;
}
.custom-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.date-box {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  flex: 1 0 auto;
  background: var(--bg-elev-2);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 7px 10px;
  color: var(--text);
  font-size: 13px;
  font-variant-numeric: tabular-nums;
  line-height: 1.2;
  cursor: pointer;
  transition: background 40ms ease-out, border-color 40ms ease-out;
}
.date-text {
  white-space: nowrap;
}
.date-box:hover {
  border-color: #3d4b5e;
  background: #1c2430;
}
.date-box.active {
  border-color: var(--mint);
  background: #1c2430;
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
  pointer-events: none;
}
.hyphen {
  color: var(--muted);
  font-size: 14px;
}
.apply-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  flex: 0 0 28px;
  padding: 0;
  background: #fff;
  color: #111;
  border: none;
  border-radius: 999px;
  transition: background 40ms ease-out, box-shadow 40ms ease-out;
}
.apply-btn svg {
  width: 14px;
  height: 14px;
  stroke: currentColor;
  fill: none;
  stroke-width: 2.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.apply-btn:hover {
  background: #f3f6fa;
  box-shadow: 0 0 0 3px rgba(255, 255, 255, 0.16);
}
.apply-btn:active {
  background: #d8dee6;
  box-shadow: none;
}
.cal-card {
  padding: 4px 0 2px;
  color: #e7edf5;
  border-top: 1px solid #2a3544;
}
.cal-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 8px;
}
.cal-titles {
  display: flex;
  align-items: center;
  gap: 2px;
}
.cal-title {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  background: transparent;
  border: none;
  color: var(--text);
  font-size: 13px;
  font-weight: 600;
  letter-spacing: 0.04em;
  padding: 4px 8px;
  border-radius: 8px;
  transition: background 40ms ease-out, color 40ms ease-out;
}
.cal-title:hover {
  background: #243042;
}
.cal-title.on {
  background: #243042;
  color: var(--mint);
}
.cal-caret {
  width: 10px;
  height: 10px;
  stroke: currentColor;
  fill: none;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.cal-title.on .cal-caret {
  transform: rotate(180deg);
}
.cal-nav {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  padding: 0;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 8px;
  color: var(--text);
  transition: background 40ms ease-out;
}
.cal-nav:hover:not(:disabled) {
  background: #243042;
}
.cal-nav:disabled {
  opacity: 0.28;
  cursor: not-allowed;
}
.cal-nav svg {
  width: 12px;
  height: 12px;
  stroke: currentColor;
  fill: none;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.cal-dows,
.cal-grid {
  display: grid;
  grid-template-columns: repeat(7, minmax(0, 1fr));
  column-gap: 2px;
  row-gap: 6px;
}
.cal-dows {
  margin-bottom: 4px;
  row-gap: 0;
}
.cal-dows span {
  text-align: center;
  font-size: 11px;
  color: var(--muted);
  padding: 2px 0;
}
.cal-day {
  width: 100%;
  height: 30px;
  padding: 0;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
  line-height: 1;
  transition: background 40ms ease-out, color 40ms ease-out, box-shadow 40ms ease-out;
}
.cal-pick {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 6px;
  min-height: 176px;
}
.cal-pick button {
  height: 38px;
  padding: 0;
  border: none;
  border-radius: 8px;
  background: transparent;
  color: var(--text);
  font-size: 13px;
  transition: background 40ms ease-out, color 40ms ease-out;
}
.cal-pick button:hover:not(:disabled):not(.on) {
  background: #243042;
}
.cal-pick button.on {
  background: var(--mint);
  color: #0b0e12;
  font-weight: 600;
}
.cal-pick button.on:hover:not(:disabled) {
  filter: brightness(1.08);
}
.cal-pick button:disabled {
  opacity: 0.28;
  cursor: default;
}
.cal-day:hover:not(:disabled):not(.bound) {
  background: #243042;
}
.cal-day.muted {
  color: #6b7787;
}
.cal-day.in-range:not(.bound) {
  background: rgba(62, 224, 179, 0.14);
}
.cal-day.today:not(.bound) {
  box-shadow: inset 0 0 0 1px var(--mint);
  color: var(--mint);
}
.cal-day.bound {
  background: var(--mint);
  color: #0b0e12;
  font-weight: 600;
}
.cal-day.bound:hover:not(:disabled) {
  filter: brightness(1.08);
}
.cal-day:disabled {
  opacity: 0.28;
  cursor: default;
}
.cal-foot {
  display: flex;
  justify-content: flex-end;
  margin-top: 8px;
  padding-top: 8px;
  border-top: 1px solid var(--line);
}
.cal-foot button {
  background: transparent;
  border: none;
  color: var(--mint);
  font-size: 12px;
  padding: 2px 6px;
  border-radius: 6px;
  transition: background 40ms ease-out, color 40ms ease-out;
}
.cal-foot button:hover {
  background: rgba(62, 224, 179, 0.12);
  color: #f4f8fc;
}
</style>
