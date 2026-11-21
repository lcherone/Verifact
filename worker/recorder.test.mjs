import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter, once } from 'node:events';
import { createServer } from 'node:http';
import { createServer as tcpServer } from 'node:net';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile, symlink, stat } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { chromium } from 'playwright';
import { BrowserWorker } from './browser-worker.mjs';
import { validateStep } from './recorder-ir.mjs';

const html = `<!doctype html><html><body><h1>Recorder fixture</h1>
<form><label for="name">Your name</label><input id="name" autocomplete="off">
<label for="password">Password</label><input id="password" type="password">
<label for="private">Private field</label><input id="private">
<label for="choice">Delivery</label><select id="choice"><option value="standard">Standard</option><option value="express">Express</option></select>
<label><input id="agree" type="checkbox">Agree</label><button>Greet</button></form><output id="greeting">Ready</output>
<button data-testid="counter" id="counter">Count: 0</button><a href="/second">Second page</a>
<button class="ambiguous">Duplicate</button><button class="ambiguous">Duplicate</button>
<script>document.querySelector('form').onsubmit=e=>{e.preventDefault();document.querySelector('#greeting').textContent='Hello, '+document.querySelector('#name').value+'!'};let count=0;document.querySelector('#counter').onclick=e=>{e.target.textContent='Count: '+(++count)};</script>
</body></html>`;
const second = '<!doctype html><h1>Second page</h1><a href="/">Home</a>';
const target = (kind, value) => ({ candidates: [{ kind, value }] });
async function availablePort() {
  const server = tcpServer(); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
async function waitFor(events, predicate, timeout = 5000) {
  return new Promise((resolve, reject) => {
    const handler = value => { if (predicate(value)) { clearTimeout(timer); events.off('recording', handler); resolve(value); } };
    const timer = setTimeout(() => { events.off('recording', handler); reject(new Error('Recording notification deadline exceeded')); }, timeout);
    events.on('recording', handler);
  });
}
async function captured(events, action, predicate) {
  const waiting = waitFor(events, predicate); await action(); return waiting;
}

test('real CDP recording, private persistence, isolated replay, generation and classified failures', { timeout: 180000 }, async t => {
  const dataDir = await mkdtemp(path.join(os.tmpdir(), 'verifact-recorder-'));
  const server = createServer((request, response) => { response.setHeader('Content-Type', 'text/html'); response.end(request.url === '/second' ? second : html); });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const origin = `http://127.0.0.1:${server.address().port}`;
  const port = await availablePort();
  let browser, worker;
  try {
    browser = await chromium.launch({ headless: true, args: [`--remote-debugging-port=${port}`] });
    const page = await browser.newPage(); await page.goto(origin);
    const events = new EventEmitter();
    worker = new BrowserWorker({ dataDir, notify: (method, params) => { if (method === 'recording.steps') events.emit('recording', structuredClone(params.recording)); } });
    await t.test('attach existing page without replacement, normalize fills, pause/resume and explicit expectations', async () => {
      const attached = await worker.dispatch('attach', { endpoint: `http://127.0.0.1:${port}` }); assert.equal(attached.attached, true);
      assert.equal(browser.contexts().flatMap(context => context.pages()).length, 1);
      await worker.dispatch('recording.start', { name: 'Greeting journey', redactSelectors: ['#private'] });
      await captured(events, () => page.getByLabel('Your name').fill('Alice'), r => r.steps.some(s => s.type === 'fill' && s.value === 'Alice'));
      await captured(events, () => page.getByLabel('Your name').fill('Synthetic visitor'), r => r.steps.some(s => s.type === 'fill' && s.value === 'Synthetic visitor'));
      let recording = await worker.dispatch('recording.get'); assert.equal(recording.steps.filter(s => s.type === 'fill').length, 1);
      await worker.dispatch('recording.pause'); await page.getByTestId('counter').click();
      recording = await worker.dispatch('recording.get'); assert.equal(recording.steps.some(s => s.type === 'click'), false);
      await worker.dispatch('recording.resume');
      await captured(events, () => page.getByLabel('Delivery').selectOption('express'), r => r.steps.some(s => s.type === 'select'));
      await captured(events, () => page.getByLabel('Agree').check(), r => r.steps.some(s => s.type === 'check'));
      await captured(events, () => page.getByRole('button', { name: 'Greet', exact: true }).click(), r => r.steps.some(s => s.type === 'click'));
      await worker.dispatch('recording.assertion', { kind: 'text', target: target('css', '#greeting'), expected: 'Hello, Synthetic visitor!' });
      await worker.dispatch('recording.checkpoint', { label: 'Greeting shown' });
      await worker.dispatch('recording.note', { text: 'Synthetic acceptance only' });
      await captured(events, () => page.getByRole('link', { name: 'Second page' }).click(), r => r.steps.some(s => s.type === 'navigate' && s.observed));
      await worker.dispatch('recording.assertion', { kind: 'url', expected: `${origin}/second` });
      await worker.dispatch('recording.stop');
      recording = await worker.dispatch('recording.get'); assert.equal(recording.state, 'completed');
      assert.equal(recording.verification.status, 'unverified');
      assert.equal((await stat(path.join(dataDir, 'recordings', `${recording.id}.json`))).mode & 0o777, 0o600);
      assert.equal((await worker.dispatch('recording.generate_test')).status, 'unverified');
      const replay = await worker.dispatch('recording.replay');
      assert.equal(replay.status, 'passed', JSON.stringify(replay)); assert.equal(replay.assertions, 2); assert.equal(replay.isolated, true);
      assert.equal(page.url(), `${origin}/second`, 'isolated replay does not navigate hosted page');
      assert.equal((await worker.dispatch('recording.get')).verification.status, 'verified');
    });
    await t.test('generated ordinary Playwright test executes independently', async () => {
      const generated = await worker.dispatch('recording.generate_test');
      assert.equal(generated.status, 'unverified', 'emitted source is not promoted solely from IR replay');
      assert.doesNotMatch(generated.source, /waitForTimeout|\.nth\(|mouse\.click/);
      const testDir = path.join(dataDir, 'generated');
      await import('node:fs/promises').then(fs => fs.mkdir(testDir));
      await symlink(new URL('../node_modules', import.meta.url).pathname, path.join(testDir, 'node_modules'));
      await writeFile(path.join(testDir, 'journey.spec.mjs'), generated.source);
      await writeFile(path.join(testDir, 'playwright.config.mjs'), `export default { testDir: '.', outputDir: 'results', reporter: 'line', timeout: 20000, workers: 1 };`);
      const child = spawn(process.execPath, [new URL('../node_modules/playwright/cli.js', import.meta.url).pathname, 'test', '--config', path.join(testDir, 'playwright.config.mjs')], { cwd: testDir, timeout: 45000, stdio: ['ignore', 'pipe', 'pipe'] });
      let output = ''; child.stdout.on('data', chunk => output += chunk); child.stderr.on('data', chunk => output += chunk);
      const [code] = await once(child, 'exit'); assert.equal(code, 0, output); assert.match(output, /1 passed/);
      const verified = await worker.dispatch('recording.verify_generated_test', { timeoutMs: 15000 });
      assert.equal(verified.status, 'passed', JSON.stringify(verified)); assert.equal(verified.sourceHash, generated.sourceHash);
      const nativeReport = JSON.parse(await readFile(verified.reportPath, 'utf8')); assert.equal(nativeReport.stats.expected, 1);
      assert.equal((await worker.dispatch('recording.generate_test')).status, 'verified');
    });
    await t.test('editing invalidates verification; ambiguity and assertion failures retain evidence', async () => {
      let recording = await worker.dispatch('recording.get');
      await worker.dispatch('recording.edit', { steps: [recording.steps[0], { type: 'click', target: target('text', 'Duplicate') }] });
      assert.equal((await worker.dispatch('recording.get')).verification.status, 'unverified');
      let replay = await worker.dispatch('recording.replay', { timeoutMs: 500 });
      assert.equal(replay.status, 'failed'); assert.equal(replay.failure.code, 'ambiguous_selector'); assert.ok((await stat(replay.evidence.path)).size > 100);
      await worker.dispatch('recording.edit', { steps: [recording.steps[0], { type: 'assertion', kind: 'text', target: target('css', '#greeting'), expected: 'Wrong expectation' }] });
      replay = await worker.dispatch('recording.replay', { timeoutMs: 500 }); assert.equal(replay.failure.code, 'assertion_failed');
      await worker.dispatch('recording.edit', { steps: [recording.steps[0], { type: 'click', target: target('css', '#missing') }] });
      replay = await worker.dispatch('recording.replay', { timeoutMs: 200 }); assert.equal(replay.failure.code, 'selector_not_found');
      await worker.dispatch('recording.edit', { steps: [recording.steps[0]] });
      replay = await worker.dispatch('recording.replay'); assert.equal(replay.status, 'unverified', 'actions without assertions never verify');
      await assert.rejects(worker.dispatch('recording.edit', { steps: [{ type: 'evaluate', code: 'process.exit()' }] }), { code: 'invalid_step' });
    });
    await t.test('sensitive inputs use secretRefs, configured fields redact, no plaintext in persisted JSON/source', async () => {
      await worker.dispatch('browser.navigate', { url: origin });
      await worker.dispatch('recording.start', { name: 'Secret journey', redactSelectors: ['#private'] });
      await captured(events, () => page.getByLabel('Password', { exact: true }).fill('Fixture-Password-Do-Not-Persist'), r => r.steps.some(s => s.secretRef));
      await captured(events, () => page.getByLabel('Private field', { exact: true }).fill('Fixture-Private-Do-Not-Persist'), r => r.steps.filter(s => s.secretRef).length === 2);
      let recording = await worker.dispatch('recording.stop');
      assert.equal(recording.steps.filter(s => s.type === 'fill').every(s => s.secretRef && !('value' in s)), true);
      const bytes = await readFile(path.join(dataDir, 'recordings', `${recording.id}.json`), 'utf8'); assert.doesNotMatch(bytes, /Fixture-Password|Fixture-Private/);
      const generated = await worker.dispatch('recording.generate_test'); assert.doesNotMatch(generated.source, /Fixture-Password|Fixture-Private/); assert.match(generated.source, /process\.env/);
      let replay = await worker.dispatch('recording.replay', { timeoutMs: 500 }); assert.equal(replay.failure.code, 'missing_secret');
      for (const step of recording.steps.filter(s => s.secretRef)) process.env[step.secretRef] = 'Disposable fixture secret';
      replay = await worker.dispatch('recording.replay'); assert.equal(replay.status, 'unverified');
      for (const step of recording.steps.filter(s => s.secretRef)) delete process.env[step.secretRef];
      const screenshot = await worker.dispatch('browser.screenshot'); assert.equal(screenshot.redacted, true); assert.ok((await stat(screenshot.path)).size > 100);
      await worker.dispatch('browser.viewport', { width: 800, height: 600 });
      assert.equal(await page.evaluate(() => innerWidth), 800);
      const snapshot = await worker.dispatch('browser.snapshot'); assert.ok(snapshot.nodes.some(node => node.role === 'button'));
      assert.doesNotMatch(JSON.stringify(snapshot), /Fixture-Password|Fixture-Private/);
      assert.equal(snapshot.redacted, true);
      await worker.dispatch('browser.trace', { action: 'start' }); await page.reload();
      const trace = await worker.dispatch('browser.trace', { action: 'stop' });
      assert.equal(trace.format, 'verifact-browser-metadata-trace'); assert.ok(trace.eventCount > 0);
      assert.doesNotMatch(await readFile(trace.path, 'utf8'), /Fixture-Password|Fixture-Private/);
      await worker.dispatch('browser.viewport', { reset: true });
      await assert.rejects(worker.dispatch('recording.edit', { steps: [{ type: 'fill', target: target('css', '#password'), value: 'unsafe' }] }), { code: 'sensitive_input' });
      await assert.rejects(worker.dispatch('browser.navigate', { url: 'javascript:alert(1)' }), { code: 'invalid_url' });
    });
    await t.test('generated verification failure keeps private reports and redacted artifacts free of entered secrets', async () => {
      const secret = 'Verifier-Failure-Secret-Do-Not-Persist';
      process.env.VERIFACT_SECRET_TEST_PRIVATE = secret;
      try {
        await worker.dispatch('recording.edit', { steps: [
          { type: 'navigate', url: origin },
          { type: 'fill', target: target('css', '#name'), secretRef: 'VERIFACT_SECRET_TEST_PRIVATE' },
          { type: 'click', target: { candidates: [{ kind: 'role', role: 'button', name: 'Greet' }] } },
          { type: 'assertion', kind: 'text', target: target('css', '#greeting'), expected: 'Intentionally wrong expectation' },
        ] });
        const result = await worker.dispatch('recording.verify_generated_test', { timeoutMs: 1200 });
        assert.equal(result.status, 'failed', JSON.stringify(result));
        assert.ok(result.evidence.some(item => item.kind === 'screenshot' && item.redacted));
        assert.equal((await worker.dispatch('recording.generate_test')).status, 'unverified');
        async function inspect(directory) {
          const entries = await import('node:fs/promises').then(fs => fs.readdir(directory, { withFileTypes: true }));
          for (const entry of entries) {
            if (entry.isSymbolicLink()) continue;
            const filename = path.join(directory, entry.name);
            if (entry.isDirectory()) await inspect(filename);
            else if (!filename.endsWith('.png')) assert.equal((await readFile(filename, 'utf8')).includes(secret), false, filename);
          }
        }
        await inspect(path.dirname(result.reportPath));
      } finally { delete process.env.VERIFACT_SECRET_TEST_PRIVATE; }
    });
    await t.test('history survives new store, and worker disconnect preserves hosted page', async () => {
      const summaries = await worker.dispatch('recording.list'); assert.equal(summaries.recordings.length, 2);
      const id = (await worker.dispatch('recording.get')).id;
      assert.equal((await worker.dispatch('recording.list', { projectId: 'not-a-project' })).recordings.length, 0);
      assert.equal((await worker.dispatch('recording.load', { id })).id, id);
      await worker.close(); worker = null;
      assert.equal(page.isClosed(), false); assert.equal(await page.title(), '');
      const reopened = new BrowserWorker({ dataDir }); assert.equal((await reopened.dispatch('recording.get', { id })).id, id);
      await reopened.dispatch('recording.discard', { id }); assert.equal((await reopened.dispatch('recording.list')).recordings.length, 1);
      await assert.rejects(reopened.dispatch('recording.get', { id: '../../etc/passwd' }), { code: 'invalid_id' });
      await reopened.close();
    });
  } finally {
    if (worker) await worker.close(); if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve)); await rm(dataDir, { recursive: true, force: true });
  }
});

