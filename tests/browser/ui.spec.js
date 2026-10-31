const { test, expect } = require('@playwright/test');
const crypto = require('node:crypto');
const fs = require('node:fs');

async function waitForSeededScan(page, path) {
  await page.goto(path);
  // The app starts serving before the scanner fixture finishes its requests.
  await expect.poll(async () => {
    await page.reload();
    return page.locator('tr.scan-row').count();
  }, { timeout: 15000 }).toBeGreaterThan(0);
}

test('language selection is per browser and survives navigation', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('html')).toHaveAttribute('lang', 'de');
  await page.getByLabel('Sprache').selectOption('en');
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await expect(page.getByRole('heading', { name: 'Recent Scans' })).toBeVisible();
  await page.getByRole('link', { name: 'Settings' }).click();
  await expect(page.getByRole('heading', { name: 'Settings' })).toBeVisible();
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await page.getByLabel('Language', { exact: true }).selectOption('de');
  await expect(page.locator('html')).toHaveAttribute('lang', 'de');
  await expect(page.getByRole('heading', { name: 'Einstellungen' })).toBeVisible();
});

test('API explorer renders the scanner contract behind the admin login', async ({ page, browser }) => {
  await page.goto('/api-docs');
  await expect(page.getByRole('heading', { name: 'API-Doku' })).toBeVisible();
  await expect(page.getByText('/NmWebService/authorization').first()).toBeVisible();
  await expect(page.getByText('/NmWebService/image').first()).toBeVisible();
  const loginOperation = page.locator('details').filter({ hasText: 'Log in and fetch ordered jobs' });
  await loginOperation.locator('summary').click();
  await expect(loginOperation.locator('pre').first()).toContainText('"auth_info_pass"');
  await expect(loginOperation.locator('pre').first()).toContainText('"writeOnly": true');
  const spec = await page.request.get('/api/openapi.json');
  expect(spec.ok()).toBeTruthy();
  expect((await spec.json()).openapi).toBe('3.1.0');
  const anonymous = await browser.newContext({ ignoreHTTPSErrors: true, storageState: { cookies: [], origins: [] } });
  const visitor = await anonymous.newPage();
  await visitor.goto('/api-docs');
  await expect(visitor).toHaveURL(/\/login$/);
  await anonymous.close();
});

test('theme follows the system and manual choice persists', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.goto('/');
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(17, 24, 39)');
  await page.getByLabel('Darstellung').selectOption('dark');
  await page.emulateMedia({ colorScheme: 'light' });
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(17, 24, 39)');
  await page.reload();
  await expect(page.getByLabel('Darstellung')).toHaveValue('dark');
  await page.getByLabel('Darstellung').selectOption('light');
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(240, 242, 245)');
  await page.getByLabel('Darstellung').selectOption('auto');
  await page.emulateMedia({ colorScheme: 'dark' });
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(17, 24, 39)');
});

test('clicking a scan row opens its details', async ({ page }) => {
  await waitForSeededScan(page, '/scans');
  const row = page.locator('tr.scan-row').first();
  await expect(row).toBeVisible();
  await expect(row.locator('time')).not.toContainText('T');
  await row.locator('td').first().click();
  await expect(page).toHaveURL(/\/scans\/[0-9a-f]+$/);
  await expect(page.getByRole('heading', { name: 'Scan-Details' })).toBeVisible();
});

test('dashboard totals and error browser describe the stored data', async ({ page }) => {
  await waitForSeededScan(page, '/');
  await expect(page.getByText('Abgeschlossene Dokumente').locator('..')).toContainText('1');
  await expect(page.getByText('Gescannte Seiten').locator('..')).toContainText('2');
  await expect(page.getByText('Aktuell gespeicherte Scan-Daten')).toBeVisible();
  await page.getByRole('link', { name: 'Fehlerprotokoll' }).click();
  await expect(page.getByRole('heading', { name: 'Fehlerprotokoll' })).toBeVisible();
  await expect(page.getByText('Keine Warnungen oder Fehler aufgezeichnet.')).toBeVisible();
  await page.getByLabel('Sprache').selectOption('en');
  await expect(page.getByRole('heading', { name: 'Error log' })).toBeVisible();
});

