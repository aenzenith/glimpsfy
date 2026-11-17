<script setup lang="ts">
// The inspector: preview, name, the two main actions, the icon row, the facts, the link and the
// recognised text, in the order of the work. During a search the text moves up under the
// actions, every match is outlined in the preview, and ⌘G and ⇧⌘G step through them.
import { computed, inject, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import { entryName, fileUrl, thumbUrl, type Entry, type HistoryItem } from "../lib/api/history";
import type { HistoryRetention } from "../lib/settings";
import { t } from "../lib/i18n";
import AppIcon from "./AppIcon.vue";
import { HISTORY, type HistoryContext } from "./context";
import { matchRanges } from "./fold";
import Marked from "./Marked.vue";
import { extensionOf, fileName, formatBytes, formatDuration, formatWhen, mediaPath } from "./model";
import Player from "./Player.vue";
import Shot from "./Shot.vue";

const props = defineProps<{
  item: HistoryItem;
  /** The entry with its text, once `history_get` answered. */
  full: Entry | null;
  overlay: boolean;
  current: number;
  retention: HistoryRetention | null;
}>();
const emit = defineEmits<{ close: []; step: [by: number] }>();
const ctx = inject(HISTORY) as HistoryContext;

const entry = computed(() => props.full ?? props.item.entry);
const id = computed(() => entry.value.id);
const kind = computed(() => entry.value.kind);
const match = computed(() => props.item.textMatch);
const searching = computed(() => !!ctx.query.value && !!match.value);
const boxes = computed(() => (searching.value ? (match.value?.boxes ?? []) : []));
const words = computed(() => ({ today: t("history.today"), yesterday: t("history.yesterday") }));
const lang = ctx.lang;

// ---------------------------------------------------------------- name
// An entry without a name of its own shows the one the window gives it, and stays unnamed until
// the user types another.
const shownName = computed(() => entryName(entry.value));
const name = ref(shownName.value);
watch(
  () => [id.value, shownName.value],
  () => {
    name.value = shownName.value;
  },
);
function commit(): void {
  const next = name.value.trim();
  if (!next) name.value = shownName.value;
  else if (next !== shownName.value) ctx.act.rename(id.value, next);
}
function nameKey(event: KeyboardEvent): void {
  if (event.key === "Enter") {
    event.preventDefault();
    (event.target as HTMLInputElement).blur();
  } else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    name.value = shownName.value;
    (event.target as HTMLInputElement).blur();
  }
}

// ---------------------------------------------------------------- facts
const where = computed(() => {
  const saved = entry.value.savedPath;
  if (!saved) return t("history.fact.inHistoryOnly");
  const parts = saved.split("/").filter(Boolean);
  return parts.slice(-3, -1).join(" › ") || saved;
});
const source = computed(() => {
  const s = entry.value.source;
  if (s.appName && s.windowTitle && s.windowTitle !== s.appName) return `${s.appName} · ${s.windowTitle}`;
  return s.appName ?? s.windowTitle ?? "—";
});
const keepLine = computed(() => {
  if (entry.value.pinned) return t("history.fact.keptPinned");
  switch (props.retention) {
    case "days7":
      return t("history.fact.removedAfter", { days: 7 });
    case "days30":
      return t("history.fact.removedAfter", { days: 30 });
    case "days90":
      return t("history.fact.removedAfter", { days: 90 });
    default:
      return t("history.fact.keptUntilDeleted");
  }
});

// ---------------------------------------------------------------- text
const textOpen = ref(true);
const lines = computed(() => {
  const e = props.full;
  if (!e) return [];
  if (e.textLines?.lines.length) return e.textLines.lines.map((l) => l.text);
  return (e.text ?? "").split("\n").filter((l) => l.length > 0);
});
const textState = computed(() => entry.value.textState);
const matchText = computed(() => {
  const n = boxes.value.length;
  return n ? t("history.match", { n }).split("{i}") : [];
});

// ---------------------------------------------------------------- link
const hosted = computed(() => entry.value.hosted);
const uploadedLine = computed(() => {
  const h = hosted.value;
  if (!h) return "";
  const when = formatWhen(h.uploadedAtMs, ctx.now.value, lang.value, words.value);
  const base = h.deletion ? t("history.link.uploadedWithDeletion", { when }) : t("history.link.uploaded", { when });
  return h.expiresAtMs ? `${base} ${t("history.link.expires", { when: formatWhen(h.expiresAtMs, ctx.now.value, lang.value, words.value) })}` : base;
});

