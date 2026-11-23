<script setup lang="ts">
// A small menu at a point (a card's More button, a right click, the popover's Send): arrow keys
// move, Return picks, Esc and a click elsewhere close it. Its items are the caller's. An item with
// `children` opens them as a submenu beside it (Send ›): → or Return opens it, ← and Esc go back,
// and a digit picks the item of the open menu whose keys are that digit.
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import { chord } from "../lib/keys";

export type MenuItem =
  | {
      label: string;
      icon: string;
      /** A picture in place of the icon: an app's own icon as a data URL. */
      image?: string | null;
      keys?: string;
      /** A word in place of the keys, such as why the item is off. */
      note?: string;
      destructive?: boolean;
      disabled?: boolean;
      run?: () => void;
      children?: readonly MenuItem[];
    }
  | "separator";

type Item = Exclude<MenuItem, "separator">;

const props = defineProps<{ items: readonly MenuItem[]; at: { x: number; y: number }; label: string }>();
const emit = defineEmits<{ close: [] }>();

const root = ref<HTMLElement | null>(null);
const subEl = ref<HTMLElement | null>(null);
const pos = ref({ left: props.at.x, top: props.at.y });
/** The open submenu: the item it belongs to and where it stands inside the menu. */
const sub = ref<{ index: number; left: number; top: number } | null>(null);

function buttons(menu: HTMLElement | null | undefined): HTMLButtonElement[] {
  return [...(menu?.querySelectorAll<HTMLButtonElement>(":scope > button:not(:disabled)") ?? [])];
}
function outside(event: MouseEvent): void {
  if (root.value && !root.value.contains(event.target as Node)) emit("close");
}
onMounted(async () => {
  await nextTick();
  const el = root.value;
  if (el) {
    const r = el.getBoundingClientRect();
    pos.value = {
      left: Math.max(8, Math.min(props.at.x, window.innerWidth - r.width - 8)),
      top: Math.max(8, props.at.y + r.height > window.innerHeight - 8 ? props.at.y - r.height - 8 : props.at.y),
    };
  }
  buttons(root.value)[0]?.focus();
  window.addEventListener("mousedown", outside, true);
  window.addEventListener("blur", close);
});
onBeforeUnmount(() => {
  window.removeEventListener("mousedown", outside, true);
  window.removeEventListener("blur", close);
});
function close(): void {
  emit("close");
}

/** Opens the submenu of the item at `index` beside it, on the side with room. */
async function openSub(index: number, button: HTMLElement, focus: boolean): Promise<void> {
  if (sub.value?.index !== index) {
    sub.value = { index, left: button.offsetLeft + button.offsetWidth - 2, top: button.offsetTop - 4 };
    await nextTick();
    const el = subEl.value;
    const menu = root.value;
    if (el && menu && sub.value) {
      const box = menu.getBoundingClientRect();
      if (box.left + sub.value.left + el.offsetWidth > window.innerWidth - 8) sub.value.left = 2 - el.offsetWidth;
      const bottom = box.top + sub.value.top + el.offsetHeight;
      if (bottom > window.innerHeight - 8) sub.value.top -= bottom - (window.innerHeight - 8);
    }
  }
  if (focus) buttons(subEl.value)[0]?.focus();
}

function closeSub(): void {
  if (!sub.value) return;
  const index = sub.value.index;
  sub.value = null;
  root.value?.querySelector<HTMLButtonElement>(`:scope > button[data-index="${index}"]`)?.focus();
}

