<script setup lang="ts">
// The first-upload confirmation (artboard 6 of the settings prototype): before anything goes to a
// host for the first time, it names the domain, who runs the host, who can open the file and how
// long it stays, from what docs/PROVIDERS.md recorded, or says plainly that the host does not
// state it. Upload records the confirmation with `upload_confirm_provider` and only then emits
// `confirmed`; Cancel and Esc record nothing. With `purpose: "test"` it confirms the connection
// test of Settings › Upload, which sends a request as well.
//
// It is the kit's `g-scrim` and `g-dialog`, modal: focus starts on Cancel and Tab stays among its
// buttons. The window's root must be positioned, because the scrim covers its nearest positioned
// ancestor.
import { computed, nextTick, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import { ApiError } from "../lib/api/core";
import { upload_confirm_provider, type ConsentInfo } from "../lib/api/upload";
import { app_open_url } from "../lib/api/windows";
import { t } from "../lib/i18n";
import { consentSentences, formatBytes, formatDay, type TestKind } from "./text";

/** The file the upload sends, for the box under the summary. */
export type DialogFile = {
  name: string;
  /** A URL the page can load: a data URL, a blob URL or a history thumbnail. */
  thumbnail: string | null;
  width: number | null;
  height: number | null;
  /** "PNG", "MP4"… */
  format: string | null;
  bytes: number | null;
};

const props = withDefaults(
  defineProps<{
    consent: ConsentInfo;
    file?: DialogFile | null;
    purpose?: "upload" | "test";
    test?: TestKind;
    /** Shows "Use another host…" for a page that can offer the host list. */
    canChooseHost?: boolean;
  }>(),
  { file: null, purpose: "upload", test: "image", canChooseHost: true },
);
const emit = defineEmits<{
  /** The confirmation is recorded; the page starts the upload or the test now. */
  confirmed: [consent: ConsentInfo];
  cancel: [];
  anotherHost: [];
  /** The settings point elsewhere by now: the page asks `upload_preflight` again. */
  stale: [];
}>();

const dialog = ref<HTMLElement | null>(null);
const cancelButton = ref<HTMLButtonElement | null>(null);
const busy = ref(false);
const problem = ref<string | null>(null);
const titleId = `upload-consent-${Math.random().toString(36).slice(2)}`;
const textId = `${titleId}-text`;

const title = computed(() =>
  props.purpose === "test"
    ? t("upload.consent.titleTest", { name: props.consent.name })
    : t("upload.consent.title", { name: props.consent.name }),
);
const body = computed(() => consentSentences(props.consent, props.purpose, props.test).join(" "));
const checked = computed(() =>
  props.consent.checkedOn ? t("upload.consent.checked", { name: props.consent.name, date: formatDay(props.consent.checkedOn) }) : null,
);
// A destination the user typed in (S3, a custom server, another 0x0 instance) is confirmed with
// its address, so changing the address asks again.
const note = computed(() => (props.consent.consentKey.includes("@") ? t("upload.consent.noteAddress") : t("upload.consent.note")));
const fileMeta = computed(() => {
  const file = props.file;
  if (!file) return "";
  const parts: string[] = [];
  if (file.width && file.height) parts.push(`${file.width} × ${file.height}`);
  if (file.format) parts.push(file.format);
  if (file.bytes != null) parts.push(formatBytes(file.bytes));
  return parts.join(" · ");
});

onMounted(() => nextTick(() => cancelButton.value?.focus()));

async function confirm(): Promise<void> {
  if (busy.value) return;
  busy.value = true;
  problem.value = null;
  try {
    const consent = await upload_confirm_provider(props.consent.provider, props.consent.consentKey);
    emit("confirmed", consent);
  } catch (error) {
    if (error instanceof ApiError && error.code === "invalid_argument") {
      problem.value = t("upload.consent.changed");
      emit("stale");
    } else {
      problem.value = t("upload.failed.other");
    }
  } finally {
    busy.value = false;
  }
}

/** The host's terms open in the browser, never inside the app's window. */
function openTerms(): void {
  if (props.consent.termsUrl) void app_open_url(props.consent.termsUrl).catch(() => undefined);
}

function key(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    event.preventDefault();
    if (!busy.value) emit("cancel");
    return;
  }
  if (event.key !== "Tab" || !dialog.value) return;
  event.preventDefault();
  const targets = [...dialog.value.querySelectorAll<HTMLElement>("button:not(:disabled), a[href]")];
  const index = targets.indexOf(document.activeElement as HTMLElement);
  targets[(index + (event.shiftKey ? -1 : 1) + targets.length) % targets.length]?.focus();
}
</script>

<template>
  <div class="g-scrim" @keydown="key">
    <div
      ref="dialog"
      class="g-dialog"
      role="alertdialog"
      aria-modal="true"
      :aria-labelledby="titleId"
      :aria-describedby="textId"
      :aria-busy="busy"
    >
      <div class="g-dialog-head">
        <span class="g-dialog-icon"><GIcon name="upload" :size="24" /></span>
        <div>
          <h2 :id="titleId" class="g-dialog-title">{{ title }}</h2>
          <p :id="textId" class="g-dialog-text">{{ body }}</p>
        </div>
      </div>
      <div v-if="file" class="upload-dialog-file">
        <span class="g-row-thumb"><img v-if="file.thumbnail" :src="file.thumbnail" alt="" /></span>
        <span class="g-row-text">
          <span class="g-row-title">{{ file.name }}</span>
          <span v-if="fileMeta" class="g-row-sub">{{ fileMeta }}</span>
        </span>
      </div>
      <p v-if="checked" class="g-dialog-note">
        {{ checked }}
        <a v-if="consent.termsUrl" class="g-link" :href="consent.termsUrl" @click.prevent="openTerms">{{
          t("upload.consent.terms")
        }}</a>
      </p>
      <p class="g-dialog-note">{{ note }}</p>
      <p v-if="problem" class="g-dialog-note upload-dialog-problem" role="alert">{{ problem }}</p>
      <div class="g-dialog-foot">
        <button v-if="canChooseHost && purpose === 'upload'" type="button" class="g-button" data-variant="plain" :disabled="busy" @click="emit('anotherHost')">
          {{ t("upload.consent.anotherHost") }}
        </button>
        <span class="g-spacer"></span>
        <button ref="cancelButton" type="button" class="g-button" :disabled="busy" @click="emit('cancel')">
          {{ t("upload.consent.cancel") }}
        </button>
        <button type="button" class="g-button" data-variant="primary" :disabled="busy" @click="confirm">
          {{ purpose === "test" ? t("upload.consent.sendTest") : t("upload.consent.upload") }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.upload-dialog-file {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px;
  border-radius: var(--radius-group);
  background: var(--win-group);
  box-shadow: inset 0 0 0 0.5px var(--win-group-edge);
}
.upload-dialog-problem {
  color: var(--win-danger);
}
</style>
