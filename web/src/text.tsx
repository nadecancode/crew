// Text agents control, shown as text (notes/web-design.md §security "Agent
// text in the page"): no Markdown, no HTML. A URL becomes a link only when it
// parses as `http:` or `https:`. Colour in a captured screen (SGR sequences)
// becomes spans with classes from a fixed palette; every other escape
// sequence is dropped.

import { For, type JSX } from "solid-js";

export type Piece = { text: string; href?: string };

const URLISH = /\bhttps?:\/\/[^\s<>"'`]+/gi;

/** Only `http:` and `https:` URLs become links; the trailing punctuation of a sentence is not part of one. */
export function safeHref(candidate: string): string | null {
  try {
    const u = new URL(candidate);
    return u.protocol === "http:" || u.protocol === "https:" ? u.href : null;
  } catch {
    return null;
  }
}

export function linkPieces(text: string): Piece[] {
  const out: Piece[] = [];
  let at = 0;
  for (const m of text.matchAll(URLISH)) {
    let url = m[0];
    const trail = /[.,;:!?)\]}]+$/.exec(url);
    if (trail) url = url.slice(0, url.length - trail[0].length);
    const href = safeHref(url);
    if (!href) continue;
    if (m.index! > at) out.push({ text: text.slice(at, m.index) });
    out.push({ text: url, href });
    at = m.index! + url.length;
  }
  if (at < text.length) out.push({ text: text.slice(at) });
  return out;
}

/** Text with its http(s) links, as DOM text and anchors set by Solid. */
export function Linked(props: { text: string }): JSX.Element {
  return (
    <For each={linkPieces(props.text)}>
      {(p) => (p.href ? <a href={p.href} target="_blank" rel="noopener noreferrer">{p.text}</a> : p.text)}
    </For>
  );
}

// ---- SGR ---------------------------------------------------------------------

export type Span = { text: string; classes: string };

type Style = { fg: string | null; bg: string | null; bold: boolean; dim: boolean; italic: boolean; underline: boolean; inverse: boolean };
const plain = (): Style => ({ fg: null, bg: null, bold: false, dim: false, italic: false, underline: false, inverse: false });

/** The nearest of the 16 palette colours to an RGB value. */
function nearest16(r: number, g: number, b: number): number {
  const PALETTE: [number, number, number][] = [
    [0, 0, 0], [205, 49, 49], [13, 188, 121], [229, 229, 16], [36, 114, 200], [188, 63, 188], [17, 168, 205], [229, 229, 229],
    [102, 102, 102], [241, 76, 76], [35, 209, 139], [245, 245, 67], [59, 142, 234], [214, 112, 214], [41, 184, 219], [255, 255, 255],
  ];
  let best = 0, d = Infinity;
  PALETTE.forEach(([pr, pg, pb], i) => {
    const e = (pr - r) ** 2 + (pg - g) ** 2 + (pb - b) ** 2;
    if (e < d) [best, d] = [i, e];
  });
  return best;
}

function cube(n: number): number {
  if (n < 16) return n;
  if (n >= 232) {
    const v = 8 + (n - 232) * 10;
    return nearest16(v, v, v);
  }
  const i = n - 16, steps = [0, 95, 135, 175, 215, 255];
  return nearest16(steps[Math.floor(i / 36)], steps[Math.floor(i / 6) % 6], steps[i % 6]);
}

function apply(style: Style, params: number[]): void {
  if (params.length === 0) params = [0];
  for (let i = 0; i < params.length; i++) {
    const p = params[i];
    if (p === 0) Object.assign(style, plain());
    else if (p === 1) style.bold = true;
    else if (p === 2) style.dim = true;
    else if (p === 3) style.italic = true;
    else if (p === 4) style.underline = true;
    else if (p === 7) style.inverse = true;
    else if (p === 22) style.bold = style.dim = false;
    else if (p === 23) style.italic = false;
    else if (p === 24) style.underline = false;
    else if (p === 27) style.inverse = false;
    else if (p >= 30 && p <= 37) style.fg = `c${p - 30}`;
    else if (p >= 90 && p <= 97) style.fg = `c${p - 90 + 8}`;
    else if (p === 39) style.fg = null;
    else if (p >= 40 && p <= 47) style.bg = `c${p - 40}`;
    else if (p >= 100 && p <= 107) style.bg = `c${p - 100 + 8}`;
    else if (p === 49) style.bg = null;
    else if (p === 38 || p === 48) {
      let c: number | null = null;
      if (params[i + 1] === 5 && params[i + 2] !== undefined) {
        c = cube(params[i + 2]);
        i += 2;
      } else if (params[i + 1] === 2 && params[i + 4] !== undefined) {
        c = nearest16(params[i + 2], params[i + 3], params[i + 4]);
        i += 4;
      }
      if (c !== null) {
        if (p === 38) style.fg = `c${c}`;
        else style.bg = `c${c}`;
      }
    }
  }
}

const classes = (s: Style) =>
  [s.fg && `fg-${s.fg}`, s.bg && `bg-${s.bg}`, s.bold && "b", s.dim && "d", s.italic && "i", s.underline && "u", s.inverse && "inv"].filter(Boolean).join(" ");

/** Text with SGR colour codes as spans; every other escape sequence and control character is dropped. */
export function sgrSpans(input: string): Span[] {
  const out: Span[] = [];
  const style = plain();
  let text = "";
  const flush = () => {
    if (text) out.push({ text, classes: classes(style) });
    text = "";
  };
  for (let i = 0; i < input.length; i++) {
    const ch = input[i];
    if (ch === "\x1b") {
      const next = input[i + 1];
      if (next === "[") {
        // CSI: parameters, intermediates, one final byte.
        let j = i + 2;
        while (j < input.length && /[0-?]/.test(input[j])) j++;
        while (j < input.length && /[ -/]/.test(input[j])) j++;
        const final = input[j];
        if (final === "m") {
          const body = input.slice(i + 2, j);
          if (/^[\d;]*$/.test(body)) {
            flush();
            apply(style, body === "" ? [] : body.split(";").map((x) => (x === "" ? 0 : Number(x))));
          }
        }
        i = j;
      } else if (next === "]" || next === "P" || next === "_" || next === "^") {
        // OSC, DCS, APC, PM: up to BEL or ST.
        let j = i + 2;
        while (j < input.length && input[j] !== "\x07" && !(input[j] === "\x1b" && input[j + 1] === "\\")) j++;
        i = input[j] === "\x07" ? j : j + 1;
      } else {
        i += 1;
      }
      continue;
    }
    if (ch === "\n" || ch === "\t") text += ch;
    else if (ch < " " || ch === "\x7f" || (ch >= "\x80" && ch <= "\x9f")) continue;
    else text += ch;
  }
  flush();
  return out;
}

export function Sgr(props: { text: string }): JSX.Element {
  return <For each={sgrSpans(props.text)}>{(s) => (s.classes ? <span class={s.classes}>{s.text}</span> : s.text)}</For>;
}
