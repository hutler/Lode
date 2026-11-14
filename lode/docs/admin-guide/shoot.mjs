// Screenshots of the admin web UI for the user guide.
//
// Start the demo server, then run this with its address:
//
//   cargo run -p lode-testkit --example admin_demo
//   node docs/admin-guide/shoot.mjs http://127.0.0.1:PORT
//
// Needs Playwright with Chromium. Writes PNGs next to this file: every page in light at
// 1280 wide, a few in dark, and a few at phone width.
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

// Through require, so a global install found by NODE_PATH works too.
const { chromium } = createRequire(import.meta.url)('playwright');

const base = process.argv[2];
if (!base) {
  console.error('usage: node shoot.mjs http://127.0.0.1:PORT');
  process.exit(2);
}
const out = path.dirname(fileURLToPath(import.meta.url));
const browser = await chromium.launch();

async function session(scheme, viewport) {
  const context = await browser.newContext({ colorScheme: scheme, viewport, deviceScaleFactor: 1 });
  const page = await context.newPage();
  page.on('dialog', (d) => d.accept());
  return { context, page };
}

async function signIn(page) {
  await page.goto(`${base}/admin/sign-in`);
  await page.fill('input[name=name]', 'admin');
  await page.fill('input[name=password]', 'demo-password');
  await Promise.all([page.waitForNavigation(), page.click('button[type=submit]')]);
}

async function shot(page, name) {
  // Times and sizes move; the shots show the layout.
  await page.screenshot({ path: path.join(out, `${name}.png`), fullPage: true });
  console.log(name);
}

async function submit(page, action, fill = async () => {}) {
  const form = page.locator(`form[action="${action}"]`).first();
  await fill(form);
  await Promise.all([page.waitForLoadState('load'), form.locator('button[type=submit]').click()]);
  await page.waitForLoadState('load');
}

// Pages shown in every theme and width.
const pages = [
  ['overview', '/admin/'],
  ['depots', '/admin/depots'],
  ['depot', '/admin/depots/game'],
  ['people', '/admin/people'],
  ['user', '/admin/people/ana'],
  ['access-check', '/admin/access?principal=ben&depot=game&branch=main&path=Content%2Fhero.psd#check'],
  ['webhooks', '/admin/webhooks'],
  ['settings', '/admin/settings'],
];

for (const [scheme, viewport, suffix, only] of [
  ['light', { width: 1280, height: 800 }, 'light', null],
  ['dark', { width: 1280, height: 800 }, 'dark', ['overview', 'depot', 'access-check']],
  ['light', { width: 390, height: 844 }, 'phone', ['overview', 'depot', 'access-check']],
]) {
  const { context, page } = await session(scheme, viewport);
  if (suffix === 'light') {
    await page.goto(`${base}/admin/sign-in`);
    await shot(page, 'sign-in-light');
  }
  await signIn(page);
  for (const [name, url] of pages) {
    if (only && !only.includes(name)) continue;
    await page.goto(`${base}${url}`);
    await shot(page, `${name}-${suffix}`);
  }
  if (suffix === 'light') {
    // Reports and one-time values.
    await page.goto(`${base}/admin/depots/game`);
    await submit(page, '/admin/depots/game/gc');
    await shot(page, 'gc-report-light');
    await page.goto(`${base}/admin/depots/game`);
    await submit(page, '/admin/depots/game/obliterate', async (f) => {
      await f.locator('textarea[name=targets]').fill('Content/hero.psd@2');
      await f.locator('input[name=reason]').fill('Old artwork under a licence that ended');
    });
    await shot(page, 'obliterate-dry-run-light');
    await page.goto(`${base}/admin/people/ana`);
    await submit(page, '/admin/people/ana/tokens', async (f) => {
      await f.locator('input[name=name]').fill('laptop');
      await f.locator('input[name=days]').fill('90');
    });
    await shot(page, 'token-created-light');
    const hook = await page.goto(`${base}/admin/webhooks`).then(() =>
      page.locator('table a[href^="/admin/webhooks/"]').first().getAttribute('href'));
    await page.goto(`${base}${hook}`);
    await shot(page, 'webhook-light');
  }
  await context.close();
}
await browser.close();