test('color modes use the selected language on every page', async ({ page }) => {
  await waitForSeededScan(page, '/');
  await expect(page.locator('tr.scan-row').first().locator('td').nth(4)).toHaveText('Farbe (RGB)');
  await expect(page.getByRole('link', { name: 'BrowserTest' }).locator('..')).toContainText('Farbe (RGB)');
  await page.goto('/jobs');
  await expect(page.locator('#jobs-table tbody tr').first().locator('td').nth(3)).toHaveText('Farbe (RGB)');
  await page.goto('/scans');
  await page.locator('tr.scan-row').first().locator('td').first().click();
  await expect(page.locator('tr', { has: page.getByRole('rowheader', { name: 'Farbmodus' }) })).toContainText('Farbe (RGB)');

  await page.getByLabel('Sprache').selectOption('en');
  await expect(page.locator('tr', { has: page.getByRole('rowheader', { name: 'Color mode' }) })).toContainText('Color (RGB)');
  await page.goto('/');
  await expect(page.locator('tr.scan-row').first().locator('td').nth(4)).toHaveText('Color (RGB)');
  await page.goto('/jobs');
  await expect(page.locator('#jobs-table tbody tr').first().locator('td').nth(3)).toHaveText('Color (RGB)');
});

test('Enter opens a focused scan row', async ({ page }) => {
  await waitForSeededScan(page, '/');
  const row = page.locator('tr.scan-row').first();
  await row.focus();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/\/scans\/[0-9a-f]+$/);
});

test('job editing keeps its scan storage without a path field', async ({ page }) => {
  await page.goto('/jobs/0/edit');
  await expect(page.locator('input[name="output_path"]')).toHaveCount(0);
  await page.locator('#jpeg_quality').fill('91');
  await page.getByRole('button', { name: 'Speichern' }).click();
  await expect(page).toHaveURL(/\/jobs$/);
  await waitForSeededScan(page, '/scans');
  await expect(page.locator('tr.scan-row')).toHaveCount(1);
});

test('jobs can be reordered and a row opens its editor', async ({ page }) => {
  await page.goto('/jobs');
  const rows = page.locator('#jobs-table tbody tr.job-row');
  const names = page.locator('#jobs-table tbody td.job-name');
  await expect(names).toHaveText(['BrowserTest', 'SecondJob', 'ThirdJob']);

  await rows.nth(0).locator('.drag-handle').dragTo(rows.nth(2).locator('.drag-handle'));
  await expect(names).toHaveText(['SecondJob', 'ThirdJob', 'BrowserTest']);
  await expect(page.getByRole('status')).toHaveText('Reihenfolge gespeichert.');
  await page.reload();
  await expect(names).toHaveText(['SecondJob', 'ThirdJob', 'BrowserTest']);

  await rows.nth(2).locator('.drag-handle').focus();
  await page.keyboard.press('ArrowUp');
  await expect(names).toHaveText(['SecondJob', 'BrowserTest', 'ThirdJob']);
  await expect(page.getByRole('status')).toHaveText('Reihenfolge gespeichert.');
  await page.reload();
  await expect(names).toHaveText(['SecondJob', 'BrowserTest', 'ThirdJob']);

  await page.getByLabel('Sprache').selectOption('en');
  await expect(page.locator('#job-order-help')).toContainText('Drag the handle');

  await rows.nth(1).locator('td.job-name').click();
  await expect(page).toHaveURL(/\/jobs\/0\/edit$/);
  await expect(page.locator('input[name="name"]')).toHaveValue('BrowserTest');
});

