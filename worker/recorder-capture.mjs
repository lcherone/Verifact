// Serialized into the attached page by Playwright. This function is application
// instrumentation, never supplied by a recording or protocol caller.
export function installCapture({ binding, redactSelectors = [], version = 0 }) {
  if (version >= (window.__verifactCaptureVersion ?? -1)) {
    window.__verifactRedactSelectors = redactSelectors;
    window.__verifactCaptureBinding = binding;
    window.__verifactCaptureVersion = version;
  }
  if (window.__verifactCaptureInstalled) return;
  window.__verifactCaptureInstalled = true;
  const normalize = value => String(value ?? '').replace(/\s+/g, ' ').trim().slice(0, 500);
  const sensitive = element => {
    const metadata = [element.type, element.name, element.id, element.autocomplete, element.getAttribute('aria-label')].join(' ');
    return /password|passwd|secret|token|auth|credit|card|cc-|cvc|cvv|one-time-code|session/i.test(metadata)
      || window.__verifactRedactSelectors.some(selector => { try { return element.matches(selector); } catch { return true; } });
  };
  const roleOf = element => element.getAttribute('role') || ({ BUTTON: 'button', A: element.hasAttribute('href') ? 'link' : null, SELECT: element.multiple ? 'listbox' : 'combobox', TEXTAREA: 'textbox', H1: 'heading', H2: 'heading', H3: 'heading' }[element.tagName]) || (element.tagName === 'INPUT' ? ({ checkbox: 'checkbox', radio: 'radio', button: 'button', submit: 'button', password: null }[element.type] ?? (element.type === 'password' ? null : 'textbox')) : null);
  const nameOf = element => {
    if (element.getAttribute('aria-label')) return normalize(element.getAttribute('aria-label'));
    const labelled = element.getAttribute('aria-labelledby');
    if (labelled) return normalize(labelled.split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' '));
    if (element.labels?.length) return normalize([...element.labels].map(label => label.textContent).join(' '));
    return normalize(element.textContent || element.getAttribute('title') || '');
  };
  function targetFor(element) {
    const candidates = [];
    const role = roleOf(element);
    const name = nameOf(element);
    if (role && name) {
      const same = [...document.querySelectorAll('button,a,input,textarea,select,h1,h2,h3,[role]')].filter(item => roleOf(item) === role && nameOf(item) === name);
      if (same.length === 1) candidates.push({ kind: 'role', role, name });
    }
    const testId = element.getAttribute('data-testid');
    if (testId && document.querySelectorAll(`[data-testid="${CSS.escape(testId)}"]`).length === 1) candidates.push({ kind: 'testId', value: testId });
    if (element.labels?.length && name && [...document.querySelectorAll('input,textarea,select')].filter(item => nameOf(item) === name).length === 1) candidates.push({ kind: 'label', value: name });
    if (element.id && document.querySelectorAll(`#${CSS.escape(element.id)}`).length === 1) candidates.push({ kind: 'css', value: `#${CSS.escape(element.id)}` });
    return candidates.length ? { candidates } : null;
  }
  function send(payload) {
    const emit = window[window.__verifactCaptureBinding];
    if (typeof emit === 'function') emit(payload).catch(() => {});
  }
  const editable = element => element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement;
  document.addEventListener('click', event => {
    const element = event.target.closest?.('button,a,input,textarea,select,[role="button"],[role="checkbox"]');
    if (!element || editable(element) || element.tagName === 'SELECT' || ['checkbox', 'radio'].includes(element.type)) return;
    const target = targetFor(element);
    if (target) send({ type: 'click', target });
    else send({ unsupported: 'No unique semantic or stable locator for clicked element' });
  }, true);
  document.addEventListener('input', event => {
    const element = event.target;
    if (!editable(element) || ['checkbox', 'radio', 'file', 'button', 'submit'].includes(element.type)) return;
    const target = targetFor(element);
    if (!target) return send({ unsupported: 'No stable locator for input' });
    if (sensitive(element) || /(?:\d[ -]?){13,19}/.test(element.value) || /(?:Bearer\s+|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{10,}\.)/.test(element.value)) send({ type: 'fill', target, secret: true });
    else send({ type: 'fill', target, value: element.value });
  }, true);
  document.addEventListener('change', event => {
    const element = event.target;
    const target = targetFor(element);
    if (!target) return;
    if (element.tagName === 'SELECT') {
      if (sensitive(element)) return send({ unsupported: 'Sensitive select requires manual secret handling' });
      send({ type: 'select', target, values: [...element.selectedOptions].map(option => option.value) });
    } else if (['checkbox', 'radio'].includes(element.type)) send({ type: 'check', target, checked: element.checked });
    else if (element.type === 'file') send({ unsupported: 'File upload capture is not supported in this slice' });
  }, true);
  document.addEventListener('keydown', event => {
    if (!['Enter', 'Tab', 'Escape', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown'].includes(event.key)) return;
    const target = targetFor(event.target);
    if (target) send({ type: 'key', target, key: event.key });
  }, true);
}
