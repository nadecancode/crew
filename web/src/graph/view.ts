// Pan and zoom as pure functions (notes/web-design.md §graphs "Viewport"):
// a transform `{x, y, k}` maps world coordinates to screen ones as
// `screen = world * k + (x, y)`.

export type View = { x: number; y: number; k: number };
export type Pt = { x: number; y: number };
export type Box = { x: number; y: number; w: number; h: number };
export type Size = { w: number; h: number };

export const K_MIN = 0.25;
export const K_MAX = 4;
/** A drag shorter than this (in screen pixels) is a click. */
export const CLICK_SLOP = 3;

export const clampK = (k: number) => Math.min(K_MAX, Math.max(K_MIN, k));

export const toWorld = (v: View, p: Pt): Pt => ({ x: (p.x - v.x) / v.k, y: (p.y - v.y) / v.k });
export const toScreen = (v: View, p: Pt): Pt => ({ x: p.x * v.k + v.x, y: p.y * v.k + v.y });

/** Zoom by `factor` about the screen point `at`: the world point under it stays put. */
export function zoomAt(v: View, at: Pt, factor: number): View {
  const k = clampK(v.k * factor);
  const w = toWorld(v, at);
  return { k, x: at.x - w.x * k, y: at.y - w.y * k };
}

export const pan = (v: View, dx: number, dy: number): View => ({ ...v, x: v.x + dx, y: v.y + dy });

/** The transform that shows `bounds` whole and centred in `size`, with a margin. */
export function fit(bounds: Box, size: Size, margin = 24, kMax = 1.25): View {
  if (bounds.w <= 0 || bounds.h <= 0 || size.w <= 0 || size.h <= 0) return { x: size.w / 2 - bounds.x, y: size.h / 2 - bounds.y, k: 1 };
  const k = clampK(Math.min((size.w - 2 * margin) / bounds.w, (size.h - 2 * margin) / bounds.h, kMax));
  return { k, x: size.w / 2 - (bounds.x + bounds.w / 2) * k, y: size.h / 2 - (bounds.y + bounds.h / 2) * k };
}

/** The smallest box around `boxes`. */
export function union(boxes: readonly Box[]): Box {
  if (boxes.length === 0) return { x: 0, y: 0, w: 0, h: 0 };
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const b of boxes) {
    x0 = Math.min(x0, b.x);
    y0 = Math.min(y0, b.y);
    x1 = Math.max(x1, b.x + b.w);
    y1 = Math.max(y1, b.y + b.h);
  }
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

export const overlaps = (a: Box, b: Box, gap = 0) =>
  a.x < b.x + b.w + gap && b.x < a.x + a.w + gap && a.y < b.y + b.h + gap && b.y < a.y + a.h + gap;

/** Where the segment from the centre of `box` towards `to` leaves the box. */
export function edgePoint(box: Box, to: Pt): Pt {
  const cx = box.x + box.w / 2, cy = box.y + box.h / 2;
  const dx = to.x - cx, dy = to.y - cy;
  if (dx === 0 && dy === 0) return { x: cx, y: cy };
  const sx = dx === 0 ? Infinity : box.w / 2 / Math.abs(dx);
  const sy = dy === 0 ? Infinity : box.h / 2 / Math.abs(dy);
  const s = Math.min(sx, sy);
  return { x: cx + dx * s, y: cy + dy * s };
}

export const center = (b: Box): Pt => ({ x: b.x + b.w / 2, y: b.y + b.h / 2 });
