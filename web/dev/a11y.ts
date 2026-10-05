// Lightweight evidence collector, not a replacement for a screen-reader or
// keyboard review. Records rendered text contrast, names, focus, and overflow
// from the actual application without changing its styles or DOM.
import type { Page } from "@playwright/test";

export async function auditPage(page: Page) {
  return page.evaluate(() => {
    const rgba = (value: string): number[] | null => {
      const m = /^rgba?\(([^)]+)\)$/.exec(value);
      if (!m) return null;
      const n = m[1].split(/[,\s/]+/).filter(Boolean).map(Number);
      return n.length >= 3 ? [n[0], n[1], n[2], n[3] ?? 1] : null;
    };
    const over = (fg: number[], bg: number[]) => fg.slice(0, 3).map((v, i) => v * fg[3] + bg[i] * (1 - fg[3]));
    const luminance = (c: number[]) => c.slice(0, 3).reduce((sum, v, i) => {
      const n = v / 255;
      return sum + (n <= .04045 ? n / 12.92 : ((n + .055) / 1.055) ** 2.4) * [.2126, .7152, .0722][i];
    }, 0);
    const ratio = (a: number[], b: number[]) => {
      const x = luminance(a), y = luminance(b);
      return (Math.max(x, y) + .05) / (Math.min(x, y) + .05);
    };
    const visible = (el: HTMLElement) => {
      const r = el.getBoundingClientRect(), s = getComputedStyle(el);
      return r.width > 0 && r.height > 0 && r.bottom > 0 && r.top < innerHeight && r.right > 0 && r.left < innerWidth && s.visibility !== "hidden" && s.display !== "none" && !el.closest("[hidden], [inert], [aria-hidden=true]");
    };
    const identify = (el: HTMLElement) => el.getAttribute("aria-label") || el.id || el.getAttribute("data-agent") || el.className || el.tagName.toLowerCase();
    const background = (el: HTMLElement) => {
      const chain: HTMLElement[] = [];
      for (let p: HTMLElement | null = el; p; p = p.parentElement) chain.unshift(p);
      let bg = [255, 255, 255];
      for (const p of chain) {
        const color = rgba(getComputedStyle(p).backgroundColor);
        if (color) bg = over(color, bg);
      }
      return bg;
    };
    const contrast: { element: string; text: string; ratio: number; required: number }[] = [];
    let checked = 0;
    for (const el of document.querySelectorAll<HTMLElement>("body *")) {
      if (!(el instanceof HTMLElement)) continue;
      if (!visible(el) || ["SCRIPT", "STYLE", "CANVAS", "SVG", "OPTION"].includes(el.tagName)) continue;
      const text = [...el.childNodes].filter((n) => n.nodeType === Node.TEXT_NODE).map((n) => n.textContent).join(" ").trim();
      if (!text || el.closest(".xterm")) continue;
      const css = getComputedStyle(el), fg = rgba(css.color);
      if (!fg) continue;
      const bg = background(el);
      let opacity = 1;
      for (let p: HTMLElement | null = el; p; p = p.parentElement) opacity *= Number(getComputedStyle(p).opacity);
      fg[3] *= opacity;
      const value = ratio(over(fg, bg), bg);
      const required = parseFloat(css.fontSize) >= 24 || (parseFloat(css.fontSize) >= 18.66 && Number(css.fontWeight) >= 700) ? 3 : 4.5;
      checked++;
      if (value + .01 < required) contrast.push({ element: identify(el), text: text.slice(0, 100), ratio: +value.toFixed(2), required });
    }
    const unnamed: string[] = [];
    const smallTargets: { element: string; width: number; height: number }[] = [];
    for (const el of document.querySelectorAll<HTMLElement>("button, input, textarea, select, [role=button], [role=tab]")) {
      if (!visible(el)) continue;
      const labels = (el as HTMLInputElement).labels;
      const name = el.getAttribute("aria-label") || el.getAttribute("aria-labelledby")?.split(/\s+/).map((id) => document.getElementById(id)?.textContent).join(" ") || (labels ? [...labels].map((l) => l.textContent).join(" ") : "") || (el.matches("button,[role=button],[role=tab]") ? el.textContent?.trim() : "");
      if (!name) unnamed.push(identify(el));
      const r = el.getBoundingClientRect();
      if (r.height < 44 || r.width < 44) smallTargets.push({ element: identify(el), width: +r.width.toFixed(1), height: +r.height.toFixed(1) });
    }
    const active = document.activeElement as HTMLElement | null;
    const focus = active ? getComputedStyle(active) : null;
    return {
      viewport: { width: innerWidth, height: innerHeight }, documentWidth: document.documentElement.scrollWidth,
      overflow: document.documentElement.scrollWidth > innerWidth,
      palette: { body: getComputedStyle(document.body).backgroundColor, text: getComputedStyle(document.body).color, theme: document.documentElement.getAttribute("data-theme") },
      contrast: { checked, failures: contrast }, unnamedControls: unnamed, targetsBelow44px: smallTargets,
      focused: active ? { element: identify(active), outline: focus?.outline, outlineOffset: focus?.outlineOffset } : null,
      panelWidth: document.querySelector("#side-panel")?.getBoundingClientRect().width ?? null,
      note: "Contrast checks solid sRGB text surfaces only; canvas, gradients, overlap, control boundaries, target exceptions, and screen-reader behavior require manual review.",
    };
  });
}
