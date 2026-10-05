// Run from the repository root after `cd web && bun install`.
// PLAYWRIGHT_EXECUTABLE_PATH=/path/to/chromium node notes/web-revamp/capture-baseline.mjs
// This uses the committed production bundle and real protocol-v6 mock server.
import { createRequire } from 'node:module';
import { mkdir, writeFile } from 'node:fs/promises';
import { startMock } from '../../web/dev/mock.ts';

const require = createRequire(new URL('../../web/package.json', import.meta.url));
const { chromium } = require('@playwright/test');
const dir = new URL('./baseline/', import.meta.url);
await mkdir(dir, { recursive: true });
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH });
const mock = await startMock({
  workspaces: [{ fixture: 'subagents' }, { fixture: 'team10', project: 'search' }],
  others: [{ id: '4c2e8a1b9d0f3e57', project: 'docs', workspace: '/home/op/work/docs' }],
});
const findings = [];
const waitFrame = (page) => page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
try {
  for (const scheme of ['dark', 'light']) {
    for (const size of ['desktop', 'narrow']) {
      const page = await browser.newPage({
        viewport: size === 'desktop' ? { width: 1440, height: 960 } : { width: 390, height: 844 },
        colorScheme: scheme, reducedMotion: 'reduce',
      });
      await page.goto(mock.pairUrl());
      await page.locator('[data-workspace-state="live"]').waitFor();
      const route = async (suffix = '') => {
        await page.evaluate((suffix) => {
          const base = location.hash.match(/^#\/w\/[^/]+/)[0];
          location.hash = base + '/' + suffix;
        }, suffix);
        await waitFrame(page);
      };
      const capture = async (name) => {
        await waitFrame(page);
        const file = `${name}-${scheme}-${size}.png`;
        await page.screenshot({ path: new URL(file, dir).pathname });
        findings.push({ file, scheme, size, ...(await page.evaluate(() => ({
          viewport: innerWidth, documentWidth: document.documentElement.scrollWidth,
          bodyBackground: getComputedStyle(document.body).backgroundColor,
          bodyMinWidth: getComputedStyle(document.body).minWidth,
          panelWidth: document.querySelector('#side-panel')?.getBoundingClientRect().width,
        }))) });
      };
      await capture('agents');
      await route('tasks');
      await page.locator('.task-dag').waitFor();
      await capture('tasks');
      await route('tasks?task=ledger');
      await page.locator('.task-panel .task-title').waitFor();
      await capture('task-detail');
      await route('logs');
      await page.locator('.log-list').waitFor();
      await capture('events');
      const event = mock.state.events.find((e) => e.source === 'vcs') ?? mock.state.events[0];
      await route(`logs?event=${event.seq}`);
      await page.locator('.event-panel').waitFor();
      await capture('event-detail');
      await route('usage');
      await page.locator('.usage-view').waitFor();
      await capture('usage');
      await route('?agent=claude-1');
      await page.locator('[data-inspector="claude-1"]').waitFor();
      await capture('inspector');
      // DOM activation avoids changing the 390px viewport by browser auto-scroll.
      await page.getByRole('tab', { name: 'terminal', exact: true }).evaluate((e) => e.click());
      await page.locator('.terminal-tab').waitFor({ state: 'attached' }).catch(() => {});
      await capture('terminal');
      await page.getByRole('tab', { name: 'transcript', exact: true }).evaluate((e) => e.click());
      await page.locator('.transcript').waitFor();
      await capture('transcript');
      await page.getByRole('tab', { name: 'diff', exact: true }).evaluate((e) => e.click());
      await page.locator('.diff').waitFor();
      await capture('diff');
      await route(`?message=${mock.state.messages.at(-1).id}`);
      await page.locator('.message-panel').waitFor();
      await capture('message-detail');
      await route();
      await page.locator('.switcher .project').evaluate((e) => e.click());
      await page.locator('.switcher-pop').waitFor();
      await capture('switcher');
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: 'keys and commands', exact: true }).evaluate((e) => e.click());
      await page.getByRole('dialog').waitFor();
      await capture('help');
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: 'stop team', exact: true }).evaluate((e) => e.click());
      await page.getByRole('dialog').waitFor();
      await capture('confirm');
      await page.keyboard.press('Escape');
      await page.locator('[data-agent="claude-1"]').evaluate((e) => {
        e.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 180, clientY: 250 }));
      });
      await page.getByRole('menu').waitFor();
      await capture('context-menu');
      await page.close();
    }
  }
} finally {
  await browser.close();
  await mock.stop();
  await writeFile(new URL('manifest.json', dir), JSON.stringify({ fixture: 'subagents', note: 'Light means OS color preference; current app has no light palette. Narrow screenshots intentionally retain clipping.', captures: findings }, null, 2) + '\n');
}
console.log(`Captured ${findings.length} views.`);
