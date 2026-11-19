<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { t, tOr } from '../i18n.svelte';
  import { formatBytes, formatCount } from '../format';
  import { inTauri } from '../tauri';
  import { extensionsList, extensionManager, windowsUpdatesList, windowsUpdatesOpen, maintenanceCancel,
    type Extension, type ExtensionScan, type InstalledUpdate } from '../tools';

  let { area }: { area: 'extensions' | 'updates' } = $props();
  let extensions = $state<ExtensionScan | null>(null);
  let updates = $state<InstalledUpdate[] | null>(null);
  let manager = $state<{ extension: Extension; uri: string } | null>(null);
  let working = $state(false);
  let stopping = $state(false);
  let acknowledged = $state(false);
  let awaitingExternal = $state(false);
  let error = $state('');
  let message = $state('');
  let request = $state<string | null>(null);
  let alive = true;
  let generation = 0;
  const current = (version: number) => alive && version === generation;
  const errorText = (e: unknown) => tOr(`err.${e}`, String(e));
  const managerUris = new Map([
    ['chrome', 'chrome://extensions/'], ['edge', 'edge://extensions/'],
    ['brave', 'brave://extensions/'], ['firefox', 'about:addons'],
  ]);

  $effect(() => {
    const scope = area;
    untrack(() => {
      generation++;
      if (request) void maintenanceCancel(request).catch(() => {});
      request = null; working = false; stopping = false; acknowledged = false; awaitingExternal = false;
      extensions = null; updates = null; manager = null; error = ''; message = '';
      if (inTauri()) void refresh(scope);
    });
  });
  onDestroy(() => { alive = false; generation++; if (request) void maintenanceCancel(request).catch(() => {}); });

  async function refresh(scope = area) {
    if (!alive || working || scope !== area) return;
    const version = ++generation;
    working = true; stopping = false; error = ''; message = ''; manager = null;
    extensions = null; updates = null; awaitingExternal = false;
    const id = scope === 'updates' ? crypto.randomUUID() : null;
    request = id;
    try {
      if (scope === 'extensions') {
        const result = await extensionsList();
        if (current(version)) extensions = result;
      } else {
        const result = await windowsUpdatesList(id!);
        if (current(version) && !stopping) updates = result;
        else if (current(version)) message = t('review.cancelled');
      }
    } catch (e) { if (current(version)) error = String(e) === 'cancelled' ? t('review.cancelled') : errorText(e); }
    finally { if (current(version)) { working = false; stopping = false; request = null; } }
  }
  async function stop() {
    if (!request || stopping) return;
    const id = request; const version = generation;
    stopping = true;
    try { await maintenanceCancel(id); }
    catch (e) { if (current(version) && request === id) { error = errorText(e); stopping = false; } }
  }
  async function showManager(extension: Extension) {
    if (working || area !== 'extensions' || !managerUris.has(extension.browser)) return;
    const version = generation;
    working = true; error = ''; message = ''; manager = null;
    try {
      const uri = await extensionManager(extension.browser);
      if (!current(version)) return;
      // Only present the agreed internal URI, never launch an arbitrary returned URL.
      const expected = managerUris.get(extension.browser)!;
      if (uri !== expected && !(expected.endsWith('/') && uri === expected.slice(0, -1))) throw 'invalid_manager_uri';
      manager = { extension, uri }; awaitingExternal = true;
    } catch (e) { if (current(version)) error = errorText(e); }
    finally { if (current(version)) working = false; }
  }
  async function copyManager() {
    if (!manager || working) return;
    const version = generation; const uri = manager.uri;
    working = true; error = ''; message = '';
    try {
      await navigator.clipboard.writeText(uri);
      if (current(version)) message = t('extensions.copied');
    } catch { if (current(version)) error = t('extensions.copyFailed'); }
    finally { if (current(version)) working = false; }
  }
  async function openUpdates() {
    if (working || !acknowledged || area !== 'updates') return;
    const version = generation;
    working = true; error = ''; message = '';
    try {
      await windowsUpdatesOpen();
      if (!current(version)) return;
      // Opening a page is not evidence of removal. Discard the stale inventory.
      updates = null; acknowledged = false; awaitingExternal = true;
      message = t('updates.opened');
    } catch (e) { if (current(version)) error = errorText(e); }
    finally { if (current(version)) working = false; }
  }
  function returned() {
    if (area === 'updates' && awaitingExternal && !working) void refresh();
  }
</script>

