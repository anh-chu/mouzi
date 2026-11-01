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
  // Close the initial settings window, then exercise the actual popup command
  // that must create a fresh webview. Creating it synchronously deadlocks on Windows.
  await page.evaluate(() => window.__TAURI_INTERNALS__.invoke('show_popup_cmd'));
  let popup;
  await until(async () => { popup = browser.contexts().flatMap(c => c.pages()).find(p => p.url().includes('popup')); return !!popup; }, 30000);
  await popup.getByRole('button', {name:'Organize Now', exact:true}).waitFor({timeout:30000});
  await popup.evaluate(() => window.__TAURI_INTERNALS__.invoke('close_settings'));
  await until(async () => page.isClosed(), 30000);
  await popup.getByRole('button', {name:'Organize Now', exact:true}).click();
  await until(async () => { page = browser.contexts().flatMap(c => c.pages()).find(p => p.url().includes('settings') && !p.isClosed()); return !!page; }, 30000);
  page.on('pageerror', e => errors.push(e.stack || e.message));
  await page.getByRole('heading', {name:'Review queue', exact:true}).waitFor({timeout:30000});
  await page.getByRole('button', {name:'Refresh preview', exact:true}).waitFor({timeout:30000});
  if (errors.length) throw Error(errors.join('\n'));
  console.log('Organize Now created and opened the review window');

} finally {
  await browser?.close();
  child?.kill();
  try { execFileSync('reg.exe', ['delete', policy, '/v', basename(exe), '/f']); } catch {}
  await rm(root, { recursive: true, force: true });
}
