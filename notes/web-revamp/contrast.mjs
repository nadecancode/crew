// WCAG relative-luminance checks for meaningful text and control boundaries.
// The decorative divider --line is deliberately excluded.
const surfaces = {
  light: ['#f5f6f3', '#ffffff', '#eef1ed', '#e7f3ec', '#fff4df', '#faeaea'],
  dark: ['#101716', '#17201e', '#1e2a26', '#203a2f', '#332b1b', '#382424'],
};
const tokens = {
  light: { text:'#182321', muted:'#53635d', quiet:'#596b62', action:'#116e59', info:'#315ebe', ok:'#216e4d', warn:'#8a570b', err:'#b23b3b', accent:'#9a491e', codex:'#136b66', pi:'#82620c', shell:'#6954ab', p2p:'#ab3e68', border:'#75867d' },
  dark: { text:'#e5eeea', muted:'#a7b8b0', quiet:'#9aafa4', action:'#71d5ae', info:'#8cb7f2', ok:'#7ccea3', warn:'#e5bf6b', err:'#f39595', accent:'#e8ac82', codex:'#70cbbb', pi:'#debd6f', shell:'#bda5ec', p2p:'#f0a0bd', border:'#79978a' },
};
function luminance(hex) {
  const channels = hex.slice(1).match(/../g).map(x => parseInt(x, 16) / 255)
    .map(x => x <= .04045 ? x / 12.92 : ((x + .055) / 1.055) ** 2.4);
  return .2126 * channels[0] + .7152 * channels[1] + .0722 * channels[2];
}
function contrast(a, b) {
  const values = [luminance(a), luminance(b)].sort((a, b) => b - a);
  return (values[0] + .05) / (values[1] + .05);
}
let failed = false;
for (const theme of ['light', 'dark']) {
  for (const [name, value] of Object.entries(tokens[theme])) {
    const minimum = Math.min(...surfaces[theme].map(background => contrast(value, background)));
    const threshold = name === 'border' ? 3 : 4.5;
    console.log(`${theme} ${name}: minimum ${minimum.toFixed(2)}:1`);
    if (minimum < threshold) failed = true;
  }
}
if (failed) process.exitCode = 1;