<svelte:window onfocus={returned} />
<section class="review">
  <p class="note">{t(area === 'extensions' ? 'extensions.scope' : 'updates.scope')}</p>
  {#if !inTauri()}<p>{t('tool.needApp')}</p>{:else}
    {#if area === 'updates'}
      <p class="warning">{t('updates.risk')}</p>
      <label class="ack"><input type="checkbox" bind:checked={acknowledged} disabled={working} />{t('updates.ack')}</label>
    {/if}
    <div class="controls">
      <button class="btn" disabled={working} onclick={() => refresh()}>{t('review.refresh')}</button>
      {#if area === 'updates'}<button class="btn" disabled={working || !acknowledged} onclick={openUpdates}>{t('updates.open')}</button>{/if}
      {#if request}<button class="btn" disabled={stopping} onclick={stop}>{t(stopping ? 'scan.stopping' : 'scan.stop')}</button>{/if}
    </div>
    {#if working}<p role="status">{t('uninstall.working')}</p>{/if}
    {#if error}<p class="warning" role="alert">{error}</p>{/if}
    {#if message}<p class="message" role="status">{message}</p>{/if}
    {#if awaitingExternal}<p class="note">{t('review.return')}</p>{/if}
    {#if manager && area === 'extensions'}
      <div class="manager">
        <h2>{t('extensions.managerFor', { name: manager.extension.name })}</h2>
        <p class="path">{t('extensions.profile', { browser: tOr(`browser.name.${manager.extension.browser}`, manager.extension.browser), profile: manager.extension.profile })}</p>
        <label>{t('extensions.uri')}<input class="uri" readonly value={manager.uri} onfocus={(event) => event.currentTarget.select()} /></label>
        <button class="btn" disabled={working} onclick={copyManager}>{t('extensions.copy')}</button>
        <p>{t(manager.extension.browser === 'firefox' ? 'extensions.firefoxProfile' : 'extensions.chromiumProfile', { profile: manager.extension.profile, uri: `${manager.extension.browser}://version` })}</p>
        <p>{t('extensions.finish')}</p>
        {#if manager.extension.managed}<p class="warning">{t('extensions.managed')}</p>{/if}
      </div>
    {/if}
    {#if area === 'extensions' && extensions}
      <p class="note" role="status">{t('extensions.stats', { n: formatCount(extensions.extensions.length), skipped: formatCount(extensions.skipped) })}</p>
      {#if extensions.skipped > 0 || extensions.truncated}<p class="warning">{t('extensions.partial')} {#if extensions.truncated}{t('maintenance.truncated')}{/if}</p>{/if}
      <ul>
        {#each extensions.extensions as item (`${item.browser}|${item.profile}|${item.id}`)}
          <li>
            <h2>{item.name} <span class="note">{item.version}</span></h2>
            <p class="path">{t('extensions.profile', { browser: tOr(`browser.name.${item.browser}`, item.browser), profile: item.profile })}</p>
            <p class="path">{t('extensions.id', { id: item.id })}</p>
            <p>{t(item.enabled === null ? 'extensions.stateUnknown' : item.enabled ? 'extensions.enabled' : 'extensions.disabled')} · {item.bytes > 0 ? t('extensions.disk', { size: formatBytes(item.bytes) }) : t('extensions.diskUnknown')}</p>
            {#if item.managed}<p class="warning">{t('extensions.managed')}</p>{/if}
            <details><summary>{t('extensions.permissions')}</summary><p class="path">{item.permissions.length ? item.permissions.join(', ') : t('extensions.permissionsUnknown')}</p></details>
            <button class="btn" disabled={working || !managerUris.has(item.browser)} onclick={() => showManager(item)}>{t('extensions.guide')}</button>
          </li>
        {:else}<li>{t('extensions.none')}</li>{/each}
      </ul>
    {:else if area === 'updates' && updates}
      <p class="note" role="status">{t('updates.count', { n: formatCount(updates.length) })}</p>
      <ul>
        {#each updates as item (item.id)}
          <li>
            <h2>{item.title}</h2>
            <p class="path">{item.kb.length ? item.kb.join(', ') : t('updates.noKb')}</p>
            <p class="path">{t('updates.id', { id: item.id })}</p>
            <p class="note">{t(item.uninstallable ? 'updates.uninstallable' : 'updates.protected')}</p>
          </li>
        {:else}<li>{t('updates.none')}</li>{/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .review { flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 10px; padding-right: 8px; }
  p { margin: 0; font-size: 12px; line-height: 1.5; } h2 { margin: 0; font-size: 14px; font-weight: 600; }
  .note { color: var(--text-dim); } .controls { display: flex; flex-wrap: wrap; gap: 8px; }
  .warning, .message { padding: 8px 12px; border-radius: var(--radius); background: rgba(70,201,139,.12); }
  .warning { color: var(--warn); background: rgba(240,179,69,.12); }
  ul { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 8px; }
  li, .manager { display: flex; flex-direction: column; gap: 7px; padding: 12px; border-radius: var(--radius); background: var(--surface); }
  .manager { border: 1px solid var(--accent); } .btn { align-self: flex-start; flex: none; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 12px; }
  .ack { flex-direction: row; align-items: flex-start; line-height: 1.5; } input[type='checkbox'] { flex: none; accent-color: var(--accent); }
  .uri { width: 100%; padding: 8px; background: var(--bg-main); color: var(--text); border: 1px solid var(--border); border-radius: var(--radius); }
  .path { overflow-wrap: anywhere; user-select: text; } summary { font-size: 12px; cursor: pointer; } details p { margin-top: 6px; }
</style>
