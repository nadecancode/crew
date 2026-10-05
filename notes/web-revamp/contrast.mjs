// Check the implemented palette, not a second copy of its expected colours.
// Run from any directory; an optional argument selects another theme.css.
import { readFile } from 'node:fs/promises';
const source = process.argv[2] ?? new URL('../../web/src/theme.css', import.meta.url);
const css = await readFile(source, 'utf8');
const declarations = new Map([...css.matchAll(/--([\w-]+)\s*:\s*([^;]+);/g)].map(([, name, value]) => [name, value.trim()]));
function resolve(name, theme, path = []) {
  if (path.includes(name)) throw new Error(`Cyclic colour token: ${[...path, name].join(', ')}`);
  let value = declarations.get(name);
  if (!value) throw new Error(`Missing colour token: --${name}`);
  const pair = /^light-dark\(([^,]+),\s*([^)]+)\)$/.exec(value);
  if (pair) value = pair[theme === 'light' ? 1 : 2].trim();
  const alias = /^var\(--([\w-]+)\)$/.exec(value);
  if (alias) return resolve(alias[1], theme, [...path, name]);
  if (/^#[\da-f]{3}$/i.test(value)) value = '#' + [...value.slice(1)].map(x => x + x).join('');
  if (!/^#[\da-f]{6}$/i.test(value)) throw new Error(`Unsupported colour value for --${name}: ${value}`);
  return value.toLowerCase();
}
function channels(hex) {
  return hex.slice(1).match(/../g).map(x => parseInt(x, 16));
}
function luminance(hex) {
  const [r, g, b] = channels(hex).map(x => x / 255)
    .map(x => x <= .04045 ? x / 12.92 : ((x + .055) / 1.055) ** 2.4);
  return .2126 * r + .7152 * g + .0722 * b;
}
function contrast(a, b) {
  const values = [luminance(a), luminance(b)].sort((a, b) => b - a);
  return (values[0] + .05) / (values[1] + .05);
}
let failed = false;
function check(condition, message) {
  console.log(`${condition ? 'PASS' : 'FAIL'} ${message}`);
  if (!condition) failed = true;
}
const surfaceNames = ['canvas', 'surface', 'surface-raised', 'action-surface', 'warn-surface', 'err-surface'];
const textNames = ['text', 'muted', 'quiet', 'action', 'info', 'ok', 'warn', 'err', 'accent', 'codex', 'pi', 'shell', 'p2p'];
const seriesNames = ['token-input', 'token-output', 'token-cache-read', 'token-cache-write'];
const neutralNames = ['canvas', 'surface', 'surface-raised', 'action-surface', 'text', 'muted', 'quiet', 'line', 'control-border', 'bg-term', 'term-text', 'term-selection'];
for (const theme of ['light', 'dark']) {
  const surfaces = surfaceNames.map(name => resolve(name, theme));
  for (const name of [...textNames, ...seriesNames, 'control-border']) {
    const minimum = Math.min(...surfaces.map(background => contrast(resolve(name, theme), background)));
    const threshold = name === 'control-border' ? 3 : 4.5;
    check(minimum >= threshold, `${theme} --${name}: minimum ${minimum.toFixed(2)}:1 (requires ${threshold}:1)`);
  }
  const series = seriesNames.map(name => resolve(name, theme));
  check(new Set(series).size === series.length, `${theme} usage series have four distinct colours`);
  for (const background of ['bg-term', 'term-selection']) {
    const ratio = contrast(resolve('term-text', theme), resolve(background, theme));
    check(ratio >= 4.5, `${theme} terminal text on --${background}: ${ratio.toFixed(2)}:1`);
  }
  const primary = contrast(resolve('canvas', theme), resolve('action', theme));
  check(primary >= 4.5, `${theme} primary button text: ${primary.toFixed(2)}:1`);
}
for (const name of neutralNames) {
  const value = resolve(name, 'dark');
  const [r, g, b] = channels(value);
  check(r === g && g === b, `dark --${name}: ${value} achromatic`);
}
const [red, green, blue] = channels(resolve('action', 'dark'));
check(blue > green && blue > red, 'dark action uses blue; green remains a semantic accent');
if (failed) process.exitCode = 1;
