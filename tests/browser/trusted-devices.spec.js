const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { spawn, spawnSync } = require('child_process');
const origin = 'https://localhost:12649';
let directory, app, proxy, client, authenticator;
const startApp = () => spawn('./target/debug/nx-boss-rs', [
  '--config', path.join(directory, 'config.yaml'), '--host', '127.0.0.1', '--port', '12647',
  '--admin-host', '127.0.0.1', '--admin-port', '12648', '--admin-allowed-host', 'localhost',
], { env: { ...process.env, NX_BOSS_AUTH_PEPPER_FILE: path.join(directory, 'pepper'), NX_BOSS_ADMIN_DOMAIN: 'localhost', NX_BOSS_ADMIN_HTTPS_PORT: '12649' }, stdio: 'ignore' });
async function waitFor() {
  for (let n=0;n<100;n++) { try { if ((await fetch('http://127.0.0.1:12647/health')).ok) return; } catch (_) {} await new Promise(r=>setTimeout(r,100)); }
  throw new Error('isolated trust server not healthy');
}
async function restart() { const exit = new Promise(r=>app.once('exit',r)); app.kill('SIGTERM'); await exit; app=startApp(); await waitFor(); }
test.use({storageState:{cookies:[],origins:[]}});
test.beforeEach(async ({page,context})=>{
  directory=fs.mkdtempSync(path.join(os.tmpdir(),'nx-trust-')); fs.mkdirSync(path.join(directory,'scans'));
  fs.writeFileSync(path.join(directory,'config.yaml'),`jobs:\n  Test:\n    output_path: ${directory}/scans\n`);
  fs.writeFileSync(path.join(directory,'pepper'),crypto.randomBytes(32),{mode:0o600});
  app=startApp(); await waitFor();
  const cert=path.join(directory,'cert.pem'),key=path.join(directory,'key.pem');
  if(spawnSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',key,'-out',cert,'-days','1','-subj','/CN=localhost','-addext','subjectAltName=DNS:localhost'],{stdio:'ignore'}).status!==0) throw Error('test certificate');
  proxy=spawn('python3',['scripts/test_https_proxy.py',cert,key,'12649','12648','localhost:12649'],{stdio:'ignore'});
  client=await context.newCDPSession(page); await client.send('WebAuthn.enable');
  ({authenticatorId:authenticator}=await client.send('WebAuthn.addVirtualAuthenticator',{options:{protocol:'ctap2',transport:'internal',hasResidentKey:true,hasUserVerification:true,isUserVerified:true,automaticPresenceSimulation:true}}));
  await expect.poll(async()=>{try{return (await context.request.get(`${origin}/setup`)).status();}catch(_){return 0;}},{timeout:15000}).toBe(200);
  await page.goto(`${origin}/setup`);
  await expect(page.locator('[name=trust_device]')).toHaveCount(0);
  await page.locator('[name=username]').fill('trust-admin');
  await page.locator('[name=token]').fill(fs.readFileSync(path.join(directory,'admin-setup-token'),'utf8'));
  await page.locator('button[type=submit]').click(); await expect(page).toHaveURL(`${origin}/`);
  await context.request.post(`${origin}/logout`,{headers:{origin}});
});
test.afterEach(async()=>{
  proxy?.kill();
  if(app && app.exitCode===null){const exit=new Promise(r=>app.once('exit',r));app.kill('SIGTERM');await exit;}
  fs.rmSync(directory,{recursive:true,force:true});
});

test('trusted device controls are opt-in and localized on mobile in dark mode',async({page})=>{
  await page.setViewportSize({width:375,height:812}); await page.emulateMedia({colorScheme:'dark'});
  for(const lang of ['de','en']) {
    await page.goto(`${origin}/language/${lang}?next=/login`);
    const option=page.getByLabel(lang==='de'?'Diesem Gerät 30 Tage vertrauen':'Trust this device for 30 days',{exact:true});
    await expect(option).not.toBeChecked({timeout:1500});
    await expect(page.locator('[name=device_name]')).toBeVisible();
    expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
    await page.screenshot({path:`/tmp/nx-trust-${lang}-mobile-dark.png`});
  }
});

test('trusted sessions persist, expire, require step-up and can be revoked', async ({ page, context, browser }) => {
  test.setTimeout(90000);
  const storePath=path.join(directory,'admin-trusted-devices.json');
  const devices=()=>JSON.parse(fs.readFileSync(storePath,'utf8')).devices;
  const cookie=async()=> (await context.cookies(origin)).find(c=>c.name==='__Host-nx-trust');
  const login=async(trusted,name='Laptop')=>{
    await page.goto(`${origin}/login`);
    if(trusted) await page.locator('[name=trust_device]').check();
    await page.locator('[name=device_name]').fill(name);
    await page.getByRole('button',{name:'Mit Passkey anmelden',exact:true}).click();
    await expect(page).toHaveURL(`${origin}/`);
  };
  await login(false);
  expect(Boolean(await cookie())).toBe(false);
  await context.request.post(`${origin}/logout`,{headers:{origin}});
  // An unknown old cookie must not erase the newly issued cookie on the finish response.
  await page.goto(`${origin}/login`);
  await context.addCookies([{name:'__Host-nx-trust',value:'f'.repeat(64),url:origin,secure:true,httpOnly:true,sameSite:'Strict'}]);
  await page.locator('[name=trust_device]').check();await page.locator('[name=device_name]').fill('Laptop');
  await page.getByRole('button',{name:'Mit Passkey anmelden',exact:true}).click();await expect(page).toHaveURL(`${origin}/`);
  const trusted=await cookie();expect(Boolean(trusted)).toBe(true);
  expect(trusted.secure).toBe(true);expect(trusted.httpOnly).toBe(true);expect(trusted.sameSite).toBe('Strict');expect(trusted.path).toBe('/');
  expect(Math.abs(trusted.expires-Date.now()/1000-30*86400)).toBeLessThan(10);
  expect(fs.readFileSync(storePath,'utf8').includes(trusted.value)).toBe(false);
  expect(fs.statSync(storePath).mode&0o777).toBe(0o600);
  expect(devices()[0].name).toBe('Laptop');
  expect(Date.parse(devices()[0].expires_at)-Date.parse(devices()[0].created_at)).toBe(30*86400000);
  let passkeyRequests=0;page.on('request',r=>{if(new URL(r.url()).pathname.startsWith('/passkey/'))passkeyRequests++;});
  const unchanged=fs.readFileSync(storePath,'utf8');
  await context.clearCookies({name:'__Host-nx-admin'});
  // The trusted bearer alone cannot authorize a form action.
  expect((await context.request.post(`${origin}/settings/language`,{headers:{origin},form:{lang:'en'}})).status()).toBe(401);
  await page.goto(`${origin}/account`);await expect(page).toHaveURL(`${origin}/account`);
  await expect(page.getByText('Dieses Gerät',{exact:true})).toBeVisible();expect(passkeyRequests).toBe(0);
  expect(fs.readFileSync(storePath,'utf8')===unchanged).toBe(true);
  expect((await context.request.post(`${origin}/settings/scanner`,{headers:{origin},form:{mode:'none',username:'',new_password:''}})).status()).toBe(401);
  await restart();await page.goto(`${origin}/account`);await expect(page).toHaveURL(`${origin}/account`);expect(passkeyRequests).toBe(0);
  expect((await cookie()).expires).toBe(trusted.expires);
  // Normal passkey-management still requires a real assertion; removal revokes trust.
  await page.locator('[name=passkey_name]').fill('Backup');
  let backup;
  await page.route('**/passkey/account/add/start',async route=>{
    ({authenticatorId:backup}=await client.send('WebAuthn.addVirtualAuthenticator',{options:{protocol:'ctap2',transport:'usb',hasResidentKey:true,hasUserVerification:true,isUserVerified:true,automaticPresenceSimulation:true}}));
    await route.continue();
  });
  await page.locator('[data-passkey-add]').click();await expect(page.getByText('Backup',{exact:true})).toBeVisible();
  await page.unroute('**/passkey/account/add/start');await client.send('WebAuthn.removeVirtualAuthenticator',{authenticatorId:backup});
  const backupRow=page.locator('tr').filter({has:page.getByText('Backup',{exact:true})});
  await backupRow.locator('[data-passkey-remove]').click();await expect(page.getByText('Backup',{exact:true})).toHaveCount(0);
  expect(devices().length).toBe(0);expect(Boolean(await cookie())).toBe(false);
  const visitor=await browser.newContext({ignoreHTTPSErrors:true,storageState:{cookies:[trusted],origins:[]}});
  const visit=await visitor.newPage();await visit.goto(`${origin}/account`);await expect(visit).toHaveURL(`${origin}/login`);await visitor.close();
  await login(true,'Current laptop');await context.clearCookies({name:'__Host-nx-admin'});await page.goto(`${origin}/account`);
  const issued=await cookie();
  await page.locator('[data-trusted-remove]').click();await expect(page).toHaveURL(`${origin}/login`);
  expect(devices().length).toBe(0);expect(Boolean(await cookie())).toBe(false);
  const revoked=await browser.newContext({ignoreHTTPSErrors:true,storageState:{cookies:[issued],origins:[]}});const rejected=await revoked.newPage();await rejected.goto(`${origin}/account`);await expect(rejected).toHaveURL(`${origin}/login`);await revoked.close();
  await login(true,'All devices');await context.clearCookies({name:'__Host-nx-admin'});await page.goto(`${origin}/account`);
  await page.locator('[data-trusted-remove-all]').click();await expect(page).toHaveURL(`${origin}/login`);expect(devices().length).toBe(0);
  await login(true,'Logout');await context.request.post(`${origin}/logout`,{headers:{origin}});expect(devices().length).toBe(0);expect(Boolean(await cookie())).toBe(false);
  await login(true,'Expiry');
  const file=JSON.parse(fs.readFileSync(storePath,'utf8'));
  const day31=Date.now()-31*86400000;
  for(const device of file.devices){device.created_at=new Date(day31).toISOString();device.last_used_at=device.created_at;device.expires_at=new Date(day31+30*86400000).toISOString();}
  fs.writeFileSync(storePath,JSON.stringify(file));await restart();
  await page.goto(`${origin}/account`);await expect(page).toHaveURL(`${origin}/login`);expect(Boolean(await cookie())).toBe(false);
  fs.writeFileSync(storePath,'broken-json');await restart();
  await login(false);expect(Boolean(await cookie())).toBe(false);
});
