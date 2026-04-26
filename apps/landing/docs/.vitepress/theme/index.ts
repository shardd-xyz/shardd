import DefaultTheme from "vitepress/theme";
import type { Router } from "vitepress";
import { h } from "vue";
import SdHome from "./SdHome.vue";
import DocActions from "./DocActions.vue";
import "./custom.css";

// Persist the selected tab across every ::: code-group on the site.
// VitePress already syncs labels across groups on a single page; this
// adds cross-page + cross-reload stickiness via localStorage, keyed on
// the lowercased tab label (`curl`, `rust`, `python`, `typescript`,
// `kotlin`).
const STORAGE_KEY = "shardd.code.lang";
const CANONICAL = new Set(["curl", "rust", "python", "typescript", "kotlin"]);

function normalize(label: string): string {
  return label.trim().toLowerCase();
}

function applyStoredLang(): void {
  if (typeof window === "undefined") return;
  const want = window.localStorage.getItem(STORAGE_KEY);
  if (!want || !CANONICAL.has(want)) return;
  document.querySelectorAll<HTMLElement>(".vp-code-group").forEach((group) => {
    const labels = Array.from(
      group.querySelectorAll<HTMLLabelElement>(".tabs label"),
    );
    const matchIdx = labels.findIndex(
      (label) =>
        normalize(label.getAttribute("data-title") || label.textContent || "") ===
        want,
    );
    if (matchIdx < 0) return;
    // Radio state keeps the tab highlight correct.
    labels.forEach((label, i) => {
      const input = label.htmlFor
        ? (document.getElementById(label.htmlFor) as HTMLInputElement | null)
        : null;
      if (input) input.checked = i === matchIdx;
    });
    // Blocks are show/hidden via `.active` — purely CSS on VitePress's
    // side, but the class is a Vue reactive binding, so a naive
    // input.click() or label.click() won't flip it. We move `.active`
    // between block children ourselves.
    const blocks = group.querySelectorAll<HTMLElement>(".blocks > div");
    blocks.forEach((block, i) => {
      block.classList.toggle("active", i === matchIdx);
    });
  });
}

function wireClicks(): void {
  if (typeof window === "undefined") return;
  // Capture phase: guarantees we observe the click regardless of any
  // stopPropagation the default theme might do on the label or the
  // radio. We only *read* the label text — the browser's own
  // label-for-input binding still toggles the radio itself.
  document.addEventListener(
    "click",
    (event) => {
      const target = event.target;
      if (!(target instanceof HTMLElement)) return;
      const label = target.closest("label");
      if (!label || !label.closest(".vp-code-group")) return;
      const lang = normalize(
        label.getAttribute("data-title") || label.textContent || "",
      );
      if (!CANONICAL.has(lang)) return;
      window.localStorage.setItem(STORAGE_KEY, lang);
      // Let the clicked radio flip first, then mirror to other groups.
      queueMicrotask(applyStoredLang);
      setTimeout(applyStoredLang, 0);
    },
    true,
  );
}

export default {
  extends: DefaultTheme,
  Layout() {
    // Inject the per-page copy / .md / llms.txt control above the
    // article body. The component itself only renders on /guide/*.
    return h(DefaultTheme.Layout, null, {
      "doc-before": () => h(DocActions),
    });
  },
  enhanceApp({
    app,
    router,
  }: {
    app: import("vue").App;
    router: Router;
  }) {
    app.component("SdHome", SdHome);
    if (typeof window === "undefined") return;
    (window as unknown as { __sharddThemeInit: boolean }).__sharddThemeInit = true;
    wireClicks();
    // Vue mounts asynchronously on initial load and on SPA navigation,
    // so querying for `.vp-code-group` right here typically returns
    // nothing. Poll briefly for up to ~1.5s until at least one group
    // exists, then apply once. Bail silently if none ever show up
    // (plenty of pages have no code-groups).
    const reapply = () => {
      let attempts = 0;
      const tick = () => {
        if (document.querySelector(".vp-code-group")) {
          applyStoredLang();
          return;
        }
        if (attempts++ < 30) setTimeout(tick, 50);
      };
      tick();
    };
    router.onAfterRouteChanged = reapply;
    reapply();
  },
};
