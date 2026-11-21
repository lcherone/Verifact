import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright';
import { startFixture } from './fixture.mjs';

const endpoint = 'http://127.0.0.1:9227';
const executable = new URL('../target/bundle/Verifact.app/Contents/MacOS/verifact', import.meta.url).pathname;
const evidence = new URL('../evidence/', import.meta.url).pathname;
const report = { startedAt: new Date().toISOString(), platform: process.platform, architecture: process.arch, runs: [] };

async function waitForEndpoint(app) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    if (app.exitCode !== null || app.signalCode !== null) throw new Error(`App exited before CDP: ${app.exitCode ?? app.signalCode}`);
    try {
      const response = await fetch(`${endpoint}/json/version`);
      if (response.ok) return await response.json();
    } catch {}
    await delay(100);
  }
  throw new Error('CEF DevTools endpoint did not become ready within 30 seconds');
}

await mkdir(evidence, { recursive: true });
try {
  await fetch(`${endpoint}/json/version`);
  throw new Error('Port 9227 is occupied. Close the interactive spike before verification.');
} catch (error) {
  if (error.message.includes('occupied')) throw error;
}
const fixture = await startFixture();
try {
  for (let run = 1; run <= 3; run++) {
    const record = { run, status: 'running', checks: [] };
    report.runs.push(record);
    const app = spawn(executable, [], {
      cwd: new URL('../', import.meta.url),
      env: { ...process.env, VERIFACT_BROWSER_ONLY: '1' },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let logs = '';
    app.stdout.on('data', bytes => { logs += bytes; });
    app.stderr.on('data', bytes => { logs += bytes; });
    const exited = once(app, 'exit');
    const watchdog = setTimeout(() => app.kill('SIGTERM'), 60000);
    let browser;
    try {
      record.version = await waitForEndpoint(app);
      browser = await chromium.connectOverCDP(endpoint);
      const contexts = browser.contexts();
      assert.equal(contexts.length, 1);
      const context = contexts[0];
      const pages = context.pages();
      assert.equal(pages.length, 1, 'Attach to exactly the page created by the native CEF host');
      const page = pages[0];
      page.setDefaultTimeout(10000);
      await page.waitForURL('http://127.0.0.1:4179/');
      await page.getByRole('heading', { name: 'A real browser inside GPUI' }).waitFor();
      record.checks.push('Attached to existing embedded page; no browser/page launched by Playwright');
      const consoleEvents = [];
      const requests = [];
      page.on('console', message => consoleEvents.push({ type: message.type(), text: message.text() }));
      page.on('request', request => requests.push({ method: request.method(), url: request.url() }));
      await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
      await page.getByRole('button', { name: 'Greet', exact: true }).click();
      assert.equal(await page.locator('#name').evaluate(input => input.validity.valueMissing), true);
      assert.equal(await page.locator('#greeting').innerText(), 'Ready for input');
      record.checks.push('Native HTML form rejects empty required input');
      const name = `Playwright run ${run}`;
      await page.getByRole('textbox', { name: 'Your name' }).fill(name);
      await page.getByRole('textbox', { name: 'Your name' }).press('End');
      await page.getByRole('textbox', { name: 'Your name' }).press('!');
      const response = page.waitForResponse('**/api/greeting');
      await page.getByRole('button', { name: 'Greet', exact: true }).click();
      assert.equal((await response).status(), 200);
      assert.equal(await page.locator('#greeting').innerText(), `Hello, ${name}!!`);
      await page.getByRole('button', { name: 'Count: 0' }).click();
      assert.equal(await page.locator('#counter').innerText(), 'Count: 1');
      await page.getByLabel('Browser input').selectOption('Keyboard');
      assert.equal(await page.getByLabel('Browser input').inputValue(), 'Keyboard');
      record.checks.push('Playwright locator clicks, fill, keypress, select, DOM assertions and network response');
      await page.getByRole('link', { name: 'Open second page', exact: true }).click();
      await page.waitForURL('**/second');
      assert.equal(await page.locator('#saved').innerText(), `Hello, ${name}!!`);
      // A restored history document may not emit another load event over CDP.
      // Check navigation commitment and the actual restored DOM instead.
      await page.goBack({ waitUntil: 'commit' });
      await page.waitForURL('http://127.0.0.1:4179/', { waitUntil: 'commit' });
      await page.getByRole('heading', { name: 'A real browser inside GPUI' }).waitFor();
      await page.goForward({ waitUntil: 'commit' });
      await page.waitForURL('**/second', { waitUntil: 'commit' });
      await page.getByRole('heading', { name: 'Second page' }).waitFor();
      await page.reload();
      assert.equal(await page.locator('#saved').innerText(), `Hello, ${name}!!`);
      record.checks.push('Navigation, history, reload and origin storage persistence');
      await page.getByRole('link', { name: 'Return to form' }).click();
      await page.locator('#bottom').scrollIntoViewIfNeeded();
      assert.ok(await page.evaluate(() => window.scrollY > 0));
      await page.evaluate(() => window.scrollTo(0, 0));
      record.viewport = await page.evaluate(() => ({ width: innerWidth, height: innerHeight, scale: devicePixelRatio }));
      await page.screenshot({ path: `${evidence}browser-run-${run}.png` });
      await writeFile(`${evidence}dom-run-${run}.html`, await page.content());
      record.ariaSnapshot = await page.locator('main').ariaSnapshot();
      assert.ok(consoleEvents.some(event => event.text === 'fixture:greeted'));
      assert.ok(requests.some(request => request.method === 'POST' && request.url.endsWith('/api/greeting')));
      record.console = consoleEvents;
      record.requests = requests;
      await context.tracing.stop({ path: `${evidence}trace-run-${run}.zip` });
      record.checks.push('Scroll, screenshot, DOM, accessibility snapshot, console/network evidence and trace');
      await page.close();
      await Promise.race([exited, delay(15000).then(() => { throw new Error('Native app did not exit after browser close'); })]);
      assert.equal(app.exitCode, 0, logs);
      assert.ok(logs.includes('CEF_BROWSER_CLOSED'));
      assert.ok(logs.includes('CEF_SHUTDOWN_COMPLETE'));
      await assert.rejects(fetch(`${endpoint}/json/version`));
      record.checks.push('Browser close callback, CEF shutdown, process exit 0 and CDP port released');
      record.status = 'passed';
      console.log(`Run ${run}: passed (${record.checks.length} groups)`);
    } catch (error) {
      record.status = 'failed';
      record.error = error.stack;
      throw error;
    } finally {
      clearTimeout(watchdog);
      await browser?.close().catch(() => {});
      if (app.exitCode === null && app.signalCode === null) app.kill('SIGTERM');
      await writeFile(`${evidence}app-run-${run}.log`, logs);
      await writeFile(`${evidence}verification.json`, JSON.stringify(report, null, 2));
    }
  }
  report.status = 'passed';
} catch (error) {
  report.status = 'failed';
  console.error(error);
  process.exitCode = 1;
} finally {
  await new Promise(resolve => fixture.close(resolve));
  report.finishedAt = new Date().toISOString();
  await writeFile(`${evidence}verification.json`, JSON.stringify(report, null, 2));
}
