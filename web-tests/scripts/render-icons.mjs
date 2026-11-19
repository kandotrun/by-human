// Renders server/web/icons/icon.svg to the PNG sizes the web app manifest and
// iOS use. Run from web-tests: node scripts/render-icons.mjs
import { readFile } from 'node:fs/promises';
import { chromium } from '@playwright/test';

const icons = new URL('../../server/web/icons/', import.meta.url);
const svg = await readFile(new URL('icon.svg', icons), 'utf8');
const browser = await chromium.launch();
for (const [name, size] of [['icon-192.png', 192], ['icon-512.png', 512], ['apple-touch-icon.png', 180]]) {
  const page = await browser.newPage({ viewport: { width: size, height: size } });
  await page.setContent(`<style>html,body{margin:0}svg{display:block;width:${size}px;height:${size}px}</style>${svg}`);
  await page.screenshot({ path: new URL(name, icons).pathname });
  await page.close();
}
await browser.close();
