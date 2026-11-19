import { defineConfig } from '@playwright/test';

// Pure browser-module contracts. They need no Rust fixture or database, so
// they can run beside the main suite.
export default defineConfig({
  testDir: './unit',
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  timeout: 30_000,
  reporter: process.env.CI ? 'github' : 'list',
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
});
