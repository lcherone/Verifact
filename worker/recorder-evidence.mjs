import { mkdir, chmod, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { requireValue, privateURL } from './recorder-ir.mjs';

export async function structuralSnapshot(worker) {
  const page = worker.attached();
  const snapshot = await page.evaluate(({ selectors }) => {
    const privacySelectors = ['input,textarea,[contenteditable="true"],[data-verifact-sensitive]', ...selectors];
    const isSensitive = element => privacySelectors.some(selector => element.matches(selector));
    const elements = [];
    const walker = document.createTreeWalker(document.documentElement, NodeFilter.SHOW_ELEMENT);
    let current = walker.currentNode;
    while (current && elements.length < 2000) { elements.push(current); current = walker.nextNode(); }
    const truncated = Boolean(current);
    const indexes = new Map(elements.map((element, index) => [element, index]));
    const roles = new Set('alert alertdialog application article banner button cell checkbox columnheader combobox complementary contentinfo dialog document feed figure form generic grid gridcell group heading img link list listbox listitem log main marquee math menu menubar menuitem menuitemcheckbox menuitemradio navigation none note presentation progressbar radio radiogroup region row rowgroup rowheader scrollbar search searchbox separator slider spinbutton status switch tab tablist tabpanel textbox timer toolbar tooltip tree treegrid treeitem'.split(' '));
    const role = element => (roles.has(element.getAttribute('role')) ? element.getAttribute('role') : null) || ({ A: element.hasAttribute('href') ? 'link' : null, BUTTON: 'button', SELECT: element.multiple ? 'listbox' : 'combobox', TEXTAREA: 'textbox', INPUT: element.type === 'password' ? null : ['checkbox', 'radio'].includes(element.type) ? element.type : 'textbox', H1: 'heading', H2: 'heading', H3: 'heading', NAV: 'navigation', MAIN: 'main', FORM: 'form' })[element.tagName] || null;
    const identifier = value => typeof value === 'string' && /^[a-zA-Z][a-zA-Z0-9_-]{0,79}$/.test(value) && !/secret|token|password|auth|session|card/i.test(value) ? value : undefined;
    const nodes = [];
    for (const element of elements) {
      if (['SCRIPT', 'STYLE', 'NOSCRIPT'].includes(element.tagName)) continue;
      let ancestor = element.parentElement;
      let privateAncestor = false;
      while (ancestor) { if (isSensitive(ancestor)) { privateAncestor = true; break; } ancestor = ancestor.parentElement; }
      if (privateAncestor) continue;
      const isPrivate = isSensitive(element);
      nodes.push({ index: indexes.get(element), parent: indexes.get(element.parentElement) ?? null, tag: element.tagName.toLowerCase(), role: role(element), id: isPrivate ? undefined : identifier(element.id), testId: isPrivate ? undefined : identifier(element.getAttribute('data-testid')), hidden: element.hidden || element.getAttribute('aria-hidden') === 'true', disabled: element.hasAttribute('disabled'), redacted: isPrivate, textOmitted: true });
    }
    return { nodes, truncated };
  }, { selectors: worker.redactSelectors });
  return { format: 'verifact-structural-snapshot', url: privateURL(page.url()), redacted: true, capturedAt: new Date().toISOString(), ...snapshot, limitations: ['Text, accessible names, input values, raw attributes and iframe contents are omitted for privacy', 'Roles are derived from native HTML semantics and explicit ARIA roles; this is not a full platform accessibility tree'] };
}
export function traceEvent(worker, type, metadata = {}) {
  if (!worker.trace) return;
  if (worker.trace.events.length >= 2000) { worker.trace.truncated = true; return; }
  worker.trace.events.push({ timestamp: new Date().toISOString(), type, ...metadata });
}
export async function browserTrace(worker, action) {
  worker.attached();
  if (action === 'start') {
    requireValue(!worker.trace, 'invalid_state', 'A browser metadata trace is already active');
    worker.trace = { id: randomUUID(), format: 'verifact-browser-metadata-trace', redacted: true, startedAt: new Date().toISOString(), events: [], truncated: false };
    traceEvent(worker, 'trace.started', { url: privateURL(worker.page.url()) });
    return { id: worker.trace.id, status: 'recording', format: worker.trace.format, redacted: true };
  }
  requireValue(action === 'stop', 'invalid_params', 'Trace action must be start or stop');
  requireValue(worker.trace, 'invalid_state', 'No browser metadata trace is active');
  const trace = worker.trace;
  trace.finishedAt = new Date().toISOString();
  const directory = path.join(worker.dataDir, 'browser-evidence');
  await mkdir(directory, { recursive: true, mode: 0o700 }); await chmod(directory, 0o700);
  const filename = path.join(directory, `metadata-trace-${trace.id}.json`);
  await writeFile(filename, JSON.stringify(trace, null, 2), { mode: 0o600 });
  worker.trace = null;
  return { id: trace.id, status: 'completed', path: filename, format: trace.format, eventCount: trace.events.length, truncated: trace.truncated, redacted: true };
}
export async function viewport(worker, params) {
  const page = worker.attached();
  if (!worker.viewportSession) worker.viewportSession = await page.context().newCDPSession(page);
  if (params.reset === true) {
    await worker.viewportSession.send('Emulation.clearDeviceMetricsOverride');
    return { reset: true };
  }
  requireValue(Number.isInteger(params.width) && params.width >= 320 && params.width <= 4096 && Number.isInteger(params.height) && params.height >= 240 && params.height <= 4096, 'invalid_params', 'Viewport must be 320–4096 wide and 240–4096 high');
  await worker.viewportSession.send('Emulation.setDeviceMetricsOverride', { width: params.width, height: params.height, deviceScaleFactor: 1, mobile: false });
  return { width: params.width, height: params.height, ...(await page.evaluate(() => ({ actualWidth: innerWidth, actualHeight: innerHeight }))) };
}
