const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './tests/browser',
  fullyParallel: false,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: process.env.CI ? [['line'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: 'https://127.0.0.1:12449',
    ignoreHTTPSErrors: true,
    storageState: '/tmp/nx-boss-browser-storage.json',
    browserName: 'chromium',
    launchOptions: process.env.PLAYWRIGHT_CHROMIUM_PATH
      ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_PATH }
      : {},
    trace: 'retain-on-failure',
  },
  webServer: {
    command: 'bash scripts/serve_playwright.sh',
    url: 'https://127.0.0.1:12449/login',
    ignoreHTTPSErrors: true,
    reuseExistingServer: false,
    timeout: 120000,
  },
});
