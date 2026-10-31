const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { spawn, spawnSync } = require('child_process');

const origin = 'https://localhost:12549';
let directory;
let app;
let proxy;

const startApp = () => spawn('./target/debug/nx-boss-rs', [
  '--config', path.join(directory, 'config.yaml'), '--host', '127.0.0.1', '--port', '12547',
  '--admin-host', '127.0.0.1', '--admin-port', '12548', '--admin-allowed-host', 'localhost',
], {
  env: { ...process.env, NX_BOSS_AUTH_PEPPER_FILE: path.join(directory, 'pepper'),
    NX_BOSS_ADMIN_DOMAIN: 'localhost', NX_BOSS_ADMIN_HTTPS_PORT: '12549' },
  stdio: 'ignore',
});

const waitFor = async (url) => {
  for (let attempt = 0; attempt < 80; attempt += 1) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch (_) { /* server still starting */ }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error('Passkey test server did not start');
};

test.use({ storageState: { cookies: [], origins: [] } });

test.beforeAll(async () => {
  directory = fs.mkdtempSync(path.join(os.tmpdir(), 'nx-boss-passkey-'));
  fs.mkdirSync(path.join(directory, 'scans'));
  fs.writeFileSync(path.join(directory, 'config.yaml'), `jobs:\n  Test:\n    output_path: ${directory}/scans\n    color: '#2196F3'\n`);
  fs.writeFileSync(path.join(directory, 'pepper'), crypto.randomBytes(32), { mode: 0o600 });
  app = startApp();
  await waitFor('http://127.0.0.1:12547/health');
  const certificate = path.join(directory, 'cert.pem');
  const key = path.join(directory, 'key.pem');
  const openssl = spawnSync('openssl', [
    'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key, '-out', certificate,
    '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost',
  ], { stdio: 'ignore' });
  if (openssl.status !== 0) throw new Error('Could not create browser test certificate');
  proxy = spawn('python3', [
    'scripts/test_https_proxy.py', certificate, key, '12549', '12548', 'localhost:12549',
  ], { stdio: 'ignore' });
});

test.afterAll(async () => {
  proxy?.kill();
  app?.kill();
  if (directory) fs.rmSync(directory, { recursive: true, force: true });
});

test('setup visibility, keyboard, mobile layout and distinct errors in both languages', async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await page.emulateMedia({ colorScheme: 'dark' });
  for (const lang of ['de', 'en']) {
    await page.goto(`${origin}/language/${lang}?next=/setup`);
    const field = page.locator('[name=token]');
    const toggle = page.locator('[data-secret-toggle]');
    await expect(field).toHaveAttribute('type', 'password');
    await expect(field).toHaveAttribute('spellcheck', 'false');
    await expect(field).toHaveAttribute('autocapitalize', 'off');
    await expect(toggle).toHaveText(lang === 'de' ? 'Anzeigen' : 'Show');
    let submissions = 0;
    const onRequest = (request) => { if (request.url().endsWith('/passkey/setup/start')) submissions += 1; };
    page.on('request', onRequest);
    await toggle.focus();
    await page.keyboard.press('Space');
    await expect(field).toHaveAttribute('type', 'text');
    await expect(toggle).toHaveText(lang === 'de' ? 'Verbergen' : 'Hide');
    await page.keyboard.press('Enter');
    await expect(field).toHaveAttribute('type', 'password');
    expect(submissions).toBe(0);
    page.off('request', onRequest);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: `/tmp/nx-setup-${lang}-mobile-dark.png` });
    await field.fill('not-the-setup-code');
    await page.locator('[name=username]').fill('browser-admin');
    for (const status of [401, 403]) {
      await page.route('**/passkey/setup/start', (route) => route.fulfill({ status }));
      await page.locator('button[type=submit]').click();
      const error = page.locator('[data-passkey-error]');
      await expect(error).toContainText(status === 401
        ? (lang === 'de' ? 'Einrichtungscode ist falsch oder abgelaufen' : 'Setup code is incorrect or expired')
        : 'https://nx.maindorfer.name');
      await page.unroute('**/passkey/setup/start');
    }
    await field.fill(fs.readFileSync(path.join(directory, 'admin-setup-token'), 'utf8'));
    await page.evaluate(() => Object.defineProperty(navigator.credentials, 'create', {
      configurable: true, value: async () => { throw new DOMException('cancelled', 'NotAllowedError'); },
    }));
    await page.locator('button[type=submit]').click();
    await expect(page.locator('[data-passkey-error]')).toContainText(lang === 'de' ? 'Vorgang wurde abgebrochen' : 'Operation was cancelled');
  }
});

