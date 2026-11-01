import { spawn, execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, writeFile, rm } from 'node:fs/promises';
import { resolve, join, basename } from 'node:path';
import { tmpdir } from 'node:os';
import { chromium } from 'playwright';

if (process.env.CI !== 'true' || process.env.GITHUB_ACTIONS !== 'true') throw Error('Run only on an isolated GitHub runner');
const exe = resolve('src-tauri/target/release/mouzi.exe');
const root = await mkdtemp(join(tmpdir(), 'mouzi-review-test-'));
const policy = 'HKLM\\Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments';
let child, browser;
const errors = [];
async function until(fn, timeout = 120000) { const end = Date.now() + timeout; while (Date.now() < end) { if (await fn()) return; await new Promise(r => setTimeout(r, 250)); } throw Error('Timed out waiting for app'); }
try {
  execFileSync('reg.exe', ['add', policy, '/v', basename(exe), '/t', 'REG_SZ', '/d', '--remote-debugging-port=19221 --remote-debugging-address=127.0.0.1', '/f']);
  child = spawn(exe, ['--add-folder', root], { windowsHide: true, stdio: 'ignore' });
  await until(async () => { if (child.exitCode !== null) throw Error(`App exited: ${child.exitCode}`); try { return (await fetch('http://127.0.0.1:19221/json/version')).ok; } catch { return false; } });
  browser = await chromium.connectOverCDP('http://127.0.0.1:19221');
  let page;
  await until(async () => { page = browser.contexts().flatMap(c => c.pages()).find(p => p.url().includes('settings')); return !!page; });
  page.on('pageerror', e => errors.push(e.stack || e.message));
  await page.getByRole('button', { name: 'Watched Folders' }).waitFor({timeout: 30000});
  await page.evaluate(async () => {
    const api = window.__TAURI_INTERNALS__;
    const invoke = api.invoke.bind(api);
    api.invoke = async (cmd, args) => {
      if (cmd === 'take_review_request_cmd') return [];
      if (cmd === 'preview_cmd') return { id: 'mock-plan', entries: [{id:'mock-1', source:'C:\\test\\file.txt', destination:'C:\\test\\Sorted\\file.txt', rule:'Documents', action:'move', size:123, warning:false, error:null}] };
      if (cmd === 'get_settings_cmd') return { ...(await invoke(cmd, args)), ai_enabled:true, ai_auto_move:true };
      if (cmd === 'auto_triage_preview_cmd') return { classifications: [{path:'C:\\test\\file.txt', verdict:'trash', confidence:90, probabilities:{trash:0.9,keep:0.05,unsure:0.05}}], moves: [{id:'mock-1', source:'C:\\test\\file.txt', success:true, error:null}] };
      if (cmd === 'discard_preview_cmd') return null;
      return invoke(cmd, args);
    };
  });
  await page.getByRole('button', { name: 'Review queue' }).click();
  await page.getByRole('heading', { name: 'Review queue' }).waitFor({timeout: 30000});
  await page.getByText('C:\\test\\file.txt', {exact:false}).first().waitFor({timeout: 30000});
  if (errors.length) throw Error(errors.join('\n'));
  console.log('Review window opened and rendered mock automatic Jev result');
} finally {
  await browser?.close();
  child?.kill();
  try { execFileSync('reg.exe', ['delete', policy, '/v', basename(exe), '/f']); } catch {}
  await rm(root, { recursive: true, force: true });
}
