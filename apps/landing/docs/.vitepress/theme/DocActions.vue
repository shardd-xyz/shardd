<script setup lang="ts">
import { useRoute } from "vitepress";
import { computed, ref } from "vue";

const route = useRoute();

// Only show on /guide/* pages — the homepage uses its own SdHome layout.
const showActions = computed(() => route.path.startsWith("/guide/"));

// /guide/quickstart   → /guide/quickstart.md
// /guide/quickstart/  → /guide/quickstart.md
const mdUrl = computed(() => `${route.path.replace(/\/$/, "")}.md`);

const copied = ref(false);
const failed = ref(false);

async function copyMarkdown() {
  failed.value = false;
  try {
    const res = await fetch(mdUrl.value);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const text = await res.text();
    await navigator.clipboard.writeText(text);
    copied.value = true;
    setTimeout(() => { copied.value = false; }, 2000);
  } catch (err) {
    console.error("copy markdown failed:", err);
    failed.value = true;
    setTimeout(() => { failed.value = false; }, 3000);
  }
}
</script>

<template>
  <!-- Anchor is zero-height so it doesn't push the article title down;
       the absolutely-positioned button floats at the top-right of the
       article column. -->
  <div v-if="showActions" class="sd-page-copy-anchor">
    <button
      type="button"
      class="sd-page-copy-btn"
      :data-state="copied ? 'copied' : failed ? 'failed' : 'idle'"
      :aria-label="copied ? 'Copied' : 'Copy page as Markdown'"
      :title="copied ? 'Copied' : failed ? 'Copy failed' : 'Copy page as Markdown'"
      @click="copyMarkdown"
    >
      <svg v-if="!copied && !failed" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
      </svg>
      <svg v-else-if="copied" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
        <polyline points="20 6 9 17 4 12" />
      </svg>
      <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <line x1="18" y1="6" x2="6" y2="18" />
        <line x1="6" y1="6" x2="18" y2="18" />
      </svg>
    </button>
  </div>
</template>

<style scoped>
.sd-page-copy-anchor {
  position: relative;
  height: 0;
  margin: 0;
}
.sd-page-copy-btn {
  position: absolute;
  right: 0;
  top: 4px;
  width: 30px;
  height: 30px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  color: var(--vp-c-text-2);
  background: var(--vp-c-bg);
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
  z-index: 5;
}
.sd-page-copy-btn:hover {
  color: var(--vp-c-text-1);
  border-color: var(--vp-c-brand-1);
  background: var(--vp-c-bg-soft);
}
.sd-page-copy-btn[data-state="copied"] {
  color: var(--vp-c-brand-1);
  border-color: var(--vp-c-brand-1);
}
.sd-page-copy-btn[data-state="failed"] {
  color: #d36363;
  border-color: #d36363;
}
</style>
