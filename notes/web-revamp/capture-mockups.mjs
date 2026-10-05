import { createRequire } from 'node:module';
import { writeFile } from 'node:fs/promises';
const require = createRequire(new URL('../../web/package.json', import.meta.url));
const { chromium } = require('@playwright/test');
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH });
const captures = [];
try {
  for (const scene of ['team', 'inspector', 'tasks', 'events', 'usage', 'question']) {
    for (const theme of ['light', 'dark']) {
      const page = await browser.newPage({ viewport: { width: 1440, height: 960 }, reducedMotion: 'reduce' });
      await page.goto(new URL(`./mockups/index.html?scene=${scene}&theme=${theme}`, import.meta.url).href);
      const file = `${scene}-${theme}-desktop.png`;
      await page.screenshot({ path: new URL(`./mockups/${file}`, import.meta.url).pathname });
      captures.push(file);
      await page.close();
    }
  }
  for (const theme of ['light', 'dark']) {
    const page = await browser.newPage({ viewport: { width: 390, height: 844 }, reducedMotion: 'reduce' });
    await page.goto(new URL(`./mockups/index.html?scene=team&theme=${theme}`, import.meta.url).href);
    await page.locator('.view-head .segmented').evaluate(e => { e.firstElementChild.classList.remove('chosen'); e.lastElementChild.classList.add('chosen'); });
    const file = `team-${theme}-narrow.png`;
    await page.screenshot({ path: new URL(`./mockups/${file}`, import.meta.url).pathname });
    captures.push(file);
    await page.close();
  }
} finally {
  await browser.close();
}
await writeFile(new URL('./mockups/manifest.json', import.meta.url), JSON.stringify({ captures }, null, 2) + '\n');
console.log(`Captured ${captures.length} mockups.`);
