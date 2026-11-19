<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { formatBytes, formatCount } from '../format';
  import { t, tOr } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import { tuningList, tuningApply, browserCacheScan, browserCacheClear, browserCacheReveal, appCacheScan, appCacheClear, appCacheReveal, browserFlushDns, browserNetwork, maintenanceCancel,
    type Tweak, type TweakReport, type BrowserCacheScan, type BrowserCacheReport } from '../tools';

  let { area }: { area: 'deep' | 'browser' | 'apps' } = $props();
  let tweaks = $state<Tweak[]>([]);
  let selected = $state<Record<string, boolean>>({});
  let cachePicked = $state<Record<string, boolean>>({});
  let scan = $state<BrowserCacheScan | null>(null);
  let report = $state<TweakReport | null>(null);
  let cacheReport = $state<BrowserCacheReport | null>(null);
  let working = $state<string | null>(null);
  let error = $state('');
  let message = $state('');
  let stopping = $state(false);
  let progress = $state({ path: '', count: 0 });
  let dnsAcknowledged = $state(false);
  let alive = true;
  let generation = 0;
  let request = $state<string | null>(null);
  const visible = $derived(tweaks.filter((item) => item.area === area));
  const chosen = $derived(visible.filter((item) => selected[item.id] && item.available && !item.active));
  const caches = $derived((scan?.caches ?? []).filter((item) => ['vscode', 'discord', 'slack'].includes(item.browser) === (area === 'apps')));
  const chosenCaches = $derived(caches.filter((item) => cachePicked[item.id]));
  const errorText = (e: unknown) => String(e) === 'cancelled' ? t('result.cancelled') : tOr(`err.${e}`, String(e));
  const current = (version: number) => alive && version === generation;

  // Scope changes invalidate old results and selections. Cancel only our own UUID.
  $effect(() => {
    const scope = area;
    untrack(() => {
      generation++;
      if (request) void maintenanceCancel(request).catch(() => {});
      request = null; working = null; stopping = false;
      tweaks = []; selected = {}; cachePicked = {}; scan = null; report = null; cacheReport = null;
      error = ''; message = ''; dnsAcknowledged = false;
      if (inTauri() && scope !== 'apps') void refresh();
    });
  });
  onDestroy(() => { alive = false; generation++; if (request) void maintenanceCancel(request).catch(() => {}); });

  async function refresh() {
    if (!alive || working || area === 'apps') return;
    const version = generation;
    working = 'list'; error = ''; selected = {};
    try { const list = await tuningList(); if (current(version)) tweaks = list; }
    catch (e) { if (current(version)) { error = errorText(e); tweaks = []; } }
    finally { if (current(version)) working = null; }
  }
  async function apply() {
    if (working || !chosen.length || area === 'apps') return;
    const version = generation;
    const ids = chosen.map((item) => item.id);
    working = 'apply'; error = ''; report = null; message = '';
    try {
      const result = await tuningApply(ids);
      if (!current(version)) return;
      report = result; selected = {};
      const list = await tuningList();
      if (current(version)) tweaks = list;
    } catch (e) { if (current(version)) { error = errorText(e); tweaks = []; selected = {}; } }
    finally { if (current(version)) working = null; }
  }
  async function stop() {
    if (!request || stopping) return;
    const id = request; const version = generation;
    stopping = true;
    try { await maintenanceCancel(id); }
    catch (e) { if (current(version) && request === id) { error = errorText(e); stopping = false; } }
  }
  async function findCaches() {
    if (working || area === 'deep') return;
    const version = generation; const scanCaches = area === 'apps' ? appCacheScan : browserCacheScan;
    const id = crypto.randomUUID(); request = id;
    working = 'scan'; stopping = false; error = ''; scan = null; cacheReport = null; cachePicked = {}; message = '';
    progress = { path: '', count: 0 };
    try {
      const result = await scanCaches(id, (path, count) => { if (current(version) && request === id) progress = { path, count }; });
      if (current(version) && request === id) scan = result;
    } catch (e) { if (current(version)) { error = errorText(e); scan = null; cachePicked = {}; } }
    finally { if (current(version) && request === id) { request = null; working = null; stopping = false; } }
  }
  async function clearCaches() {
    if (working || !chosenCaches.length || area === 'deep') return;
    const version = generation; const clear = area === 'apps' ? appCacheClear : browserCacheClear;
    const ids = chosenCaches.map((item) => item.id);
    const id = crypto.randomUUID(); request = id;
    working = 'clear'; stopping = false; error = ''; cacheReport = null; message = '';
    try {
      const result = await clear(ids, id);
      if (!current(version) || request !== id) return;
      cacheReport = result; cachePicked = {};
      if (!result.failed && !result.cancelled && scan) scan = { ...scan, caches: scan.caches.filter((item) => !ids.includes(item.id)) };
      else { scan = null; message = t('browser.rescan'); }
    } catch (e) { if (current(version)) { error = errorText(e); scan = null; cachePicked = {}; } }
    finally { if (current(version) && request === id) { request = null; working = null; stopping = false; } }
  }
  async function showCache(id: string) {
    if (working || area === 'deep') return;
    const version = generation; const reveal = area === 'apps' ? appCacheReveal : browserCacheReveal;
    working = 'reveal'; error = '';
    try { await reveal(id); }
    catch (e) { if (current(version)) { error = errorText(e); scan = null; cachePicked = {}; } }
    finally { if (current(version)) working = null; }
  }
  async function network(flush: boolean) {
    if (working || area !== 'browser' || (flush && !dnsAcknowledged)) return;
    const version = generation;
    working = flush ? 'dns' : 'network'; error = ''; message = '';
    try {
      if (flush) await browserFlushDns(); else await browserNetwork();
      if (current(version)) { message = t(flush ? 'browser.dnsDone' : 'browser.networkOpened'); dnsAcknowledged = false; }
    } catch (e) { if (current(version)) error = errorText(e); }
    finally { if (current(version)) working = null; }
  }
