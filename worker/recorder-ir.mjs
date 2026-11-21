import { randomUUID } from 'node:crypto';
import { mkdir, chmod, writeFile, rename, readFile, readdir, unlink, stat } from 'node:fs/promises';
import path from 'node:path';
import { expect } from 'playwright/test';

export class WorkerError extends Error {
  constructor(code, message) { super(message); this.code = code; }
}
export function requireValue(condition, code, message) {
  if (!condition) throw new WorkerError(code, message);
}
export function text(value, max = 1000) {
  requireValue(typeof value === 'string' && value.length <= max, 'invalid_params', 'Expected a bounded string');
  return value;
}
export function httpURL(value) {
  let url;
  try { url = new URL(text(value, 8192)); } catch { throw new WorkerError('invalid_url', 'Expected an HTTP(S) URL'); }
  requireValue(['http:', 'https:'].includes(url.protocol) && !url.username && !url.password, 'invalid_url', 'Only HTTP(S) URLs without embedded credentials are supported');
  return url.href;
}
export function privateURL(value) {
  try {
    const url = new URL(value);
    url.username = ''; url.password = ''; url.hash = '';
    // URL queries can contain credentials or private application data. Never persist them.
    if (url.search) url.search = '?REDACTED';
    return url.href;
  } catch { return '[unavailable]'; }
}
export function validateTarget(target) {
  requireValue(target && Array.isArray(target.candidates) && target.candidates.length > 0 && target.candidates.length <= 8, 'invalid_target', 'A target needs 1–8 locator candidates');
  return { candidates: target.candidates.map(candidate => {
    requireValue(['role', 'testId', 'label', 'text', 'css'].includes(candidate.kind), 'invalid_target', 'Unsupported locator kind');
    if (candidate.kind === 'role') return { kind: 'role', role: text(candidate.role, 50), name: text(candidate.name, 500) };
    return { kind: candidate.kind, value: text(candidate.value, 500) };
  }) };
}
export function locatorFor(page, candidate) {
  switch (candidate.kind) {
    case 'role': return page.getByRole(candidate.role, { name: candidate.name, exact: true });
    case 'testId': return page.getByTestId(candidate.value);
    case 'label': return page.getByLabel(candidate.value, { exact: true });
    case 'text': return page.getByText(candidate.value, { exact: true });
    case 'css': return page.locator(candidate.value);
    default: throw new WorkerError('invalid_target', 'Unsupported locator');
  }
}
export async function resolveTarget(page, target, timeout = 5000) {
  const validated = validateTarget(target);
  // The selected locator is fixed at capture/review time. Falling through to a
  // different element on replay could silently change the meaning of the test.
  const locator = locatorFor(page, validated.candidates[0]);
  try { await locator.first().waitFor({ state: 'attached', timeout }); } catch {
    throw new WorkerError('selector_not_found', 'Recorded target was not found');
  }
  const count = await locator.count();
  requireValue(count === 1, 'ambiguous_selector', `Recorded target matches ${count} elements`);
  return locator;
}
const actions = ['navigate', 'click', 'fill', 'select', 'check', 'key', 'assertion', 'checkpoint', 'note'];
const assertions = ['visible', 'hidden', 'text', 'contains', 'value', 'url', 'count'];
export function validateStep(input) {
  requireValue(input && actions.includes(input.type), 'invalid_step', 'Unsupported step type');
  const step = { id: typeof input.id === 'string' && /^[a-zA-Z0-9-]{1,64}$/.test(input.id) ? input.id : randomUUID(), type: input.type };
  if (input.type === 'navigate') {
    step.url = httpURL(input.url);
    requireValue(!new URL(step.url).search && !new URL(step.url).hash, 'sensitive_url', 'Recording URLs with query strings or fragments requires a sanitized URL');
    step.observed = input.observed === true;
  } else if (['checkpoint', 'note'].includes(input.type)) step.text = text(input.text ?? '', 2000);
  else if (input.type === 'assertion') {
    requireValue(assertions.includes(input.kind), 'invalid_assertion', 'Unsupported assertion kind');
    step.kind = input.kind;
    if (input.kind !== 'url') step.target = validateTarget(input.target);
    if (input.kind === 'count') {
      requireValue(Number.isInteger(input.expected) && input.expected >= 0, 'invalid_assertion', 'Count must be a nonnegative integer');
      step.expected = input.expected;
    } else if (!['visible', 'hidden'].includes(input.kind)) {
      step.expected = input.kind === 'url' ? httpURL(input.expected) : text(input.expected, 2000);
      if (input.kind === 'value') requireValue(!/password|passwd|secret|token|credit|card|cc-|cvc|cvv|session/i.test(JSON.stringify(step.target)), 'sensitive_input', 'Sensitive value assertions are not persisted');
      requireValue(!/(?:\d[ -]?){13,19}|Bearer\s+|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{10,}\./.test(String(step.expected)), 'sensitive_input', 'Assertion expectations must not contain credentials');
      if (input.kind === 'url') requireValue(!new URL(step.expected).search && !new URL(step.expected).hash, 'sensitive_url', 'URL assertions must omit query strings and fragments');
    }
  } else {
    step.target = validateTarget(input.target);
    if (input.type === 'fill') {
      if (input.secretRef) {
        requireValue(/^VERIFACT_SECRET_[A-Z0-9_]{1,80}$/.test(input.secretRef), 'invalid_secret', 'Secret reference must name a VERIFACT_SECRET_ environment variable');
        step.secretRef = input.secretRef;
      } else {
        requireValue(!/password|passwd|secret|token|credit|card|cc-|cvc|cvv|session/i.test(JSON.stringify(step.target)), 'sensitive_input', 'Sensitive fills must use an environment secretRef');
        step.value = text(input.value, 10000);
        requireValue(!/(?:\d[ -]?){13,19}/.test(step.value) && !/(?:Bearer\s+|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{10,}\.)/.test(step.value), 'sensitive_input', 'Sensitive values must use an environment secretRef');
      }
    }
    if (input.type === 'select') {
      requireValue(Array.isArray(input.values) && input.values.length <= 100, 'invalid_step', 'Select requires values');
      step.values = input.values.map(value => text(value, 1000));
    }
    if (input.type === 'check') step.checked = Boolean(input.checked);
    if (input.type === 'key') {
      requireValue(['Enter', 'Tab', 'Escape', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown'].includes(input.key), 'invalid_step', 'Unsupported replay key');
      step.key = input.key;
    }
  }
  return step;
}
export class RecordingStore {
  constructor(dataDir) { this.directory = path.join(dataDir, 'recordings'); }
  file(id) {
    requireValue(typeof id === 'string' && /^[a-f0-9-]{36}$/.test(id), 'invalid_id', 'Invalid recording ID');
    return path.join(this.directory, `${id}.json`);
  }
  async save(recording) {
    await mkdir(this.directory, { recursive: true, mode: 0o700 });
    await chmod(this.directory, 0o700);
    const file = this.file(recording.id);
    const temporary = `${file}.${randomUUID()}.tmp`;
    await writeFile(temporary, JSON.stringify(recording, null, 2), { mode: 0o600 });
    await rename(temporary, file);
  }
  async get(id, { activeId } = {}) {
    try {
      const filename = this.file(id);
      requireValue((await stat(filename)).size <= 2 * 1024 * 1024, 'corrupt_recording', 'Recording exceeds the supported size limit');
      const recording = JSON.parse(await readFile(filename, 'utf8'));
      requireValue(recording && recording.id === id && recording.schemaVersion === 1
        && typeof recording.name === 'string' && recording.name.length <= 200
        && Number.isInteger(recording.revision) && recording.revision >= 1
        && ['recording', 'paused', 'completed', 'interrupted'].includes(recording.state)
        && Array.isArray(recording.steps) && recording.steps.length <= 5000
        && typeof recording.updatedAt === 'string' && typeof recording.createdAt === 'string',
      'corrupt_recording', 'Recording metadata is invalid');
      recording.steps = recording.steps.map(validateStep);
      requireValue(recording.redactSelectors === undefined || (Array.isArray(recording.redactSelectors)
        && recording.redactSelectors.length <= 50 && recording.redactSelectors.every(selector => typeof selector === 'string' && selector.length <= 500)),
      'corrupt_recording', 'Recording redaction metadata is invalid');
      if (recording.id !== activeId && ['recording', 'paused'].includes(recording.state)) {
        recording.state = 'interrupted';
        recording.recovery = { recoverable: true, message: 'Recording ended before completion; review the preserved steps before replay' };
        recording.verification = { status: 'unverified' };
      }
      return recording;
    } catch (error) {
      if (error.code === 'ENOENT') throw new WorkerError('not_found', 'Recording not found');
      if (error.code === 'invalid_id') throw error;
      throw new WorkerError('corrupt_recording', 'Recording is unreadable or invalid; its original file was preserved');
    }
  }
  async list(activeId) {
    await mkdir(this.directory, { recursive: true, mode: 0o700 });
    const recordings = [];
    for (const file of await readdir(this.directory)) {
      if (/^[a-f0-9-]{36}\.json$/.test(file)) {
        const id = file.slice(0, -5);
        try {
          const recording = await this.get(id, { activeId });
          recordings.push({ id: recording.id, projectId: recording.projectId ?? null, name: recording.name, state: recording.state, revision: recording.revision, stepCount: recording.steps.length, verification: recording.verification, updatedAt: recording.updatedAt, recovery: recording.recovery });
        } catch {
          recordings.push({ id, name: 'Unreadable recording', state: 'corrupt', stepCount: 0, verification: { status: 'unverified' }, updatedAt: '', error: 'corrupt_recording' });
        }
      }
    }
    return recordings.sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
  }
  async discard(id) { await unlink(this.file(id)); }
}
export async function executeStep(page, step, timeout) {
  if (step.type === 'navigate') {
    if (step.observed) await page.waitForURL(step.url, { timeout, waitUntil: 'commit' });
    else {
      let response;
      try { response = await page.goto(step.url, { timeout, waitUntil: 'domcontentloaded' }); } catch (error) {
        throw new WorkerError(error.name === 'TimeoutError' ? 'navigation_timeout' : 'network_failure', 'Navigation did not complete');
      }
      if ([401, 403].includes(response?.status())) throw new WorkerError('authentication_failure', 'Navigation requires authentication');
      if ((response?.status() ?? 200) >= 500) throw new WorkerError('network_failure', 'Server rejected navigation');
    }
    return;
  }
  if (['note', 'checkpoint'].includes(step.type)) return;
  if (step.type === 'assertion' && step.kind === 'url') { await expect(page).toHaveURL(step.expected, { timeout }); return; }
  // Count and hidden assertions intentionally allow zero matches; ambiguity for
  // single-element assertions/actions is always an explicit failure.
  if (step.type === 'assertion' && ['count', 'hidden'].includes(step.kind)) {
    const locator = locatorFor(page, validateTarget(step.target).candidates[0]);
    if (step.kind === 'count') await expect(locator).toHaveCount(step.expected, { timeout });
    else { requireValue(await locator.count() <= 1, 'ambiguous_selector', 'Hidden target is ambiguous'); await expect(locator).toBeHidden({ timeout }); }
    return;
  }
  const locator = await resolveTarget(page, step.target, timeout);
  switch (step.type) {
    case 'click': await locator.click({ timeout }); break;
    case 'fill': {
      const value = step.secretRef ? process.env[step.secretRef] : step.value;
      requireValue(value !== undefined, 'missing_secret', `Set environment variable ${step.secretRef}`);
      await locator.fill(value, { timeout }); break;
    }
    case 'select': await locator.selectOption(step.values, { timeout }); break;
    case 'check': await locator.setChecked(step.checked, { timeout }); break;
    case 'key': await locator.press(step.key, { timeout }); break;
    case 'assertion': {
      const options = { timeout };
      if (step.kind === 'visible') await expect(locator).toBeVisible(options);
      if (step.kind === 'text') await expect(locator).toHaveText(step.expected, options);
      if (step.kind === 'contains') await expect(locator).toContainText(step.expected, options);
      if (step.kind === 'value') await expect(locator).toHaveValue(step.expected, options);
      break;
    }
  }
}
const quoted = value => JSON.stringify(value);
function locatorSource(candidate) {
  if (candidate.kind === 'role') return `page.getByRole(${quoted(candidate.role)}, { name: ${quoted(candidate.name)}, exact: true })`;
  if (candidate.kind === 'testId') return `page.getByTestId(${quoted(candidate.value)})`;
  if (candidate.kind === 'label') return `page.getByLabel(${quoted(candidate.value)}, { exact: true })`;
  if (candidate.kind === 'text') return `page.getByText(${quoted(candidate.value)}, { exact: true })`;
  return `page.locator(${quoted(candidate.value)})`;
}
export function generateSource(recording) {
  const lines = ["import { test, expect } from 'playwright/test';", '', `test(${quoted(recording.name)}, async ({ page }) => {`];
  for (const input of recording.steps) {
    const step = validateStep(input);
    if (step.type === 'navigate') lines.push(step.observed ? `  await expect(page).toHaveURL(${quoted(step.url)});` : `  await page.goto(${quoted(step.url)});`);
    else if (['note', 'checkpoint'].includes(step.type)) lines.push(`  // ${step.type}: ${step.text.replace(/[\r\n\u2028\u2029]/g, ' ')}`);
    else if (step.type === 'assertion' && step.kind === 'url') lines.push(`  await expect(page).toHaveURL(${quoted(step.expected)});`);
    else {
      const locator = locatorSource(step.target.candidates[0]);
      if (!(step.type === 'assertion' && ['hidden', 'count'].includes(step.kind))) lines.push(`  await expect(${locator}).toHaveCount(1);`);
      if (step.type === 'click') lines.push(`  await ${locator}.click();`);
      if (step.type === 'fill') {
        if (step.secretRef) lines.push(`  if (process.env[${quoted(step.secretRef)}] === undefined) throw new Error(${quoted(`Missing secret: ${step.secretRef}`)});`);
        lines.push(`  await ${locator}.fill(${step.secretRef ? `process.env[${quoted(step.secretRef)}]` : quoted(step.value)});`);
      }
      if (step.type === 'select') lines.push(`  await ${locator}.selectOption(${quoted(step.values)});`);
      if (step.type === 'check') lines.push(`  await ${locator}.setChecked(${step.checked});`);
      if (step.type === 'key') lines.push(`  await ${locator}.press(${quoted(step.key)});`);
      if (step.type === 'assertion') {
        const methods = { visible: 'toBeVisible', hidden: 'toBeHidden', text: 'toHaveText', contains: 'toContainText', value: 'toHaveValue', count: 'toHaveCount' };
        lines.push(`  await expect(${locator}).${methods[step.kind]}(${step.expected === undefined ? '' : quoted(step.expected)});`);
      }
    }
  }
  const body = lines.splice(3).map(line => `  ${line}`);
  lines.push('  try {', ...body, '  } catch {');
  const selectors = ['input,textarea,[contenteditable="true"],[data-verifact-sensitive]', ...(recording.redactSelectors ?? [])];
  lines.push(`    const masks = ${quoted(selectors)}.map(selector => page.locator(selector));`);
  lines.push('    for (const [key, value] of Object.entries(process.env)) {');
  lines.push("      if (key.startsWith('VERIFACT_SECRET_') && value) masks.push(page.getByText(value, { exact: false }));");
  lines.push('    }');
  lines.push("    try { await page.screenshot({ path: test.info().outputPath('failure.png'), mask: masks }); } catch {}");
  // Avoid writing actual observed secret values into Playwright failure output.
  lines.push("    throw new Error('Recorded workflow failed; raw browser values omitted for privacy');");
  lines.push('  }', '});', '');
  return lines.join('\n');
}