test('JSON lines errors and EOF are bounded and graceful', { timeout: 20000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'verifact-protocol-'));
  try {
    const scriptAlias = path.join(directory, 'worker-alias.mjs');
    await symlink(new URL('./browser-worker.mjs', import.meta.url).pathname, scriptAlias);
    const child = spawn(process.execPath, [scriptAlias, '--data-dir', directory], { stdio: ['pipe', 'pipe', 'pipe'], timeout: 10000 });
    let output = ''; child.stdout.on('data', bytes => output += bytes);
    child.stdin.end('{broken\n' + JSON.stringify({ id: 1, method: 'browser.status' }) + '\n' + JSON.stringify({ id: 2, method: 'unknown' }) + '\n');
    const [code] = await once(child, 'exit'); assert.equal(code, 0);
    const responses = output.trim().split('\n').map(line => JSON.parse(line));
    assert.equal(responses[0].error.code, 'parse_error'); assert.equal(responses[1].result.attached, false); assert.equal(responses[2].error.code, 'method_not_found');
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('worker EOF and SIGTERM disconnect CDP without closing the hosted page, endpoint or browser', { timeout: 40000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'verifact-worker-lifecycle-'));
  const port = await availablePort();
  let browser;
  const children = [];
  try {
    browser = await chromium.launch({ headless: true, args: [`--remote-debugging-port=${port}`] });
    const page = await browser.newPage(); await page.goto('data:text/html,<h1>Hosted page survives</h1>');
    for (const signal of ['EOF', 'SIGTERM']) {
      const child = spawn(process.execPath, [new URL('./browser-worker.mjs', import.meta.url).pathname, '--data-dir', directory], { stdio: ['pipe', 'pipe', 'pipe'], timeout: 10000 });
      children.push(child);
      let buffer = '';
      const responses = new EventEmitter();
      child.stdout.on('data', bytes => {
        buffer += bytes;
        let newline;
        while ((newline = buffer.indexOf('\n')) >= 0) {
          const message = JSON.parse(buffer.slice(0, newline)); buffer = buffer.slice(newline + 1); responses.emit('response', message);
        }
      });
      const received = new Promise((resolve, reject) => {
        const deadline = setTimeout(() => reject(new Error('Attach response missing')), 5000);
        responses.on('response', message => { if (message.id === 1) { clearTimeout(deadline); resolve(message); } });
      });
      child.stdin.write(JSON.stringify({ id: 1, method: 'attach', params: { endpoint: `http://127.0.0.1:${port}` } }) + '\n');
      assert.equal((await received).result.attached, true);
      const exited = once(child, 'exit');
      if (signal === 'EOF') child.stdin.end(); else child.kill(signal);
      const [code] = await exited; assert.equal(code, 0, `${signal} exits cleanly`);
      assert.equal(browser.isConnected(), true); assert.equal(page.isClosed(), false);
      assert.equal(await page.getByRole('heading').innerText(), 'Hosted page survives');
      assert.equal((await fetch(`http://127.0.0.1:${port}/json/version`)).status, 200, 'native-owned endpoint remains available');
    }
  } finally {
    for (const child of children) if (child.exitCode === null) child.kill('SIGKILL');
    if (browser) await browser.close(); await rm(directory, { recursive: true, force: true });
  }
});

