<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'

const CLOSE_EVT = 'ai-usage:close-filters'

const props = withDefaults(
  defineProps<{
    label: string
    modelValue: string
    options: { value: string; label: string }[]
    allValue?: string
    allLabel?: string
  }>(),
  { allValue: '', allLabel: '全部' },
)

const emit = defineEmits<{ 'update:modelValue': [string] }>()

const wrap = ref<HTMLElement | null>(null)
const open = ref(false)

const items = computed(() => [
  { value: props.allValue, label: props.allLabel },
  ...props.options,
])

const current = computed(() => {
  if (props.modelValue === props.allValue) return props.allLabel
  return props.options.find((o) => o.value === props.modelValue)?.label ?? props.allLabel
})

function close() {
  open.value = false
}

function toggle() {
  const next = !open.value
  document.dispatchEvent(new Event(CLOSE_EVT))
  open.value = next
}

function pick(value: string) {
  emit('update:modelValue', value)
  close()
}

function onDoc(ev: PointerEvent) {
  if (!open.value) return
  const el = wrap.value
  if (el && !el.contains(ev.target as Node)) close()
}

function onKey(ev: KeyboardEvent) {
  if (ev.key === 'Escape') close()
}

onMounted(() => {
  document.addEventListener(CLOSE_EVT, close)
  document.addEventListener('pointerdown', onDoc)
  document.addEventListener('keydown', onKey)
})
onUnmounted(() => {
  document.removeEventListener(CLOSE_EVT, close)
  document.removeEventListener('pointerdown', onDoc)
  document.removeEventListener('keydown', onKey)
})
</script>

<template>
  <div ref="wrap" class="filter-dd" :class="{ open }">
    <button
      type="button"
      class="filter-pill"
      :aria-label="label"
      aria-haspopup="listbox"
      :aria-expanded="open"
      @click="toggle"
    >
      <span class="fp-icon" aria-hidden="true">
        <slot />
      </span>
      <span class="fp-text">
        <span class="fp-k">{{ label }}</span>
        <span class="fp-v">{{ current }}</span>
      </span>
      <svg class="fp-chev" viewBox="0 0 12 12" aria-hidden="true">
        <path d="M3 4.5 L6 8 L9 4.5" />
      </svg>
    </button>
    <ul v-if="open" class="filter-menu" role="listbox" :aria-label="label">
      <li
        v-for="it in items"
        :key="it.value || '__all__'"
        role="option"
        :aria-selected="it.value === modelValue"
        :class="{ selected: it.value === modelValue }"
        @click="pick(it.value)"
      >
        <span class="box" :class="{ on: it.value === modelValue }" aria-hidden="true">
          <svg v-if="it.value === modelValue" viewBox="0 0 12 12">
            <path d="M2 6.2 L4.8 9 L10 3.2" />
          </svg>
        </span>
        <span class="opt">{{ it.label }}</span>
      </li>
    </ul>
  </div>
</template>

<style scoped>
.filter-dd {
  position: relative;
}
.filter-pill {
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--bg-elev-2);
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 6px 12px 6px 10px;
  color: var(--text);
  cursor: pointer;
  min-width: 0;
  line-height: 1.2;
}
.filter-dd.open .filter-pill {
  border-color: #3d4b5e;
  background: #1c2430;
}
.fp-icon {
  display: flex;
  width: 16px;
  height: 16px;
  color: var(--muted);
  flex: 0 0 16px;
}
.fp-icon :deep(svg) {
  width: 16px;
  height: 16px;
  stroke: currentColor;
  fill: none;
  stroke-width: 1.5;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.fp-text {
  display: flex;
  align-items: baseline;
  gap: 6px;
  min-width: 0;
  font-size: 13px;
}
.fp-k { color: var(--muted); }
.fp-v {
  color: var(--text);
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.fp-chev {
  width: 10px;
  height: 10px;
  flex: 0 0 10px;
  stroke: var(--muted);
  fill: none;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
  transition: transform 0.12s ease;
}
.filter-dd.open .fp-chev {
  transform: rotate(180deg);
  stroke: var(--text);
}
.filter-menu {
  position: absolute;
  z-index: 40;
  top: calc(100% + 6px);
  right: 0;
  min-width: max(100%, 200px);
  max-width: 280px;
  max-height: 280px;
  overflow: auto;
  margin: 0;
  padding: 6px;
  list-style: none;
  background: #161c24;
  color: #e7edf5;
  border: 1px solid #334155;
  border-radius: 12px;
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.55);
  scrollbar-width: none;
  -ms-overflow-style: none;
}
.filter-menu::-webkit-scrollbar {
  width: 0;
  height: 0;
  display: none;
}
.filter-menu li {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 10px;
  border-radius: 8px;
  color: #d5dee8;
  font-size: 13px;
  cursor: pointer;
  user-select: none;
}
.filter-menu li:hover {
  background: #243042;
  color: #f4f8fc;
}
.filter-menu li.selected {
  color: #e7edf5;
}
.box {
  width: 15px;
  height: 15px;
  flex: 0 0 15px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: 1.5px solid #6b7787;
  border-radius: 3px;
  background: transparent;
}
.box.on {
  border-color: #3ee0b3;
  background: #3ee0b3;
}
.box svg {
  width: 10px;
  height: 10px;
  stroke: #0b0e12;
  fill: none;
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.opt {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
