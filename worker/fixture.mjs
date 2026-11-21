import { createServer } from 'node:http';

const style = `<style>
  * { box-sizing: border-box } body { margin: 0; background: #f4f6f9; color: #182638; font: 16px system-ui; }
  main { max-width: 720px; margin: 40px auto; padding: 32px; background: white; border: 1px solid #dce2ea; border-radius: 12px; }
  h1 { font-size: 28px; margin-top: 0 } p { line-height: 1.6 } label { display: block; margin: 22px 0 8px }
  input, button, select { font: inherit; padding: 10px 14px; border: 1px solid #bac6d5; border-radius: 6px; }
  input { width: 100% } button { cursor: pointer; background: #235fd6; color: white; margin: 16px 12px 0 0 }
  a { color: #235fd6 } output { display: block; padding: 18px 0; font-weight: 600 } .muted { color: #65748a }
  .spacer { height: 700px; background: linear-gradient(white, #edf2fb); border-radius: 8px; }
</style>`;
const home = `<!doctype html><html lang="en"><meta charset="utf-8"><title>Verifact browser fixture</title>${style}
<main><p class="muted">LOCAL BROWSER FIXTURE</p><h1>A real browser inside GPUI</h1>
<p>Use the form by hand, then let Playwright inspect the same page. This fixture makes no external requests.</p>
<form><label for="name">Your name</label><input id="name" name="name" required autocomplete="off" placeholder="Type here using your keyboard">
<button type="submit">Greet</button><output id="greeting" aria-live="polite">Ready for input</output></form>
<button id="counter">Count: 0</button><label for="choice">Browser input</label><select id="choice"><option>Mouse</option><option>Keyboard</option></select>
<p><a href="/second">Open second page</a></p><p><a href="/second" target="_blank">Open popup</a></p>
<p class="muted">Scroll to check native scrolling.</p><div class="spacer"></div><p id="bottom">End of scroll area</p></main>
<script>
const output = document.querySelector('#greeting');
let count = 0;
document.querySelector('form').addEventListener('submit', event => {
  event.preventDefault();
  const name = document.querySelector('#name').value;
  output.textContent = 'Hello, ' + name + '!';
  localStorage.setItem('lastGreeting', output.textContent);
  console.log('fixture:greeted');
  fetch('/api/greeting', { method: 'POST', body: JSON.stringify({ name }) });
});
document.querySelector('#counter').onclick = event => { event.target.textContent = 'Count: ' + ++count; };
</script></html>`;
const second = `<!doctype html><html lang="en"><meta charset="utf-8"><title>Verifact fixture — Second page</title>${style}<main><h1>Second page</h1><p id="saved"></p><a href="/">Return to form</a></main><script>document.querySelector('#saved').textContent = localStorage.getItem('lastGreeting') || 'No saved greeting';</script></html>`;

export function startFixture(port = 4179) {
  const server = createServer((request, response) => {
    if (request.url === '/api/greeting') {
      request.resume();
      response.writeHead(200, { 'Content-Type': 'application/json' });
      response.end('{"ok":true}');
      return;
    }
    if (request.url === '/favicon.ico') { response.writeHead(204); response.end(); return; }
    if (!['/', '/second'].includes(request.url)) { response.writeHead(404); response.end('Not found'); return; }
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    response.end(request.url === '/second' ? second : home);
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', () => resolve(server));
  });
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  await startFixture();
  console.log('Fixture listening on http://127.0.0.1:4179');
}
