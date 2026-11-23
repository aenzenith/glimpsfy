// The text actions of the text layer (A3): the phone numbers, addresses, links and dates found in
// the recognised text, their underlines, the keyboard focus that walks them and the menu that acts
// on one. `TextLayer.vue` draws what this returns and hands it the keys and the clicks first.
//
// Finding runs in the core (`ocr_detect_entities`) on the text alone, after every recognition.
// An action runs only on a click or its key: nothing opens, previews or asks the network by
// itself. An action that brings another app forward goes through `leave` when the page gives one
// (the overlay closes or hands off its capture first); copying never does.

import { computed, onBeforeUnmount, onMounted, ref, shallowRef, watch, type Ref } from "vue";

import type { Bounds, Entity, OcrResult } from "../lib/api/ocr";
import { ocr_detect_entities } from "../lib/api/ocr";
import { app_add_to_calendar, app_can_open_url, app_open_url } from "../lib/api/windows";
import { language, t } from "../lib/i18n";
import { commandHeld } from "../lib/keys";
import type { MenuRow } from "./EntityMenu.vue";
import { ACTION_KINDS, actionOf, dateTitle, eventOf } from "./entities";
import type { Rect } from "./text";

type Options = {
  result: Readonly<Ref<OcrResult | null>>;
  shown: Readonly<Ref<boolean>>;
  /** Maps a box of the recognised image into CSS pixels of the layer. */
  place: Readonly<Ref<((b: Bounds, pad?: number) => Rect) | null>>;
  /** The recognised area in CSS pixels of the layer; the menu goes above its line rather than
   * past the area's bottom, where the layer's chip sits. */
  area: Readonly<Ref<Rect | null>>;
  /** The layer's root, whose box the menu stays inside. */
  root: Readonly<Ref<HTMLElement | null>>;
  /** Copies text with the layer's own toast. */
  copy: (text: string) => Promise<void>;
  /** Runs an action that leaves the page, or `null` to run it in place. */
  leave: () => ((run: () => Promise<void>) => void) | null;
};

/** A press that moves this far is a drag that selects text, not a click on a piece. */
const CLICK_SLOP = 3;