test('failed job reorder restores the saved order', async ({ page }) => {
  await page.goto('/jobs');
  const names = page.locator('#jobs-table tbody td.job-name');
  const original = await names.allTextContents();
  await page.route('**/jobs/reorder', (route) => route.fulfill({ status: 500 }));
  const rows = page.locator('#jobs-table tbody tr.job-row');
  await rows.nth(0).locator('.drag-handle').dragTo(rows.nth(2).locator('.drag-handle'));
  await expect(page.getByRole('status')).toHaveText('Reihenfolge konnte nicht gespeichert werden.');
  await expect(names).toHaveText(original.map((name) => name.trim()));
});

test('admin pages reject missing sessions and cross-origin writes', async ({ browser, page }) => {
  const anonymous = await browser.newContext({ ignoreHTTPSErrors: true, storageState: { cookies: [], origins: [] } });
  const visitor = await anonymous.newPage();
  await visitor.goto('/settings/scanner');
  await expect(visitor).toHaveURL(/\/login$/);
  await anonymous.close();
  const response = await page.request.post('/settings/scanner', {
    headers: { origin: 'https://attacker.invalid' },
    form: { mode: 'none', username: '', new_password: '', admin_password: '', admin_code: '' },
    failOnStatusCode: false,
  });
  expect(response.status()).toBe(403);
});

test('login page honors the saved light and dark choices', async ({ browser }) => {
  const context = await browser.newContext({ ignoreHTTPSErrors: true, storageState: { cookies: [], origins: [] } });
  const page = await context.newPage();
  await page.emulateMedia({ colorScheme: 'light' });
  await page.goto('/login');
  await page.evaluate(() => localStorage.setItem('nx-boss-theme', 'dark'));
  await page.reload();
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(17, 24, 39)');
  await page.evaluate(() => localStorage.setItem('nx-boss-theme', 'light'));
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.reload();
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(238, 241, 245)');
  await page.getByRole('link', { name: 'English' }).click();
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await expect(page.getByRole('heading', { name: 'Admin sign in' })).toBeVisible();
  await context.close();
});

test('scanner password mode can be enabled in the UI and authenticates', async ({ page, request }) => {
  const fixture = JSON.parse(fs.readFileSync('/tmp/nx-boss-browser-totp.json', 'utf8'));
  // Node Buffer has no base32 decoder, so decode the RFC 4648 alphabet locally.
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const char of fixture.secret) bits += alphabet.indexOf(char).toString(2).padStart(5, '0');
  const key = Buffer.from(bits.match(/.{8}/g).map((chunk) => parseInt(chunk, 2)));
  let counter = Math.floor(Date.now() / 30000);
  while (counter <= fixture.used_counter) {
    await new Promise((resolve) => setTimeout(resolve, 500));
    counter = Math.floor(Date.now() / 30000);
  }
  const message = Buffer.alloc(8);
  message.writeBigUInt64BE(BigInt(counter));
  const digest = crypto.createHmac('sha1', key).update(message).digest();
  const offset = digest[19] & 15;
  const code = String((digest.readUInt32BE(offset) & 0x7fffffff) % 1000000).padStart(6, '0');
  await page.goto('/settings/scanner');
  await page.locator('[name=mode]').selectOption('userpass');
  await page.locator('[name=username]').fill('browser-scanner');
  await page.locator('[name=new_password]').fill('browser-test-scanner-password');
  await page.locator('[name=admin_password]').fill('browser test password 2026');
  await page.locator('[name=admin_code]').fill(code);
  await page.getByRole('button', { name: 'Speichern' }).click();
  await expect(page.getByRole('status')).toContainText('Gespeichert');
  const login = await request.post('http://127.0.0.1:12447/NmWebService/authorization', {
    headers: { 'x-forwarded-proto': 'https' },
    data: { auth_type: 'userpass', auth_info_user: 'browser-scanner', auth_info_pass: 'browser-test-scanner-password' },
  });
  expect(login.status()).toBe(200);
  expect((await login.json()).access_token).not.toBe('unused');
});
