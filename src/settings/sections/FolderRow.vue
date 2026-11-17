<script setup lang="ts">
// A folder setting: the folder as a path chip with Choose…, and a warning when the folder is
// missing (it is made at the next save) or Glimpsfy may not write in it (saves fail).
import { ref, watch } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import { settings_folder_status, type FolderStatus } from "../../lib/api/settings";
import { app_choose_folder } from "../../lib/api/windows";
import { t } from "../../lib/i18n";
import { describeError, folderLabel, home } from "../text";

const props = defineProps<{ label: string; path: string }>();
const emit = defineEmits<{ change: [path: string] }>();

const status = ref<FolderStatus | null>(null);
const error = ref<string | null>(null);

async function check(path: string): Promise<void> {
  try {
    status.value = await settings_folder_status(path);
  } catch {
    status.value = null;
  }
}
watch(() => props.path, (path) => void check(path), { immediate: true });

async function choose(): Promise<void> {
  error.value = null;
  try {
    const picked = await app_choose_folder(props.path || null);
    if (picked) emit("change", picked);
  } catch (e) {
    error.value = describeError(e);
  }
}

function help(): { text: string; tone: "warn" | "danger" } | null {
  if (error.value) return { text: error.value, tone: "danger" };
  if (!status.value) return null;
  if (!status.value.exists) return { text: t("settings.folder.missing"), tone: "warn" };
  if (!status.value.writable) return { text: t("settings.folder.readOnly"), tone: "danger" };
  return null;
}
</script>

<template>
  <FormRow :label="label" :help="help()?.text" :tone="help()?.tone">
    <span class="p-path" :data-tone="help()?.tone === 'warn' ? 'warn' : undefined" :title="path"
      ><GIcon name="folder" :size="16" /><span>{{ folderLabel(path, home) }}</span></span
    >
    <button type="button" class="g-button" @click="choose">{{ t("settings.common.choose") }}</button>
  </FormRow>
</template>
