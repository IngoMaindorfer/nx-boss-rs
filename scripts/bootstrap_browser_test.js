// Isolated browser fixtures only. No production paths or authenticators.
const { chromium } = require('@playwright/test');
const fs = require('node:fs');
const path = require('node:path');
(async () => {
  const [root, storage, credentials] = process.argv.slice(2);
  const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_PATH || undefined });
  const context = await browser.newContext({ ignoreHTTPSErrors: true });
  const page = await context.newPage();
  const client = await context.newCDPSession(page);
  await client.send('WebAuthn.enable');
  const { authenticatorId } = await client.send('WebAuthn.addVirtualAuthenticator', { options: {
    protocol: 'ctap2', transport: 'internal', hasResidentKey: true,
    hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true,
  } });
  await page.goto('https://localhost:12449/setup');
  await page.locator('[name=token]').fill(fs.readFileSync(path.join(root, 'admin-setup-token'), 'utf8'));
  await page.locator('[name=username]').fill('browser-admin');
  await page.locator('button[type=submit]').click();
  await page.waitForURL('https://localhost:12449/');
  await context.storageState({ path: storage });
  fs.chmodSync(storage, 0o600);
  fs.writeFileSync(credentials, JSON.stringify((await client.send('WebAuthn.getCredentials', { authenticatorId })).credentials), { mode: 0o600 });
  await browser.close();
})().catch(() => { console.error('Disposable passkey bootstrap failed'); process.exit(1); });
