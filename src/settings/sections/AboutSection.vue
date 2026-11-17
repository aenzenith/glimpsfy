<script setup lang="ts">
// Settings › About: the app's name and version, its licence, links, the update state, the
// projects it is built on and the way back to the welcome window.
import { getVersion } from "@tauri-apps/api/app";
import { onMounted, ref } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import { app_open_url, window_open } from "../../lib/api/windows";
import { t, type Key } from "../../lib/i18n";
import { banner } from "../store";
import { describeError } from "../text";
import UpdateRow from "../UpdateRow.vue";

const version = ref<string | null>(null);
onMounted(async () => {
  try {
    version.value = await getVersion();
  } catch {
    version.value = null;
  }
});

const REPO = "https://github.com/aenzenith/glimpsfy";
const LINKS: { icon: string; title: Key; url: string }[] = [
  { icon: "globe", title: "settings.about.source", url: REPO },
  { icon: "warning", title: "settings.about.issue", url: `${REPO}/issues` },
  { icon: "history", title: "settings.about.changelog", url: `${REPO}/releases` },
];

const ACK: { project: string; use: Key; licence: Key }[] = [
  { project: "Tauri", use: "settings.about.ackTauri", licence: "settings.about.mitOrApache" },
  { project: "Vue", use: "settings.about.ackVue", licence: "settings.about.mit" },
  { project: "SQLite · rusqlite", use: "settings.about.ackSqlite", licence: "settings.about.publicDomainMit" },
  { project: "keyring-core", use: "settings.about.ackKeyring", licence: "settings.about.mitOrApache" },
  { project: "objc2", use: "settings.about.ackObjc2", licence: "settings.about.mit" },
  { project: "png · image-webp", use: "settings.about.ackImages", licence: "settings.about.mitOrApache" },
  { project: "jpeg-encoder", use: "settings.about.ackJpeg", licence: "settings.about.mitOrApacheIjg" },
  { project: "ureq", use: "settings.about.ackUreq", licence: "settings.about.mitOrApache" },
];

function open(url: string): void {
  void app_open_url(url).catch((error) => (banner.value = { tone: "danger", text: describeError(error) }));
}

function welcome(): void {
  void window_open("firstrun").catch((error) => (banner.value = { tone: "danger", text: describeError(error) }));
}
</script>

<template>
  <div class="p-about">
    <span class="p-about-icon"><GIcon name="glyph" :size="40" /></span>
    <h2 class="p-about-name">Glimpsfy</h2>
    <p v-if="version" class="p-about-meta">{{ t("settings.about.version", { version }) }}</p>
    <p class="p-about-meta">{{ t("settings.about.licence") }}</p>
  </div>

  <SectionHead :title="t('settings.about.headUpdate')" />
  <div class="g-group">
    <UpdateRow />
  </div>

  <SectionHead :title="t('settings.about.headLinks')" />
  <div class="g-group">
    <div class="g-list p-list">
      <button v-for="link in LINKS" :key="link.url" type="button" class="g-row" @click="open(link.url)">
        <span class="g-row-lead"><GIcon :name="link.icon" :size="16" /></span>
        <span class="g-row-text">
          <span class="g-row-title">{{ t(link.title) }}</span>
          <span class="g-row-sub">{{ link.url.replace(/^https:\/\//, "") }}</span>
        </span>
        <span class="g-row-trail"><GIcon name="external-link" :size="16" class="p-chev" /></span>
      </button>
    </div>
  </div>

  <SectionHead :title="t('settings.about.headWelcome')" />
  <div class="g-group">
    <FormRow :label="t('settings.about.welcome')" :help="t('settings.about.welcomeHelp')">
      <button type="button" class="g-button" @click="welcome">{{ t("settings.about.welcomeOpen") }}</button>
    </FormRow>
  </div>

  <SectionHead :title="t('settings.about.headAck')" :desc="t('settings.about.ackDesc')" />
  <div class="g-group">
    <div class="p-table">
      <table class="g-table">
        <thead>
          <tr>
            <th>{{ t("settings.about.project") }}</th>
            <th>{{ t("settings.about.usedFor") }}</th>
            <th>{{ t("settings.about.licenceColumn") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in ACK" :key="row.project">
            <td>{{ row.project }}</td>
            <td class="g-cell-sub">{{ t(row.use) }}</td>
            <td class="g-cell-sub">{{ t(row.licence) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
  <!-- The IJG licence of the JPEG encoder asks for this sentence in the documentation of every
       binary that carries it. -->
  <p class="p-foot g-help">{{ t("settings.about.ijg") }}</p>
  <div class="p-foot"><span class="g-help">{{ t("settings.about.copyright") }}</span></div>
</template>
