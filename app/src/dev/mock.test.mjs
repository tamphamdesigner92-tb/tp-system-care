import test from 'node:test';
import assert from 'node:assert/strict';
import { registerHooks } from 'node:module';
import { invoke } from '@tauri-apps/api/core';

// Node 24 strips TS types; resolve the app's bundler-style local imports for this test only.
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (context.parentURL?.startsWith(new URL('../', import.meta.url).href) &&
        specifier.startsWith('.') && !/\.[a-z]+$/i.test(specifier)) {
      return nextResolve(`${specifier}.ts`, context);
    }
    return nextResolve(specifier, context);
  },
});
globalThis.window = { location: new URL('http://localhost/?mock'), crypto: globalThis.crypto };
const { install } = await import('./mock.ts');
const tools = await import('../lib/tools.ts');
install();
const params = (query = '') => { window.location = new URL(`http://localhost/?mock&${query}`); };
const request = () => crypto.randomUUID();
const rejects = (promise, code) => assert.rejects(promise, (error) => error === code);

test('phase 4 mock contracts are safe and scoped', async (t) => {
  await t.test('all app constructors include seconds/unknown usage and same-date peers', async () => {
    const apps = await tools.appsList();
    assert.ok(apps.some((a) => a.lastUsed === null));
    assert.ok(apps.some((a) => a.lastUsed !== null && a.lastUsed < Date.now() / 1000 - 90 * 86400));
    for (const app of apps) {
      assert.ok(Object.hasOwn(app, 'lastUsed') && Array.isArray(app.installedTogether));
      for (const peer of app.installedTogether) assert.equal(apps.find((a) => a.id === peer).installDate, app.installDate);
    }
    const forced = await tools.forceTarget('Fixture App', 'C:\\Tools\\Fixture App');
    assert.equal(forced.lastUsed, null); assert.deepEqual(forced.installedTogether, []);
    params('usage=unknown');
    assert.ok((await tools.appsList()).every((a) => a.lastUsed === null));
    params();
  });
  await t.test('review handoffs never claim or simulate completed removal', async () => {
    const before = await tools.extensionsList();
    assert.ok(before.extensions.length > 0);
    assert.ok(before.extensions.every((e) => e.managed === true)); // Managed or unverified, not confirmed policy.
    for (const browser of ['chrome', 'edge', 'brave']) {
      const extensions = before.extensions.filter((e) => e.browser === browser);
      assert.ok(extensions.length > 0);
      assert.ok(extensions.every((e) => e.enabled === null));
    }
    const firefox = before.extensions.filter((e) => e.browser === 'firefox');
    assert.ok(firefox.length > 0);
    assert.ok(firefox.every((e) => e.bytes === 0)); // Unknown size, not an empty extension.
    assert.ok(firefox.some((e) => e.enabled === false));
    assert.equal(await tools.extensionManager('chrome'), 'chrome://extensions/');
    assert.equal(await tools.extensionManager('firefox'), 'about:addons');
    await rejects(tools.extensionManager('https://example.invalid'), 'unsupported_browser');
    assert.deepEqual(await tools.extensionsList(), before);
    const updates = await tools.windowsUpdatesList(request());
    assert.ok(updates.some((u) => !u.uninstallable));
    await tools.windowsUpdatesOpen();
    assert.deepEqual(await tools.windowsUpdatesList(request()), updates);
    assert.deepEqual(await invoke('rescue_history'), []);
    params('review=partial');
    const partial = await tools.extensionsList();
    assert.ok(partial.skipped > 0 && partial.truncated);
    await rejects(tools.windowsUpdatesList(request()), 'review_failed');
    params('review=error');
    await rejects(tools.extensionsList(), 'review_failed');
    params();
  });
  await t.test('busy and cancellation are request-owned, not global', async () => {
    const id = request(); const pending = tools.windowsUpdatesList(id);
    await rejects(tools.extensionsList(), 'busy');
    await rejects(tools.windowsUpdatesOpen(), 'busy');
    await tools.maintenanceCancel(request()); // Another page cannot cancel this request.
    assert.ok((await pending).length > 0);
    const stopId = request(); const stopped = tools.windowsUpdatesList(stopId);
    await tools.maintenanceCancel(stopId);
    await rejects(stopped, 'cancelled');
  });
  await t.test('cancellation before the progress listener resolves never starts a scan', async () => {
    for (const scan of [tools.appCacheScan, tools.browserCacheScan]) {
      const id = request();
      const progress = [];
      const pending = scan(id, (...event) => progress.push(event));
      const rejection = rejects(pending, 'cancelled');
      await tools.maintenanceCancel(id);
      await rejection;
      assert.deepEqual(progress, []);
    }
    assert.ok((await tools.extensionsList()).extensions.length > 0);
  });
  await t.test('app and browser cache selection cannot cross scopes; open apps block cleanup', async () => {
    const progress = [];
    const apps = await tools.appCacheScan(request(), (path, count) => progress.push({ path, count }));
    assert.ok(progress.length > 0);
    assert.ok(apps.caches.every((c) => ['vscode', 'discord', 'slack'].includes(c.browser) && /\\(Cache|Code Cache|GPUCache)$/.test(c.path)));
    const browsers = await tools.browserCacheScan(request(), () => {});
    const appId = apps.caches[0].id; const browserId = browsers.caches[0].id;
    await rejects(tools.appCacheClear([browserId], request()), 'changed_target');
    await rejects(tools.browserCacheClear([appId], request()), 'changed_target');
    await rejects(tools.appCacheReveal(browserId), 'changed_target');
    await rejects(tools.browserCacheReveal(appId), 'changed_target');
    params('apps=running');
    await rejects(tools.appCacheClear([appId], request()), 'browser_running');
    params('browser=running');
    await rejects(tools.browserCacheClear([browserId], request()), 'browser_running');
    params('apps=partial');
    const second = apps.caches[1].id;
    const pending = tools.appCacheClear([appId, second], request());
    await tools.maintenanceCancel(request()); // Unrelated requests cannot cancel cache cleanup either.
    const report = await pending;
    assert.equal(report.cancelled, false);
    assert.equal(report.failed, 1); assert.ok(report.done > 0);
    await rejects(tools.appCacheReveal(second), 'changed_target');
    await tools.appCacheReveal(appId);
    await tools.browserCacheReveal(browserId);
    const record = (await invoke('rescue_history')).find((r) => r.id === report.recordId);
    assert.equal(record.title, `browserCache; ${JSON.stringify(report.recovery)}`);
    params();
    const stopId = request(); const stopped = tools.appCacheClear([appId], stopId);
    await tools.maintenanceCancel(stopId);
    assert.deepEqual(await stopped, { done: 0, failed: 0, bytes: 0, recovery: [], cancelled: true, recordId: null });
    await tools.appCacheReveal(appId);
    assert.deepEqual((await tools.browserCacheScan(request(), () => {})).caches, browsers.caches);
  });
  await t.test('new fixed tweaks use the existing Rescue undo', async () => {
    const ids = ['enableTransparency', 'aeroPeek', 'listviewAlphaSelect', 'listviewShadow'];
    const before = await tools.tuningList();
    assert.ok(ids.every((id) => before.some((item) => item.id === id && !item.active)));
    assert.ok(['chromeNetworkPrediction', 'edgeNetworkPrediction', 'braveNetworkPrediction'].every((id) => before.some((item) => item.id === id)));
    const report = await tools.tuningApply(ids);
    assert.equal(report.done, ids.length); assert.ok(report.recordId !== null);
    await invoke('rescue_undo', { id: report.recordId });
    assert.ok((await tools.tuningList()).filter((item) => ids.includes(item.id)).every((item) => !item.active));
  });
});