const path = computed(() => mediaPath(entry.value));
/** An upload the history never saw the pixels of (uploaded straight from the overlay): only the
 * link was kept, so the file actions have nothing to act on. */
const linkOnly = computed(() => kind.value !== "textCopy" && !path.value && !!hosted.value);
const preview = computed(() => (linkOnly.value ? null : (thumbUrl(entry.value) ?? (kind.value === "screenshot" ? fileUrl(id.value) : null))));
</script>

<template>
  <aside class="hv-panel" :aria-label="t('history.inspector.label')" :data-mode="overlay ? 'overlay' : undefined">
    <div v-if="overlay" class="hv-panel-head">
      <span class="hv-sec-title">{{ t("history.inspector.title") }}</span>
      <button type="button" class="g-btn" data-size="small" :aria-label="t('history.inspector.close')" :data-tip="t('history.inspector.closeShort')" data-key="Esc" @click="emit('close')">
        <GIcon name="close" :size="16" />
      </button>
    </div>

    <div class="hv-psec hv-psec-top">
      <Player v-if="kind === 'recording' && !linkOnly" :src="fileUrl(id)" :poster="thumbUrl(entry)" />
      <div v-else-if="kind === 'screenshot' || linkOnly" class="hv-preview">
        <Shot :src="preview" :size="match?.imageSize ?? entry.size" mode="contain" :boxes="boxes" :current="boxes.length ? current : null" all>
          <template #missing>
            <div class="hv-placeholder"><GIcon name="image" :size="32" /></div>
          </template>
        </Shot>
      </div>
      <div v-if="matchText.length" class="hv-matchnav">
        <span>{{ matchText[0] }}<b>{{ current + 1 }}</b>{{ matchText[1] ?? "" }}</span>
        <button type="button" class="g-btn" data-size="small" :aria-label="t('history.match.prev')" :data-tip="t('history.match.prev')" data-key="⇧⌘G" @click="emit('step', -1)">
          <GIcon name="chevron-up" :size="16" />
        </button>
        <button type="button" class="g-btn" data-size="small" :aria-label="t('history.match.next')" :data-tip="t('history.match.next')" data-key="⌘G" @click="emit('step', 1)">
          <GIcon name="chevron-down" :size="16" />
        </button>
      </div>
      <input v-model="name" class="hv-name" :aria-label="t('history.inspector.name')" spellcheck="false" maxlength="500" @keydown="nameKey" @blur="commit" />
      <div class="hv-file">
        <bdi v-if="kind === 'textCopy'">{{ t("history.inspector.copiedFrom", { app: entry.source.appName ?? "—" }) }}</bdi>
        <span v-else-if="linkOnly">{{ t("history.inspector.linkOnly") }}</span>
        <bdi v-else>{{ fileName(path) }}</bdi>
      </div>
      <div class="hv-actions">
        <template v-if="linkOnly">
          <button type="button" class="g-button" data-variant="primary" :data-tip="t('history.action.copyLink')" data-key="⇧⌘L" @click="ctx.act.copyLink(id)">
            <GIcon name="link" :size="16" />{{ t("history.action.copyLink") }}
          </button>
          <button type="button" class="g-button" :data-tip="t('history.action.openLink')" @click="ctx.act.openLink(id)">
            <GIcon name="external-link" :size="16" />{{ t("history.action.openLink") }}
          </button>
        </template>
        <template v-else-if="kind === 'textCopy'">
          <button type="button" class="g-button" data-variant="primary" :data-tip="t('history.action.copyText')" data-key="⇧⌘C" @click="ctx.act.copyText(id)">
            <GIcon name="copy" :size="16" />{{ t("history.action.copyText") }}
          </button>
        </template>
        <template v-else-if="kind === 'recording'">
          <button type="button" class="g-button" data-variant="primary" :data-tip="t('history.action.trim')" data-key="⌘T" @click="ctx.act.trim(id, 'mp4')">
            <GIcon name="trim" :size="16" />{{ t("history.action.trim") }}
          </button>
          <button type="button" class="g-button" :data-tip="t('history.action.copy')" data-key="⌘C" @click="ctx.act.copy(id)">
            <GIcon name="copy" :size="16" />{{ t("history.action.copy") }}
          </button>
        </template>
        <template v-else>
          <button type="button" class="g-button" data-variant="primary" :data-tip="t('history.action.open')" data-key="⌘O" @click="ctx.act.open(id)">
            <GIcon name="external-link" :size="16" />{{ t("history.action.open") }}
          </button>
          <button type="button" class="g-button" :data-tip="t('history.action.copy')" data-key="⌘C" @click="ctx.act.copy(id)">
            <GIcon name="copy" :size="16" />{{ t("history.action.copy") }}
          </button>
        </template>
      </div>
      <div class="hv-iconrow" role="toolbar" :aria-label="t('history.inspector.more')">
        <template v-if="!linkOnly">
        <button v-if="kind !== 'textCopy'" type="button" class="g-btn" :aria-label="t('history.action.saveAs')" :data-tip="t('history.action.saveAs')" data-key="⇧⌘S" @click="ctx.act.saveAs(id)">
          <GIcon name="save-as" />
        </button>
        <button v-if="kind !== 'textCopy'" type="button" class="g-btn" :aria-label="t('history.action.upload')" :data-tip="t('history.action.upload')" data-key="⌘U" @click="ctx.act.upload([id])">
          <GIcon name="upload" />
        </button>
        <button v-if="kind === 'screenshot'" type="button" class="g-btn" :aria-label="t('history.action.pinToScreen')" :data-tip="t('history.action.pinToScreen')" data-key="⌘P" @click="ctx.act.pinToScreen(id)">
          <GIcon name="pin" />
        </button>
        <button v-if="kind === 'recording'" type="button" class="g-btn" :aria-label="t('history.action.gif')" :data-tip="t('history.action.gif')" data-key="⇧⌘E" @click="ctx.act.trim(id, 'gif')">
          <GIcon name="gif" />
        </button>
        <button v-if="kind !== 'textCopy'" type="button" class="g-btn" :aria-label="t('history.action.reveal')" :data-tip="t('history.action.reveal')" data-key="⌥⌘R" @click="ctx.act.reveal(id)">
          <GIcon name="folder" />
        </button>
        </template>
        <button type="button" class="g-btn" :aria-label="t('history.action.delete')" :data-tip="t('history.action.delete')" data-key="⌘⌫" @click="ctx.act.remove([id])">
          <GIcon name="trash" />
        </button>
      </div>
    </div>

    <template v-for="section in searching ? ['text', 'facts', 'link'] : ['facts', 'link', 'text']" :key="section">
      <!-- facts -->
      <div v-if="section === 'facts'" class="hv-psec">
        <dl class="hv-props">
          <template v-if="kind === 'screenshot'">
            <dt>{{ t("history.fact.dimensions") }}</dt>
            <dd>
              <span class="g-mono">{{ entry.size ? `${entry.size.width} × ${entry.size.height}` : "—" }}</span>
              <span v-if="entry.size" class="g-muted">{{ entry.scale && entry.scale > 1 ? `px · @${entry.scale}x` : "px" }}</span>
            </dd>
          </template>
          <template v-if="kind === 'recording'">
            <dt>{{ t("history.fact.duration") }}</dt>
            <dd><span class="g-mono">{{ formatDuration(entry.durationMs) }}</span></dd>
            <dt>{{ t("history.fact.resolution") }}</dt>
            <dd><span class="g-mono">{{ entry.size ? `${entry.size.width} × ${entry.size.height}` : "—" }}</span></dd>
          </template>
          <template v-if="kind === 'textCopy'">
            <dt>{{ t("history.fact.length") }}</dt>
            <dd><span>{{ entry.textLineCount === 1 ? t("history.lines.one") : t("history.lines", { n: entry.textLineCount }) }}</span></dd>
          </template>
          <template v-else>
            <dt>{{ t("history.fact.size") }}</dt>
            <dd><span>{{ formatBytes(entry.fileBytes, lang) }} · {{ extensionOf(entry) }}</span></dd>
          </template>
          <dt>{{ kind === "textCopy" ? t("history.fact.copied") : t("history.fact.captured") }}</dt>
          <dd><span>{{ formatWhen(entry.createdAtMs, ctx.now.value, lang, words) }}</span></dd>
          <dt>{{ t("history.fact.source") }}</dt>
          <dd>
            <AppIcon v-if="entry.source.appName" :name="entry.source.appName" :size="14" />
            <span>{{ source }}</span>
          </dd>
          <template v-if="entry.display?.name">
            <dt>{{ t("history.fact.display") }}</dt>
            <dd><span>{{ entry.display.name }}</span></dd>
          </template>
          <template v-if="kind !== 'textCopy'">
            <dt>{{ t("history.fact.location") }}</dt>
            <dd><span>{{ where }}</span></dd>
          </template>
          <dt>{{ t("history.fact.kept") }}</dt>
          <dd>
            <span>{{ keepLine }}</span>
          </dd>
        </dl>
        <button type="button" class="g-button" data-variant="plain" style="margin-top: 8px" @click="ctx.act.setPinned(id, !entry.pinned)">
          <GIcon name="pin" :size="16" />{{ entry.pinned ? t("history.action.unpin") : t("history.action.pin") }}
        </button>
      </div>

      <!-- link -->
      <div v-else-if="section === 'link' && kind !== 'textCopy'" class="hv-psec">
        <template v-if="hosted">
          <div class="hv-sec-head">
            <span class="hv-sec-title">{{ t("history.link.title") }}</span>
            <span class="g-badge" data-tone="ok"><GIcon name="cloud" :size="12" />{{ ctx.hostName(hosted.provider) }}</span>
          </div>
          <div class="hv-linkrow">
            <input class="g-field" data-kind="mono" readonly :value="hosted.url" :aria-label="t('history.link.title')" />
            <button type="button" class="g-btn" data-size="small" :aria-label="t('history.action.copyLink')" :data-tip="t('history.action.copyLink')" data-key="⇧⌘L" @click="ctx.act.copyLink(id)">
              <GIcon name="copy" :size="16" />
            </button>
            <button type="button" class="g-btn" data-size="small" :aria-label="t('history.action.openLink')" :data-tip="t('history.action.openLink')" @click="ctx.act.openLink(id)">
              <GIcon name="external-link" :size="16" />
            </button>
          </div>
          <div class="hv-linkmeta"><span>{{ uploadedLine }}</span></div>
          <div style="margin-top: 10px">
            <button type="button" class="g-button" data-variant="destructive" @click="ctx.act.deleteHosted(id)">
              <GIcon name="cloud" :size="16" />{{ t("history.action.deleteHosted") }}
            </button>
          </div>
        </template>
        <template v-else>
          <div class="hv-sec-head"><span class="hv-sec-title">{{ t("history.link.title") }}</span></div>
          <div class="hv-linkmeta">
            <span>{{ t("history.link.notUploaded") }}</span>
            <button type="button" class="g-button" :data-tip="t('history.action.upload')" data-key="⌘U" @click="ctx.act.upload([id])">
              <GIcon name="upload" :size="16" />{{ t("history.action.upload") }}
            </button>
          </div>
        </template>
      </div>

      <!-- text -->
      <div v-else-if="section === 'text' && kind !== 'recording'" class="hv-psec">
        <div class="hv-sec-head">
          <button type="button" class="hv-disclose" :aria-expanded="textOpen" @click="textOpen = !textOpen">
            <GIcon name="chevron-down" :size="16" />
            <span class="hv-sec-title">{{ kind === "textCopy" ? t("history.text.copiedTitle") : t("history.text.title") }}</span>
            <span v-if="lines.length" class="hv-count">{{ lines.length === 1 ? t("history.lines.one") : t("history.lines", { n: lines.length }) }}</span>
          </button>
          <button
            v-if="lines.length"
            type="button"
            class="g-button"
            data-variant="plain"
            :data-tip="t('history.action.copyText')"
            data-key="⇧⌘C"
            @click="ctx.act.copyText(id)"
          >
            <GIcon name="copy" :size="16" />{{ t("history.action.copy") }}
          </button>
        </div>
        <template v-if="textOpen">
          <div v-if="lines.length" class="hv-text" tabindex="0" role="document" :aria-label="t('history.text.title')">
            <div v-for="(line, i) in lines" :key="i"><Marked :text="line" :ranges="ctx.query.value ? matchRanges(line, ctx.query.value) : []" /></div>
          </div>
          <p v-else-if="!full" class="g-help">{{ t("history.text.loading") }}</p>
          <p v-else-if="textState === 'pending'" class="g-help">{{ t("history.text.pending") }}</p>
          <p v-else-if="textState === 'failed'" class="g-help" data-tone="warn">{{ t("history.text.failed") }}</p>
          <p v-else class="g-help">{{ t("history.text.none") }}</p>
          <div v-if="lines.length && kind === 'screenshot'" class="hv-text-meta">{{ t("history.text.readHere") }}</div>
        </template>
      </div>

      <!-- a recording's note -->
      <div v-else-if="section === 'text' && kind === 'recording'" class="hv-psec">
        <p class="g-help" style="margin: 0">{{ t("history.text.recordingNote") }}</p>
      </div>
    </template>
  </aside>
</template>