</script>

<section class="panel">
  <h2>{t(area === 'apps' ? 'appCache.title' : area === 'deep' ? 'tuning.deep' : 'tuning.browser')}</h2>
  {#if area !== 'apps'}<p class="note">{t('tuning.safety')}</p>{/if}
  {#if !inTauri()}<p>{t('tool.needApp')}</p>{:else}
    {#if area !== 'apps'}
      <div class="controls">
        <button class="btn" disabled={!!working} onclick={refresh}>{t('uninstall.reload')}</button>
        <button class="btn primary" disabled={!!working || !chosen.length} onclick={apply}>{t('tuning.apply', { n: chosen.length })}</button>
      </div>
    {/if}
    {#if working}<p class="note" role="status">{t('uninstall.working')} {#if working === 'scan'}{t('clean.scanning', { n: formatCount(progress.count) })} <span class="path">{progress.path}</span>{/if}</p>{/if}
    {#if error}<p class="message bad" role="alert">{error}</p>{/if}
    {#if message}<p class="message" role="status">{message}</p>{/if}
    {#if area !== 'apps'}
      {#if report}<p class="message" class:bad={report.failed > 0} role="status">
        {t('tuning.report', { done: report.done, failed: report.failed })}
        {#if report.recordId !== null}{t('maintenance.record', { id: report.recordId })}{/if}
        {#if report.needsRestart}{t(area === 'browser' ? 'tuning.browserRestart' : 'tuning.sessionRestart')}{/if}
      </p>{/if}
      <ul>
        {#each visible as item (item.id)}
          <li>
            <label><input type="checkbox" checked={!!selected[item.id]} disabled={!!working || item.active || !item.available} onchange={() => (selected[item.id] = !selected[item.id])} />
              <span><strong>{tOr(`tuning.${item.id}.name`, item.id)}</strong><small>{tOr(`tuning.${item.id}.desc`, item.id)}</small></span>
            </label>
            <p class="note">{t(item.active ? 'tuning.active' : item.available ? 'tuning.available' : 'tuning.unavailable')}
              {#if item.reason} · {tOr(`tuning.reason.${item.reason}`, item.reason)}{/if}
              {#if item.needsRestart} · {t(area === 'browser' ? 'tuning.browserRestart' : 'tuning.sessionRestart')}{/if}
            </p>
          </li>
        {/each}
      </ul>
      <p class="note">{t('tuning.note')}</p>
    {/if}
    {#if area === 'browser' || area === 'apps'}
      {#if area === 'browser'}<h2>{t('browser.cache')}</h2>{/if}
      <p class="note">{t(area === 'apps' ? 'appCache.note' : 'browser.cacheNote')}</p>
      <div class="controls">
        <button class="btn" disabled={!!working} onclick={findCaches}>{t('browser.scan')}</button>
        <button class="btn primary" disabled={!!working || !chosenCaches.length} onclick={clearCaches}>{t('browser.clear', { n: chosenCaches.length })}</button>
        {#if request}<button class="btn" disabled={stopping} onclick={stop}>{t(stopping ? 'scan.stopping' : 'scan.stop')}</button>{/if}
      </div>
      {#if cacheReport}<p class="message" class:bad={cacheReport.failed > 0} role="status">
        {t('browser.report', { done: formatCount(cacheReport.done), failed: formatCount(cacheReport.failed), size: formatBytes(cacheReport.bytes) })}
        {#if cacheReport.cancelled}{t('result.cancelled')}{/if}
        {#if cacheReport.recordId !== null}{t('maintenance.record', { id: cacheReport.recordId })}{/if}
      </p>
        {#if cacheReport.recovery.length}<div class="message" class:bad={cacheReport.failed > 0} role={cacheReport.failed > 0 ? 'alert' : 'status'}>
          {#each cacheReport.recovery as detail}<p class="path">{t(detail.startsWith('recycled: ') ? 'maintenance.recycledLocation' : 'maintenance.recovery')} {detail}</p>{/each}
        </div>{/if}
      {/if}
      {#if scan}
        <p class="note">{t('browser.stats', { n: caches.length, skipped: scan.skipped })} {#if scan.cancelled}{t('result.cancelled')}{/if}</p>
        <ul>
          {#each caches as cache (cache.id)}
            <li class="cache-row">
              <label><input type="checkbox" checked={!!cachePicked[cache.id]} disabled={!!working} onchange={() => (cachePicked[cache.id] = !cachePicked[cache.id])} aria-label={t('browser.selectCache', { path: cache.path })} />
                <span class="path"><strong>{tOr(`browser.name.${cache.browser}`, cache.browser)} · {formatBytes(cache.bytes)} · {t('browser.files', { n: formatCount(cache.files) })}</strong><small>{cache.path}</small></span>
              </label>
              <button class="btn" disabled={!!working} onclick={() => showCache(cache.id)}>{t('tool.reveal')}</button>
            </li>
          {:else}<li>{t('browser.none')}</li>{/each}
        </ul>
      {/if}
    {/if}
    {#if area === 'browser'}
      <h2>{t('browser.network')}</h2>
      <p class="warning">{t('browser.dnsNote')}</p>
      <label class="ack"><input type="checkbox" bind:checked={dnsAcknowledged} disabled={!!working} />{t('browser.dnsAck')}</label>
      <div class="controls">
        <button class="btn" disabled={!!working || !dnsAcknowledged} onclick={() => network(true)}>{t('browser.flushDns')}</button>
        <button class="btn" disabled={!!working} onclick={() => network(false)}>{t('browser.openNetwork')}</button>
      </div>
      <p class="note">{t('browser.scope')}</p>
    {/if}
  {/if}
</section>

<style>
  .panel { flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 10px; padding-right: 8px; }
  h2 { margin: 0; font-size: 16px; } p { margin: 0; font-size: 12px; line-height: 1.5; } .note, small { color: var(--text-dim); }
  .controls { display: flex; flex-wrap: wrap; gap: 8px; } ul { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 6px; }
  li { padding: 10px 12px; border-radius: 8px; background: var(--surface); } li p { padding-left: 24px; margin-top: 5px; }
  label { display: flex; align-items: flex-start; gap: 8px; font-size: 12px; line-height: 1.5; min-width: 0; } input { flex: none; accent-color: var(--accent); }
  strong { font-size: 13px; font-weight: 600; } small { display: block; margin-top: 3px; font-size: 12px; }
  .message, .warning { padding: 8px 12px; border-radius: 8px; background: rgba(70,201,139,.12); }
  .bad, .warning { color: var(--warn); background: rgba(240,179,69,.12); } .path { overflow-wrap: anywhere; user-select: text; }
  .cache-row { display: flex; align-items: center; gap: 10px; } .cache-row label { flex: 1; } .cache-row .btn { flex: none; }
</style>