export function useEntities(o: Options) {
  const entities = shallowRef<Entity[]>([]);
  const focused = ref<number | null>(null);
  const menuOpen = ref(false);
  const active = ref<MenuRow>("main");
  const canOpen = ref<boolean | null>(null);
  const failed = ref(false);
  const hovering = ref(false);

  function reset(): void {
    focused.value = null;
    menuOpen.value = false;
    failed.value = false;
  }

  let asked = 0;
  watch(
    o.result,
    async (result) => {
      const mine = ++asked;
      reset();
      entities.value = [];
      if (!result || !result.text.trim()) return;
      try {
        const found = await ocr_detect_entities(result, ACTION_KINDS);
        if (mine === asked) entities.value = found;
      } catch (error) {
        console.warn("[glimpsfy] ocr_detect_entities", error);
      }
    },
    { immediate: true },
  );
  watch(o.shown, (visible) => visible || reset());

  /** Each piece's boxes and its lines' boxes in CSS pixels of the layer. */
  const pieces = computed(() => {
    const p = o.place.value;
    const result = o.result.value;
    if (!p || !result) return [];
    return entities.value.map((entity) => ({
      entity,
      boxes: entity.boxes.map((b) => p(b.bounds)),
      lines: entity.boxes.map((b) => p(result.lines[b.line]?.bounds ?? b.bounds)),
    }));
  });

  /** The 2 px iris line under every piece, inside its line's outline. */
  const underlines = computed(() =>
    pieces.value.flatMap(({ boxes }) => boxes.map((b) => ({ x: b.x, y: Math.round(b.y + b.height - 1), width: b.width, height: 2 }))),
  );

  /** The tint over the piece the keyboard is on or the menu acts on. */
  const tints = computed(() => {
    const piece = focused.value === null ? null : pieces.value[focused.value];
    return piece ? piece.boxes.map((b) => ({ x: b.x - 1, y: b.y - 1, width: b.width + 2, height: b.height + 2 })) : [];
  });

  const menu = computed(() => {
    const piece = menuOpen.value && focused.value !== null ? pieces.value[focused.value] : null;
    const root = o.root.value;
    if (!piece || !root) return null;
    const { entity } = piece;
    const action = actionOf(entity);
    const value = entity.date ? dateTitle(entity.date, language.value) : entity.kind === "phone" ? entity.text.trim() : entity.value;
    const noApp = action.url !== null && canOpen.value === false;
    const line = piece.lines[0];
    const area = o.area.value;
    return {
      action,
      value,
      note: entity.date && !entity.date.allDay && entity.date.endAssumed ? t("ocr.action.oneHour") : null,
      disabled: noApp,
      problem: failed.value ? t("ocr.action.failed") : noApp ? t(action.noApp) : null,
      active: active.value,
      // The line's outline, as the layer draws it: 3 px across and 2 px up and down.
      line: { x: line.x - 3, y: line.y - 2, width: line.width + 6, height: line.height + 4 },
      x: piece.boxes[0].x,
      room: { width: root.clientWidth, height: root.clientHeight },
      floor: area ? area.y + area.height : null,
    };
  });

  function open(index: number): void {
    const entity = entities.value[index];
    if (!entity) return;
    focused.value = index;
    menuOpen.value = true;
    failed.value = false;
    active.value = "main";
    canOpen.value = null;
    const url = actionOf(entity).url;
    if (!url) return;
    void app_can_open_url(url)
      .then((can) => {
        if (focused.value !== index || !menuOpen.value) return;
        canOpen.value = can;
        if (!can) active.value = "copy";
      })
      .catch((error) => console.warn("[glimpsfy] app_can_open_url", error));
  }

  function close(): void {
    menuOpen.value = false;
    failed.value = false;
  }

  async function run(row: MenuRow): Promise<void> {
    const entity = focused.value === null ? null : entities.value[focused.value];
    const result = o.result.value;
    if (!entity || !result) return;
    const action = actionOf(entity);
    if (row === "copy") {
      close();
      return o.copy(action.copyText);
    }
    if (action.url === null && !entity.date) return;
    const go = async () => {
      if (action.url !== null) return app_open_url(action.url);
      const event = eventOf(entity, result);
      if (event) await app_add_to_calendar(event);
    };
    const leave = o.leave();
    if (leave) {
      reset();
      return leave(go);
    }
    try {
      await go();
      close();
    } catch (error) {
      console.warn("[glimpsfy] text action", error);
      failed.value = true;
      active.value = "copy";
    }
  }

  /** The piece under a point in CSS pixels of the layer: its box widened by 2 px, as tall as its
   * line's outline. */
  function pieceAt(x: number, y: number): number | null {
    const index = pieces.value.findIndex(({ boxes, lines }) =>
      boxes.some((b, i) => x >= b.x - 2 && x <= b.x + b.width + 2 && y >= lines[i].y - 2 && y <= lines[i].y + lines[i].height + 2),
    );
    return index >= 0 ? index : null;
  }

  function local(e: PointerEvent): { x: number; y: number } | null {
    const base = o.root.value?.getBoundingClientRect();
    return base ? { x: e.clientX - base.left, y: e.clientY - base.top } : null;
  }

  let pressed: { x: number; y: number } | null = null;

  function press(e: PointerEvent): void {
    pressed = e.button === 0 ? { x: e.clientX, y: e.clientY } : null;
  }

  /** A click that did not move opens the piece's menu; a drag was a selection and is left alone. */
  function release(e: PointerEvent): void {
    const from = pressed;
    pressed = null;
    if (!from || Math.hypot(e.clientX - from.x, e.clientY - from.y) > CLICK_SLOP) return;
    const at = local(e);
    const index = at ? pieceAt(at.x, at.y) : null;
    if (index === null) return reset();
    document.getSelection()?.removeAllRanges();
    open(index);
  }

  function move(e: PointerEvent): void {
    const at = local(e);
    hovering.value = at !== null && pieceAt(at.x, at.y) !== null;
  }

  /** A press anywhere but the menu and the text lets go of the piece; the text's own presses
   * decide on release. */
  function onWindowPress(e: PointerEvent): void {
    const target = e.target instanceof Element ? e.target : null;
    if (focused.value !== null && !target?.closest(".ocr-menu, .ocr-text")) reset();
  }
  onMounted(() => window.addEventListener("pointerdown", onWindowPress, true));
  onBeforeUnmount(() => window.removeEventListener("pointerdown", onWindowPress, true));

  /** The pieces' keys, asked before the layer's and the page's own: Tab and ⇧Tab walk the pieces
   * in reading order (Tab belongs to the layer while it shows), Return opens the menu and then runs
   * its row, ↑↓ pick the row, ⌘C copies the value and Esc closes only the menu. Answers whether it
   * took the key. */
  function onKey(e: KeyboardEvent): boolean {
    if (!o.shown.value) return false;
    const plain = !e.metaKey && !e.ctrlKey && !e.altKey;
    if (e.key === "Tab" && plain) {
      const count = entities.value.length;
      if (count) {
        const step = e.shiftKey ? -1 : 1;
        const from = focused.value ?? (e.shiftKey ? 0 : -1);
        focused.value = (from + step + count) % count;
        close();
      }
      return true;
    }
    if (e.key === "Enter" && plain && !e.shiftKey) {
      if (menuOpen.value) {
        void run(active.value);
        return true;
      }
      if (focused.value !== null) {
        open(focused.value);
        return true;
      }
      return false;
    }
    if (!menuOpen.value) return false;
    if (e.key === "Escape") {
      close();
      return true;
    }
    if ((e.key === "ArrowDown" || e.key === "ArrowUp") && plain) {
      const mainOff = menu.value?.disabled ?? false;
      active.value = active.value === "main" || mainOff ? "copy" : "main";
      return true;
    }
    if (commandHeld(e) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "c") {
      void run("copy");
      return true;
    }
    return false;
  }

  return { underlines, tints, menu, hovering, active, run, press, release, move, onKey };
}