function key(event: KeyboardEvent): void {
  const active = document.activeElement as HTMLButtonElement | null;
  const inSub = !!subEl.value && !!active && subEl.value.contains(active);
  const menu = inSub ? subEl.value : root.value;
  const list = buttons(menu);
  const index = list.indexOf(active as HTMLButtonElement);
  const own = inSub ? children.value : props.items;
  if (event.key === "Escape" || event.key === "ArrowLeft") {
    if (inSub) closeSub();
    else if (event.key === "Escape") emit("close");
    else return;
  } else if (event.key === "Tab") emit("close");
  else if (event.key === "ArrowDown") list[(index + 1) % list.length]?.focus();
  else if (event.key === "ArrowUp") list[(index - 1 + list.length) % list.length]?.focus();
  else if (event.key === "ArrowRight" && !inSub && active?.dataset.index) {
    const at = Number(active.dataset.index);
    const item = props.items[at];
    if (item === "separator" || !item?.children) return;
    children.value = item.children;
    void openSub(at, active, true);
  } else if (/^[1-9]$/.test(event.key) && !event.metaKey && !event.ctrlKey && !event.altKey) {
    const item = own.find((x): x is Item => x !== "separator" && x.keys === event.key && !x.disabled);
    if (!item) return;
    pick(item);
  } else return;
  event.preventDefault();
  event.stopPropagation();
}

const children = ref<readonly MenuItem[]>([]);

function pick(item: Item, index?: number, event?: MouseEvent): void {
  if (item.children) {
    if (index === undefined) return;
    const button = (event?.currentTarget as HTMLElement | undefined) ?? root.value?.querySelector<HTMLElement>(`:scope > button[data-index="${index}"]`);
    children.value = item.children;
    if (button) void openSub(index, button, !event || event.detail === 0);
    return;
  }
  emit("close");
  item.run?.();
}

function hover(item: Item, index: number, event: MouseEvent): void {
  if (item.children) {
    children.value = item.children;
    void openSub(index, event.currentTarget as HTMLElement, false);
  } else if (sub.value) sub.value = null;
}
</script>

<template>
  <div ref="root" class="g-menu hv-menu" role="menu" :aria-label="label" :style="{ left: `${pos.left}px`, top: `${pos.top}px` }" @keydown="key">
    <template v-for="(item, i) in items" :key="i">
      <hr v-if="item === 'separator'" class="g-menu-separator" />
      <button
        v-else
        type="button"
        class="g-menu-item"
        role="menuitem"
        :data-index="i"
        :data-variant="item.destructive ? 'destructive' : undefined"
        :aria-haspopup="item.children ? 'menu' : undefined"
        :aria-expanded="item.children ? sub?.index === i : undefined"
        :disabled="item.disabled"
        @mouseenter="hover(item, i, $event)"
        @click="pick(item, i, $event)"
      >
        <img v-if="item.image" class="hv-menu-image" :src="item.image" alt="" />
        <GIcon v-else :name="item.icon" :size="16" />
        <span style="flex: 1">{{ item.label }}</span>
        <span v-if="item.children" class="g-menu-sub"><GIcon name="chevron-right" :size="14" /></span>
        <span v-else-if="item.note" class="g-menu-key">{{ item.note }}</span>
        <kbd v-else-if="item.keys" class="g-kbd">{{ chord(item.keys) }}</kbd>
      </button>
    </template>
    <div
      v-if="sub"
      ref="subEl"
      class="g-menu hv-sub"
      role="menu"
      :aria-label="(items[sub.index] as Item).label"
      :style="{ left: `${sub.left}px`, top: `${sub.top}px` }"
    >
      <template v-for="(item, i) in children" :key="i">
        <hr v-if="item === 'separator'" class="g-menu-separator" />
        <button
          v-else
          type="button"
          class="g-menu-item"
          role="menuitem"
          :data-variant="item.destructive ? 'destructive' : undefined"
          :disabled="item.disabled"
          @click="pick(item)"
        >
          <img v-if="item.image" class="hv-menu-image" :src="item.image" alt="" />
          <GIcon v-else :name="item.icon" :size="16" />
          <span style="flex: 1">{{ item.label }}</span>
          <span v-if="item.note" class="g-menu-key">{{ item.note }}</span>
          <kbd v-else-if="item.keys" class="g-kbd">{{ chord(item.keys) }}</kbd>
        </button>
      </template>
    </div>
  </div>
</template>

<style scoped>
.hv-menu {
  position: fixed;
  z-index: 40;
}
.hv-sub {
  position: absolute;
  z-index: 1;
  min-width: 220px;
}
.hv-menu-image {
  flex: none;
  width: 16px;
  height: 16px;
}
.g-menu-item:disabled > .hv-menu-image {
  opacity: 0.4;
  filter: grayscale(1);
}
</style>
