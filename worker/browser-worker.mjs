import { createInterface } from 'node:readline';
import { randomUUID, createHash } from 'node:crypto';
import { mkdir, chmod, writeFile, realpath } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';
import { verifyGenerated } from './recorder-verification.mjs';
import { structuralSnapshot, browserTrace, traceEvent, viewport } from './recorder-evidence.mjs';
import { installCapture } from './recorder-capture.mjs';
import { WorkerError, requireValue, text, httpURL, privateURL, validateStep, validateTarget, RecordingStore, executeStep, generateSource } from './recorder-ir.mjs';

const now = () => new Date().toISOString();
const privateSelectors = 'input,textarea,[contenteditable="true"], [autocomplete*="cc-"], [data-verifact-sensitive]';
export class BrowserWorker {
  constructor({ dataDir, notify = () => {} }) {
    this.dataDir = path.resolve(dataDir);
    this.store = new RecordingStore(this.dataDir);
    this.notify = notify;
    this.binding = `__verifact_${randomUUID().replaceAll('-', '')}`;
    this.recording = null;
    this.captureQueue = Promise.resolve();
    this.pendingCaptures = 0;
    this.consoleEvents = [];
    this.networkEvents = [];
    this.redactSelectors = [];
    this.captureVersion = Date.now() * 1000;
  }
  log(level, message) { this.notify('logs', { level, message }); }
  async persist() {
    requireValue(Buffer.byteLength(JSON.stringify(this.recording)) <= 2 * 1024 * 1024, 'recording_limit', 'Recording exceeded the 2 MiB size limit');
    this.recording.updatedAt = now();
    await this.store.save(this.recording);
    this.notify('recording.steps', { recording: this.recording });
  }
  active() { requireValue(this.recording, 'no_recording', 'Start or load a recording first'); return this.recording; }
  attached() { requireValue(this.page && !this.page.isClosed(), 'not_attached', 'Attach to the embedded browser first'); return this.page; }
  enqueueCapture(payload) {
    if (this.pendingCaptures >= 128) {
      if (this.recording?.state === 'recording') {
        this.recording.state = 'paused';
        this.captureQueue = this.captureQueue.then(() => this.persist()).catch(() => this.log('error', 'Could not save paused recording'));
        this.log('warning', 'Recording paused because capture could not keep up');
      }
      return;
    }
    this.pendingCaptures++;
    this.captureQueue = this.captureQueue.then(async () => {
      if (this.recording?.state !== 'recording') return;
      if (this.recording.steps.length >= 5000) { this.recording.state = 'paused'; await this.persist(); this.log('warning', 'Recording paused at the 5000-step limit'); return; }
      if (!payload || typeof payload !== 'object') return;
      if (payload.unsupported) { this.log('warning', 'An unsupported action was not recorded; review the journey before replay.'); return; }
      let step;
      try {
        if (payload.secret === true && payload.type === 'fill') {
          const key = createHash('sha256').update(JSON.stringify(validateTarget(payload.target))).digest('hex').slice(0, 12).toUpperCase();
          step = validateStep({ ...payload, value: undefined, secretRef: `VERIFACT_SECRET_${key}` });
        } else step = validateStep(payload);
        // Page events cannot create assertions, notes, executable expressions or navigations.
        requireValue(['click', 'fill', 'select', 'check', 'key'].includes(step.type), 'invalid_event', 'Unsupported browser event');
      } catch { this.log('warning', 'An invalid capture event was ignored'); return; }
      const previous = this.recording.steps.at(-1);
      if (step.type === 'fill' && previous?.type === 'fill' && JSON.stringify(previous.target) === JSON.stringify(step.target)) {
        step.id = previous.id;
        this.recording.steps[this.recording.steps.length - 1] = step;
      } else this.recording.steps.push(step);
      traceEvent(this, 'recording.action', { stepId: step.id, action: step.type });
      this.recording.revision++;
      this.recording.verification = { status: 'unverified' };
      await this.persist();
    }).catch(() => { this.log('error', 'Recording persistence failed; recording paused'); if (this.recording) this.recording.state = 'paused'; }).finally(() => { this.pendingCaptures--; });
  }
  async attach(endpoint) {
    requireValue(!this.browser, 'already_attached', 'Worker is already attached');
    const url = new URL(httpURL(endpoint));
    requireValue(['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname), 'invalid_endpoint', 'CDP attachment must use loopback');
    this.browser = await chromium.connectOverCDP(url.href, { timeout: 10000 });
    if (this.closing) { await this.browser.close(); this.browser = null; throw new WorkerError('cancelled', 'Worker is shutting down'); }
    const pages = this.browser.contexts().flatMap(context => context.pages());
    if (pages.length !== 1) {
      await this.browser.close(); this.browser = null;
      throw new WorkerError('ambiguous_page', 'Expected exactly one existing hosted page');
    }
    this.page = pages[0];
    this.page.setDefaultTimeout(5000);
    await this.page.exposeBinding(this.binding, (source, payload) => {
      // Subframes are intentionally outside this first slice.
      if (source.frame === this.page.mainFrame()) this.enqueueCapture(payload);
    });
    await this.page.addInitScript(installCapture, { binding: this.binding, version: this.captureVersion });
    await this.page.evaluate(installCapture, { binding: this.binding, version: this.captureVersion });
    this.page.on('console', message => {
      // Console bodies and network bodies may contain credentials. Keep metadata only.
      this.consoleEvents.push({ type: message.type(), text: '[content omitted for privacy]', timestamp: now() });
      traceEvent(this, 'console', { level: message.type(), contentOmitted: true });
      if (this.consoleEvents.length > 200) this.consoleEvents.shift();
    });
    this.page.on('response', response => {
      this.networkEvents.push({ url: privateURL(response.url()), status: response.status(), method: response.request().method(), timestamp: now() });
      traceEvent(this, 'network.response', { url: privateURL(response.url()), status: response.status(), method: response.request().method() });
      if (this.networkEvents.length > 200) this.networkEvents.shift();
    });
    this.page.on('framenavigated', frame => {
      if (frame !== this.page.mainFrame()) return;
      traceEvent(this, 'navigation', { url: privateURL(frame.url()) });
      this.captureQueue = this.captureQueue.then(async () => {
        if (this.recording?.state !== 'recording') return;
        if (this.recording.steps.length >= 5000) { this.recording.state = 'paused'; await this.persist(); return; }
        try {
          const step = validateStep({ type: 'navigate', url: frame.url(), observed: true });
          requireValue(this.recording.steps.length < 5000, 'recording_limit', 'Recording reached the 5000-step limit');
        this.recording.steps.push(step); this.recording.revision++;
          this.recording.verification = { status: 'unverified' };
          await this.persist();
        } catch { this.log('warning', 'Navigation URL was not recorded because it contains private or unsupported components'); }
      }).catch(() => this.log('error', 'Navigation persistence failed'));
    });
    return this.status();
  }
  status() { return { attached: Boolean(this.page && !this.page.isClosed()), url: this.page && !this.page.isClosed() ? privateURL(this.page.url()) : null, recording: this.recording ? { id: this.recording.id, state: this.recording.state, stepCount: this.recording.steps.length } : null }; }
  async screenshot(page = this.attached(), prefix = 'browser', selectors = this.redactSelectors) {
    const directory = path.join(this.dataDir, 'browser-evidence');
    await mkdir(directory, { recursive: true, mode: 0o700 }); await chmod(directory, 0o700);
    const filename = `${prefix}-${randomUUID()}.png`;
    const masks = [page.locator(privateSelectors), ...selectors.map(selector => page.locator(selector))];
    for (const [key, value] of Object.entries(process.env)) {
      if (key.startsWith('VERIFACT_SECRET_') && value) masks.push(page.getByText(value, { exact: false }));
    }
    await page.screenshot({ path: path.join(directory, filename), mask: masks, animations: 'disabled', timeout: 10000 });
    await chmod(path.join(directory, filename), 0o600);
    return { path: path.join(directory, filename), redacted: true };
  }
  async recordingFor(params) {
    if (!params.id || params.id === this.recording?.id) return this.active();
    return this.store.get(params.id);
  }
  cancelReplay() {
    const operation = this.operation;
    if (!operation || operation.cancelled) return { cancelled: false, status: operation ? 'cancelling' : 'idle' };
    operation.cancelled = true;
    this.notify('recording.replay', { id: operation.id, kind: operation.kind, status: 'cancelling' });
    if (operation.kind === 'generated_test') {
      if (operation.child) {
        try {
          if (process.platform !== 'win32') process.kill(-operation.child.pid, 'SIGINT');
          else operation.child.kill('SIGINT');
        } catch {}
        operation.killTimer = setTimeout(() => {
          try {
            if (process.platform !== 'win32') process.kill(-operation.child.pid, 'SIGKILL');
            else operation.child.kill('SIGKILL');
          } catch {}
        }, 3000);
        operation.killTimer.unref();
      }
    } else {
      operation.cancelTask = (async () => {
        if (operation.page && !this.closing) {
          try { operation.result.evidence = await this.screenshot(operation.page, 'replay-cancelled', operation.selectors); }
          catch { operation.result.evidence = { unavailable: true }; }
        }
        if (this.replayBrowser) await this.replayBrowser.close();
      })().catch(() => {});
    }
    return { cancelled: true, status: 'cancelling', runId: operation.id };
  }
  async replay(recording, timeout) {
    requireValue(recording.state === 'completed', 'invalid_state', 'Stop recording before replay');
    requireValue(!this.operation, 'busy', 'A browser replay or generated test is already running');
    const revision = recording.revision;
    const steps = recording.steps.map(validateStep);
    const result = { id: randomUUID(), recordingId: recording.id, revision, status: 'running', startedAt: now(), steps: [], assertions: 0, isolated: true };
    const operation = { id: result.id, kind: 'replay', result, selectors: recording.redactSelectors ?? [], cancelled: false, page: null };
    this.operation = operation;
    this.notify('recording.replay', { id: result.id, kind: 'replay', status: 'running', recordingId: recording.id });
    let browser;
    const deadline = Date.now() + 90000;
    try {
      requireValue(!this.closing, 'cancelled', 'Worker is shutting down');
      browser = await chromium.launch({ headless: true, timeout: 20000 });
      this.replayBrowser = browser;
      result.runtime = { node: process.version, browser: browser.version() };
      requireValue(!this.closing && !operation.cancelled, 'cancelled', 'Replay was cancelled');
      const context = await browser.newContext();
      const page = await context.newPage();
      operation.page = page;
      for (const step of steps) {
        try {
          requireValue(!operation.cancelled, 'cancelled', 'Replay was cancelled');
          requireValue(Date.now() < deadline, 'timed_out', 'Replay exceeded the 90-second limit');
          this.notify('recording.replay', { id: result.id, kind: 'replay', status: 'running', stepId: step.id, completedSteps: result.steps.length });
          await executeStep(page, step, Math.min(timeout, Math.max(100, deadline - Date.now())));
          result.steps.push({ id: step.id, status: 'passed' });
          traceEvent(this, 'replay.step', { stepId: step.id, status: 'passed' });
          if (step.type === 'assertion') result.assertions++;
        } catch (error) {
          const code = operation.cancelled ? 'cancelled' : error.code || (step.type === 'assertion' ? 'assertion_failed' : error.name === 'TimeoutError' ? (step.type === 'navigate' ? 'navigation_timeout' : 'element_not_interactable') : 'infrastructure_error');
          result.steps.push({ id: step.id, status: code === 'cancelled' ? 'cancelled' : 'failed', code });
          result.failure = { stepId: step.id, code, message: this.failureMessage(code) };
          if (!operation.cancelled) {
            try { result.evidence = await this.screenshot(page, 'replay-failure', operation.selectors); }
            catch { result.evidence = { unavailable: true }; }
          }
          throw new WorkerError(code, result.failure.message);
        }
      }
      result.status = result.assertions > 0 ? 'passed' : 'unverified';
    } catch (error) {
      result.status = operation.cancelled || error.code === 'cancelled' ? 'cancelled' : 'failed';
      result.failure ??= { code: error.code || 'infrastructure_error', message: 'Isolated replay could not complete' };
    } finally {
      if (operation.cancelTask) await operation.cancelTask;
      if (browser) await browser.close();
      this.replayBrowser = null;
      this.operation = null;
    }
    result.finishedAt = now();
    recording.verification = { status: result.status === 'passed' ? 'verified' : 'unverified', kind: 'ir_replay', revision, replayId: result.id, assertions: result.assertions };
    recording.lastReplay = result;
    await this.store.save(recording);
    if (this.recording?.id === recording.id) this.notify('recording.steps', { recording });
    this.notify('recording.replay', { ...result, kind: 'replay' });
    return result;
  }
  failureMessage(code) {
    return ({ cancelled: 'Replay was cancelled', ambiguous_selector: 'Recorded target matches multiple elements', selector_not_found: 'Recorded target was not found', assertion_failed: 'Expected browser state was not observed', missing_secret: 'A required secret environment variable is missing', authentication_failure: 'The destination requires authentication', network_failure: 'The destination could not be reached successfully', navigation_timeout: 'Navigation did not complete before its deadline', element_not_interactable: 'Element could not be interacted with before its deadline' })[code] || 'Browser replay could not complete';
  }
  async dispatch(method, params = {}) {
    requireValue(!this.closing, 'cancelled', 'Worker is shutting down');
    requireValue(params && typeof params === 'object' && !Array.isArray(params), 'invalid_params', 'params must be an object');
    await this.captureQueue;
    switch (method) {
      case 'attach': return this.attach(params.endpoint);
      case 'browser.status': return this.status();
      case 'browser.navigate': await this.attached().goto(httpURL(params.url), { waitUntil: 'domcontentloaded', timeout: 15000 }); return this.status();
      case 'browser.back': await this.attached().goBack({ waitUntil: 'commit', timeout: 10000 }); return this.status();
      case 'browser.forward': await this.attached().goForward({ waitUntil: 'commit', timeout: 10000 }); return this.status();
      case 'browser.reload': await this.attached().reload({ waitUntil: 'domcontentloaded', timeout: 10000 }); return this.status();
      case 'browser.viewport': return viewport(this, params);
      case 'browser.snapshot': return structuralSnapshot(this);
      case 'browser.trace': return browserTrace(this, params.action);
      case 'browser.trace.start': return browserTrace(this, 'start');
      case 'browser.trace.stop': return browserTrace(this, 'stop');
      case 'browser.screenshot': return this.screenshot();
      case 'browser.console': return { entries: this.consoleEvents };
      case 'browser.network': return { entries: this.networkEvents };
      case 'browser.storageclear': {
        const page = this.attached();
        await page.context().clearCookies();
        await page.evaluate(() => { localStorage.clear(); sessionStorage.clear(); });
        return { cleared: true, scope: 'context cookies and current-origin local/session storage' };
      }
      case 'recording.start': {
        this.attached();
        requireValue(!this.recording || this.recording.state === 'completed', 'invalid_state', 'Stop the active recording first');
        const selectors = params.redactSelectors ?? [];
        requireValue(Array.isArray(selectors) && selectors.length <= 50, 'invalid_params', 'redactSelectors must be an array of at most 50 CSS selectors');
        this.redactSelectors = selectors.map(selector => text(selector, 500));
        for (const selector of this.redactSelectors) { try { await this.page.locator(selector).count(); } catch { throw new WorkerError('invalid_params', 'Invalid redaction selector'); } }
        const initial = validateStep({ type: 'navigate', url: this.page.url() });
        this.recording = { schemaVersion: 1, id: randomUUID(), name: text(params.name ?? 'Untitled journey', 200), projectId: params.projectId ? text(params.projectId, 200) : null, state: 'recording', revision: 1, createdAt: now(), updatedAt: now(), steps: [initial], redactSelectors: this.redactSelectors, verification: { status: 'unverified' } };
        this.captureVersion++;
        await this.page.addInitScript(installCapture, { binding: this.binding, redactSelectors: this.redactSelectors, version: this.captureVersion });
        await this.page.evaluate(installCapture, { binding: this.binding, redactSelectors: this.redactSelectors, version: this.captureVersion });
        await this.persist(); return this.recording;
      }
      case 'recording.pause': requireValue(this.active().state === 'recording', 'invalid_state', 'Recording is not active'); this.recording.state = 'paused'; await this.persist(); return this.recording;
      case 'recording.resume': requireValue(this.active().state === 'paused', 'invalid_state', 'Recording is not paused'); this.recording.state = 'recording'; await this.persist(); return this.recording;
      case 'recording.stop': requireValue(['recording', 'paused'].includes(this.active().state), 'invalid_state', 'Recording is already stopped'); this.recording.state = 'completed'; await this.persist(); return this.recording;
      case 'recording.status': return { recording: this.recording };
      case 'recording.get': return this.recordingFor(params);
      case 'recording.load': {
        requireValue(!this.recording || !['recording', 'paused'].includes(this.recording.state), 'invalid_state', 'Stop the active recording before loading another');
        requireValue(!this.operation, 'busy', 'Wait for replay to finish before loading another recording');
        this.recording = await this.store.get(params.id);
        this.redactSelectors = this.recording.redactSelectors ?? [];
        this.notify('recording.steps', { recording: this.recording });
        return this.recording;
      }
      case 'recording.list': {
        const recordings = await this.store.list(this.recording?.id);
        const projectId = params.projectId === undefined ? undefined : text(params.projectId, 200);
        return { recordings: projectId === undefined ? recordings : recordings.filter(recording => recording.projectId === projectId) };
      }
      case 'recording.note': case 'recording.checkpoint': case 'recording.assertion': {
        requireValue(['recording', 'paused'].includes(this.active().state), 'invalid_state', 'Recording is stopped');
        const type = method.slice('recording.'.length);
        const step = validateStep({ ...params, type, text: params.text ?? params.label ?? '' });
        if (type === 'assertion') {
          // Validate the developer-supplied expectation against the current page,
          // but only replay in a fresh context can establish verification.
          await executeStep(this.attached(), step, 5000);
        }
        requireValue(this.recording.steps.length < 5000, 'recording_limit', 'Recording reached the 5000-step limit');
        this.recording.steps.push(step); this.recording.revision++;
        this.recording.verification = { status: 'unverified' }; await this.persist(); return this.recording;
      }
      case 'recording.edit': {
        const recording = await this.recordingFor(params);
        requireValue(['completed', 'interrupted'].includes(recording.state), 'invalid_state', 'Stop recording before editing');
        recording.state = 'completed';
        if (params.name !== undefined) recording.name = text(params.name, 200);
        if (params.steps !== undefined) {
          requireValue(Array.isArray(params.steps) && params.steps.length <= 5000, 'invalid_params', 'Expected at most 5000 steps');
          recording.steps = params.steps.map(validateStep);
        }
        recording.revision++; recording.updatedAt = now(); recording.verification = { status: 'unverified' }; delete recording.lastReplay; delete recording.generatedVerification; delete recording.lastGeneratedRun;
        await this.store.save(recording); if (recording.id === this.recording?.id) this.notify('recording.steps', { recording }); return recording;
      }
      case 'recording.discard': {
        const recording = await this.recordingFor(params);
        await this.store.discard(recording.id); if (this.recording?.id === recording.id) this.recording = null;
        return { discarded: true, id: recording.id };
      }
      case 'recording.cancel_replay': return this.cancelReplay();
      case 'recording.verify_generated_test': {
        const timeoutMs = params.timeoutMs ?? 60000;
        requireValue(Number.isInteger(timeoutMs) && timeoutMs >= 100 && timeoutMs <= 90000, 'invalid_params', 'timeoutMs must be 100–90000');
        return verifyGenerated(this, await this.recordingFor(params), timeoutMs);
      }
      case 'recording.replay': {
        const timeout = params.timeoutMs ?? 5000;
        requireValue(Number.isInteger(timeout) && timeout >= 100 && timeout <= 30000, 'invalid_params', 'timeoutMs must be 100–30000');
        return this.replay(await this.recordingFor(params), timeout);
      }
      case 'recording.generate_test': {
        const recording = await this.recordingFor(params);
        requireValue(recording.state === 'completed', 'invalid_state', 'Stop recording before generating');
        const source = generateSource(recording);
        const sourceHash = createHash('sha256').update(source).digest('hex');
        const generatedVerified = recording.generatedVerification?.status === 'verified' && recording.generatedVerification.revision === recording.revision && recording.generatedVerification.sourceHash === sourceHash;
        return { recordingId: recording.id, revision: recording.revision, status: generatedVerified ? 'verified' : 'unverified', sourceHash, generatedVerification: recording.generatedVerification, recordingVerification: recording.verification, source, format: 'playwright-test', verificationScope: 'Exact generated source revision executed independently with explicit assertions' };
      }
      default: throw new WorkerError('method_not_found', 'Unknown worker method');
    }
  }
  async close() {
    this.closing = true;
    this.cancelReplay();
    if (this.trace) { try { await browserTrace(this, 'stop'); } catch {} }
    if (this.replayBrowser) await this.replayBrowser.close();
    await this.captureQueue;
    if (this.recording && ['recording', 'paused'].includes(this.recording.state)) { this.recording.state = 'interrupted'; await this.persist(); }
    // Closing a CDP connection disconnects this client; it does not own CEF.
    if (this.browser) await this.browser.close();
  }
}

async function main() {
  const index = process.argv.indexOf('--data-dir');
  requireValue(index >= 0 && process.argv[index + 1], 'invalid_args', 'Usage: node worker/browser-worker.mjs --data-dir PATH');
  const dataDir = path.resolve(process.argv[index + 1]);
  await mkdir(dataDir, { recursive: true, mode: 0o700 });
  const actual = await realpath(dataDir);
  const checkout = await realpath(fileURLToPath(new URL('../', import.meta.url)));
  requireValue(actual !== checkout && !actual.startsWith(`${checkout}${path.sep}`), 'invalid_data_dir', 'Recording data must be outside the source checkout');
  const send = value => process.stdout.write(`${JSON.stringify(value)}\n`);
  const worker = new BrowserWorker({ dataDir: actual, notify: (method, params) => send({ method, params }) });
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
  let requests = Promise.resolve();
  input.on('line', line => {
    let request;
    try {
      requireValue(Buffer.byteLength(line) <= 1024 * 1024, 'invalid_request', 'Request exceeds 1 MiB');
      try { request = JSON.parse(line); } catch { throw new WorkerError('parse_error', 'Invalid JSON'); }
      requireValue(request && (typeof request.id === 'string' || typeof request.id === 'number') && typeof request.method === 'string', 'invalid_request', 'Request requires id and method');
    } catch (error) {
      requests = requests.then(() => send({ id: request?.id ?? null, error: { code: error.code || 'invalid_request', message: error.message } }));
      return;
    }
    if (request.method === 'recording.cancel_replay') {
      send({ id: request.id, result: worker.cancelReplay() });
      return;
    }
    requests = requests.then(async () => {
      try {
        const result = await worker.dispatch(request.method, request.params ?? {});
        send({ id: request.id, result });
      } catch (error) { send({ id: request.id, error: { code: error.code || 'worker_error', message: error instanceof WorkerError ? error.message : 'Browser worker operation failed' } }); }
    });
  });
  let stopping = false;
  async function shutdown(cancelActive) {
    if (stopping) return;
    stopping = true;
    if (cancelActive) await worker.close();
    await requests;
    await worker.close();
    process.exitCode = 0;
  }
  input.on('close', () => shutdown(false).catch(() => { process.exitCode = 1; }));
  const signal = () => { shutdown(true).catch(() => { process.exitCode = 1; }); input.close(); };
  process.on('SIGTERM', signal);
  process.on('SIGINT', signal);
  process.on('SIGUSR1', () => worker.cancelReplay());
}
async function isMainModule() {
  if (!process.argv[1]) return false;
  try { return await realpath(process.argv[1]) === await realpath(fileURLToPath(import.meta.url)); }
  catch { return false; }
}
if (await isMainModule()) main().catch(() => { process.stderr.write('Browser worker startup failed\n'); process.exitCode = 1; });