test('passkey setup, login, replay rejection, and sensitive change', async ({ page, context }) => {
  const passkeyResponses = [];
  page.on('response', (response) => {
    const pathname = new URL(response.url()).pathname;
    if (pathname.startsWith('/passkey/')) passkeyResponses.push(`${pathname}:${response.status()}`);
  });
  const client = await context.newCDPSession(page);
  await client.send('WebAuthn.enable');
  await client.send('WebAuthn.addVirtualAuthenticator', { options: {
    protocol: 'ctap2', transport: 'internal', hasResidentKey: true,
    hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true,
  } });
  await page.goto(`${origin}/setup`);
  const token = fs.readFileSync(path.join(directory, 'admin-setup-token'), 'utf8');
  await page.locator('[name=token]').fill(`${token} `);
  await page.locator('[data-secret-toggle]').click();
  await page.evaluate(() => {
    const create = navigator.credentials.create.bind(navigator.credentials);
    Object.defineProperty(navigator.credentials, 'create', { configurable: true, value: async (options) => {
      sessionStorage.setItem('setupWasHidden', String(document.querySelector('[name=token]').type === 'password'));
      return create(options);
    } });
  });
  await page.locator('[name=username]').fill('browser-admin');
  await page.getByRole('button', { name: 'Passkey erstellen' }).click();
  await expect(page).toHaveURL(`${origin}/`);
  expect(await page.evaluate(() => sessionStorage.getItem('setupWasHidden'))).toBe('true');
  await expect(page.locator('[data-passkey-error]')).toHaveCount(0);
  expect(fs.existsSync(path.join(directory, 'admin-passkeys.json'))).toBe(true);
  expect(fs.existsSync(path.join(directory, 'admin-setup-token'))).toBe(false);
  for (const pathname of ['/login', '/setup/prepare', '/setup/complete', '/account', '/account/totp/prepare', '/account/totp/complete']) {
    const removed = await context.request.post(`${origin}${pathname}`, { headers: { origin }, data: {}, failOnStatusCode: false });
    expect([404, 405]).toContain(removed.status());
  }


  await page.goto(`${origin}/account`);
  await page.route('**/passkey/account/add/start', async (route) => {
    await client.send('WebAuthn.addVirtualAuthenticator', { options: {
      protocol: 'ctap2', transport: 'usb', hasResidentKey: true,
      hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true,
    } });
    await route.continue();
  });
  await page.getByRole('button', { name: 'Weiteren Passkey hinzufügen' }).click();
  try {
    await expect(page.getByText('2 Passkey(s) eingerichtet')).toBeVisible();
  } catch (_) {
    throw new Error(`Backup passkey registration did not finish: ${passkeyResponses.join(', ')}`);
  }

  await context.clearCookies();
  await page.goto(`${origin}/login`);
  let assertion;
  await page.route('**/passkey/login/finish', async (route) => {
    assertion = route.request().postDataJSON();
    await route.continue();
  });
  await page.getByRole('button', { name: 'Mit Passkey anmelden' }).click();
  await expect(page).toHaveURL(`${origin}/`);
  const replay = await context.request.post(`${origin}/passkey/login/finish`, {
    headers: { origin }, data: assertion, failOnStatusCode: false,
  });
  expect(replay.status()).toBe(401);

  const missingStepup = await context.request.post(`${origin}/settings/scanner`, {
    headers: { origin }, form: { mode: 'none', username: '', new_password: '' },
    failOnStatusCode: false,
  });
  expect(missingStepup.status()).toBe(401);
  const wrongOrigin = await context.request.post(`${origin}/passkey/stepup/start`, {
    headers: { origin: 'https://attacker.invalid' }, data: {}, failOnStatusCode: false,
  });
  expect(wrongOrigin.status()).toBe(403);

  await page.goto(`${origin}/settings/scanner`);
  await page.getByRole('button', { name: 'Speichern' }).click();
  await expect(page.getByRole('status')).toContainText('Gespeichert');

  const exit = new Promise((resolve) => app.once('exit', resolve));
  app.kill('SIGTERM');
  await exit;
  app = startApp();
  await waitFor('http://127.0.0.1:12547/health');
  await page.goto(`${origin}/account`);
  await expect(page).toHaveURL(`${origin}/login`);
  await page.getByRole('button', { name: 'Mit Passkey anmelden' }).click();
  await expect(page).toHaveURL(`${origin}/`);
  // Recovery actions use the normal page, real session and fresh passkey assertions.
  const opened = await context.request.post('http://127.0.0.1:12547/NmWebService/batch', { data: { job_id: 0 } });
  expect(opened.status()).toBe(200);
  const batchId = (await opened.json()).batch_id;
  await page.goto(`${origin}/scans/${batchId}`);
  await page.getByRole('button', { name: 'Abschließen und zustellen', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Abschließen und zustellen', exact: true })).toHaveCount(0);
  expect(JSON.parse(fs.readFileSync(path.join(directory, 'scans', batchId, 'metadata.json'), 'utf8')).completed).toBe(true);
  const corruptId = '0123456789abcdef0123456789abcdef';
  fs.mkdirSync(path.join(directory, 'scans', corruptId));
  await page.goto(`${origin}/scans/${corruptId}`);
  await expect(page.getByText('Dieser Scan ist beschädigt.', { exact: false })).toBeVisible();
  await page.getByRole('button', { name: 'Verwerfen', exact: true }).click();
  await expect(page).toHaveURL(`${origin}/scans`);
  expect(fs.existsSync(path.join(directory, 'scans', corruptId))).toBe(false);

});