test('crash recovery, malformed files, size limits and literal-secret editing fail closed', { timeout: 10000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'verifact-recorder-recovery-'));
  try {
    const worker = new BrowserWorker({ dataDir: directory });
    const { randomUUID } = await import('node:crypto');
    const recording = { schemaVersion: 1, id: randomUUID(), name: 'Interrupted synthetic journey', revision: 1, state: 'recording', createdAt: new Date().toISOString(), updatedAt: new Date().toISOString(), steps: [{ id: randomUUID(), type: 'navigate', url: 'http://127.0.0.1:4179/', observed: false }], verification: { status: 'unverified' } };
    await worker.store.save(recording);
    const recovered = await worker.dispatch('recording.load', { id: recording.id });
    assert.equal(recovered.state, 'interrupted'); assert.equal(recovered.recovery.recoverable, true);
    await assert.rejects(worker.dispatch('recording.replay', { id: recording.id }), { code: 'invalid_state' });
    const reviewed = await worker.dispatch('recording.edit', { id: recording.id, name: 'Reviewed preserved journey' }); assert.equal(reviewed.state, 'completed');
    const brokenId = randomUUID(); await writeFile(worker.store.file(brokenId), '{broken', { mode: 0o600 });
    const oversizedId = randomUUID(); await writeFile(worker.store.file(oversizedId), 'x'.repeat(2 * 1024 * 1024 + 1), { mode: 0o600 });
    for (const id of [brokenId, oversizedId]) await assert.rejects(worker.dispatch('recording.get', { id }), { code: 'corrupt_recording' });
    const list = await worker.dispatch('recording.list'); assert.equal(list.recordings.length, 3); assert.equal(list.recordings.filter(r => r.state === 'corrupt').length, 2);
    assert.equal(await readFile(worker.store.file(brokenId), 'utf8'), '{broken', 'corrupt source is preserved');
    for (const value of ['4111 1111 1111 1111', 'Bearer synthetic-authentication-token', 'sk-synthetic-secret-key']) {
      assert.throws(() => validateStep({ type: 'fill', target: target('css', '#ordinary'), value }), { code: 'sensitive_input' });
    }
    assert.throws(() => validateStep({ type: 'assertion', kind: 'value', target: target('css', '#password'), expected: 'private-password' }), { code: 'sensitive_input' });
    await worker.close();
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('SIGUSR1 and JSON-lines fast-path cancel replay, persist partial evidence and keep worker usable', { timeout: 45000 }, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'verifact-replay-cancel-'));
  const server = createServer((request, response) => { response.setHeader('Content-Type', 'text/html'); response.end('<h1>Cancellation fixture</h1>'); });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const origin = `http://127.0.0.1:${server.address().port}`;
  let child;
  try {
    const { randomUUID } = await import('node:crypto');
    const prepared = new BrowserWorker({ dataDir: directory });
    const missing = { id: randomUUID(), type: 'click', target: target('css', '#never-present') };
    const recording = { schemaVersion: 1, id: randomUUID(), name: 'Cancellable synthetic journey', revision: 1, state: 'completed', createdAt: new Date().toISOString(), updatedAt: new Date().toISOString(), steps: [validateStep({ type: 'navigate', url: origin }), missing], verification: { status: 'unverified' } };
    await prepared.store.save(recording); await prepared.close();
    child = spawn(process.execPath, [new URL('./browser-worker.mjs', import.meta.url).pathname, '--data-dir', directory], { stdio: ['pipe', 'pipe', 'pipe'], timeout: 30000 });
    const pending = new Map(); const events = new EventEmitter(); let sequence = 0; let buffer = '';
    child.stdout.on('data', bytes => {
      buffer += bytes;
      let index;
      while ((index = buffer.indexOf('\n')) >= 0) {
        const value = JSON.parse(buffer.slice(0, index)); buffer = buffer.slice(index + 1);
        if (value.id !== undefined) { pending.get(value.id)?.(value); pending.delete(value.id); }
        else events.emit('notification', value);
      }
    });
    function rpc(method, params = {}) {
      const id = ++sequence;
      return new Promise(resolve => { pending.set(id, resolve); child.stdin.write(JSON.stringify({ id, method, params }) + '\n'); });
    }
    function pendingStep() {
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('Replay did not reach pending step')), 7000);
        const listener = value => { if (value.method === 'recording.replay' && value.params.stepId === missing.id) { clearTimeout(timer); events.off('notification', listener); resolve(); } };
        events.on('notification', listener);
      });
    }
    for (const method of ['SIGUSR1', 'RPC']) {
      const reached = pendingStep(); const replay = rpc('recording.replay', { id: recording.id, timeoutMs: 30000 });
      await reached;
      if (method === 'SIGUSR1') child.kill('SIGUSR1');
      else assert.equal((await rpc('recording.cancel_replay')).result.cancelled, true);
      const result = (await replay).result; assert.equal(result.status, 'cancelled', JSON.stringify(result));
      assert.equal(result.steps[0].status, 'passed'); assert.equal(result.steps.at(-1).status, 'cancelled');
      assert.ok((await stat(result.evidence.path)).size > 100);
      assert.equal((await rpc('recording.get', { id: recording.id })).result.lastReplay.status, 'cancelled');
      assert.equal((await rpc('browser.status')).result.attached, false, 'worker still responds after cancellation');
    }
    const requested = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Generated test did not open its fresh browser')), 7000);
      server.once('request', () => { clearTimeout(timer); resolve(); });
    });
    const generated = rpc('recording.verify_generated_test', { id: recording.id, timeoutMs: 30000 });
    await requested; child.kill('SIGUSR1');
    const generatedResult = (await generated).result;
    assert.equal(generatedResult.status, 'cancelled', JSON.stringify(generatedResult));
    assert.equal((await rpc('browser.status')).result.attached, false, 'worker remains usable after cancelling Playwright Test');
    const exited = once(child, 'exit'); child.stdin.end(); assert.equal((await exited)[0], 0);
  } finally {
    if (child?.exitCode === null) child.kill('SIGKILL');
    await new Promise(resolve => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});
