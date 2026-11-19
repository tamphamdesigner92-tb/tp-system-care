<script lang="ts">
  import { onDestroy } from 'svelte';
  import { formatCount } from '../format';
  import { t, tOr, type Key } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import {
    extensionManage,
    extensionsList,
    extensionsRemove,
    whenIdle,
    type BrowserExtension,
    type ExtensionBrowser,
    type ExtensionRemoval,
  } from '../tools';

  type Step = 'list' | 'confirm' | 'removing' | 'done';
  type Filter = 'all' | ExtensionBrowser;

  let items = $state<BrowserExtension[]>([]);
  let skipped = $state(0);
  let loading = $state(true);
  let loadError = $state('');
  let query = $state('');
  let filter = $state<Filter>('all');
  let step = $state<Step>('list');
  /** Chọn theo `trình duyệt|mã tiện ích`: một mục registry áp dụng cho mọi hồ sơ. */
  let selected = $state<Record<string, boolean>>({});
  let report = $state<ExtensionRemoval | null>(null);
  let error = $state('');
  let notice = $state('');
  let removingCount = $state(0);
  let destroyed = false;
  let version = 0;

  const groupKey = (e: BrowserExtension) => `${e.browser}|${e.extensionId}`;
  const errorText = (e: unknown) => tOr(`err.${String(e)}`, String(e));
  /** Lỗi từng mục có dạng `mã: vị trí registry`. */
  const removalError = (message: string) => {
    const at = message.indexOf(': ');
    return at < 0 ? message : `${errorText(message.slice(0, at))} ${message.slice(at + 2)}`;
  };
  const busy = $derived(loading || step === 'removing');
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return items.filter((e) =>
      (filter === 'all' || e.browser === filter) &&
      (!q || e.name.toLowerCase().includes(q) || e.extensionId.toLowerCase().includes(q)),
    );
  });
  const browsers = $derived([...new Set(items.map((e) => e.browser))]);
  const chosen = $derived(items.filter((e) => e.removal === 'registry' && selected[groupKey(e)]));
  const chosenKeys = $derived([...new Set(chosen.map(groupKey))]);
  const chosenRegistry = $derived([...new Set(chosen.flatMap((e) => e.registry))]);
  const chosenAdmin = $derived(chosen.some((e) => e.needsAdmin));

  async function load() {
    if (!inTauri()) { loading = false; return; }
    const current = ++version;
    loading = true;
    loadError = '';
    try {
      const scan = await whenIdle(extensionsList, () => !destroyed && current === version);
      if (destroyed || current !== version) return;
      items = scan.extensions;
      skipped = scan.skipped;
      const keys = new Set(scan.extensions.filter((e) => e.removal === 'registry').map(groupKey));
      selected = Object.fromEntries(Object.keys(selected).filter((k) => selected[k] && keys.has(k)).map((k) => [k, true]));
    } catch (e) {
      if (!destroyed && current === version) loadError = errorText(e);
    } finally {
      if (!destroyed && current === version) loading = false;
    }
  }

  async function manage(item: BrowserExtension) {
    error = '';
    notice = '';
    try {
      await extensionManage(item.id);
      if (!destroyed) notice = t('ext.opened', { browser: t(`ext.browser.${item.browser}` as Key) });
    } catch (e) {
      if (!destroyed) error = errorText(e);
    }
  }

  async function remove() {
    if (busy || !chosen.length) return;
    step = 'removing';
    removingCount = chosenKeys.length;
    error = '';
    try {
      const result = await extensionsRemove(chosen.map((e) => e.id));
      if (destroyed) return;
      report = result;
      selected = {};
      step = 'done';
    } catch (e) {
      if (destroyed) return;
      error = errorText(e);
      step = 'confirm';
    }
  }

  function back() {
    if (busy) return;
    step = 'list';
    error = '';
    notice = '';
    report = null;
    void load();
  }

  onDestroy(() => { destroyed = true; ++version; });
  void load();
</script>

<div class="extensions">
  {#if !inTauri()}
    <p class="empty">{t('tool.needApp')}</p>
  {:else if step === 'list'}
    <div class="toolbar">
      <input class="search" type="search" aria-label={t('ext.search')} placeholder={t('ext.search')} bind:value={query} disabled={busy} />
      <select aria-label={t('ext.filterLabel')} bind:value={filter} disabled={busy}>
        <option value="all">{t('ext.filter.all')}</option>
        {#each browsers as b (b)}<option value={b}>{t(`ext.browser.${b}` as Key)}</option>{/each}
      </select>
      <button class="btn small" disabled={busy} onclick={load}>{t('ext.reload')}</button>
    </div>
    {#if error}<p class="message bad" role="alert">{error}</p>{/if}
    {#if notice}<p class="message" role="status">{notice}</p>{/if}
    {#if loading}
      <p class="empty">{t('ext.loading')}</p>
    {:else if loadError}
      <p class="message bad" role="alert">{loadError}</p>
    {:else}
      <div class="row selection">
        <button class="btn small primary" disabled={!chosenKeys.length} onclick={() => (step = 'confirm')}>
          {t('ext.removeAction', { n: formatCount(chosenKeys.length) })}
        </button>
        {#if chosenKeys.length}<button class="link" onclick={() => (selected = {})}>{t('uninstall.clearSelection')}</button>{/if}
      </div>
      <ul class="list">
        {#each shown as e (e.id)}
          <li>
            {#if e.removal === 'registry'}
              <input class="check" type="checkbox" aria-label={t('ext.select', { name: e.name })} checked={!!selected[groupKey(e)]} onchange={() => (selected[groupKey(e)] = !selected[groupKey(e)])} />
            {:else}
              <span class="check-space"></span>
            {/if}
            <div class="text">
              <span class="name" title={e.extensionId}>{e.name}</span>
              <span class="detail">{[t(`ext.browser.${e.browser}` as Key), e.profile ?? t('ext.noProfile'), e.version].filter(Boolean).join(' · ')}</span>
              <span class="badges">
                <span class="badge" class:warn={e.source === 'external'}>{t(`ext.source.${e.source}` as Key)}</span>
                {#if e.enabled === false}<span class="badge">{t('ext.disabled')}</span>{/if}
                {#if e.needsAdmin}<span class="badge">{t('ext.needsAdmin')}</span>{/if}
              </span>
            </div>
            {#if e.removal === 'browser'}
              <button class="btn small" onclick={() => manage(e)}>{t('ext.manage')}</button>
            {:else if e.removal === 'none'}
              <span class="badge protected" title={t('ext.policyHint')}>{t('ext.policy')}</span>
            {/if}
          </li>
        {:else}
          <li class="none">{t('ext.none')}</li>
        {/each}
      </ul>
      <p class="note">
        {t('ext.count', { n: formatCount(items.length) })}{#if skipped > 0} · {t('ext.skipped', { n: formatCount(skipped) })}{/if}
      </p>
      <p class="note">{t('ext.note')}</p>
    {/if}
  {:else}
    <div class="panel">
      <h2>{t('ext.confirmTitle', { n: formatCount(step === 'confirm' ? chosenKeys.length : removingCount) })}</h2>
      {#if error}<p class="message bad" role="alert">{error}</p>{/if}
      {#if step === 'confirm'}
        <p class="desc">{t('ext.confirm')}</p>
        <ul class="list paths">{#each chosenRegistry as path (path)}<li><span class="name" title={path}>{path}</span></li>{/each}</ul>
        {#if chosenAdmin}<p class="note">{t('ext.adminNote')}</p>{/if}
        <div class="row">
          <button class="btn" onclick={back}>{t('uninstall.cancel')}</button>
          <button class="btn primary" onclick={remove}>{t('ext.removeRun')}</button>
        </div>
      {:else if step === 'removing'}
        <p class="desc" role="status">{t('uninstall.removing')}</p>
      {:else if step === 'done' && report}
        <p class="message" class:bad={report.failed > 0}>
          {t('ext.removed', { n: formatCount(report.done) })}{#if report.failed > 0}{' '}{t('ext.removeFailed', { n: formatCount(report.failed) })}{/if}
        </p>
        {#if report.errors.length}
          <ul class="list paths">{#each report.errors as message (message)}<li><span class="name bad" title={removalError(message)}>{removalError(message)}</span></li>{/each}</ul>
        {/if}
        <p class="note">{t('ext.afterRemove')}</p>
        <div class="row"><button class="btn primary" onclick={back}>{t('uninstall.back')}</button></div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .extensions, .panel { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 10px; }
  .empty { flex: 1; display: grid; place-items: center; margin: 0; color: var(--text-faint); }
  .toolbar, .row { flex: none; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .search, select { min-width: 0; height: 34px; padding: 0 12px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); color: var(--text); font: inherit; font-size: 13px; }
  .toolbar > .search { flex: 1; }
  .search:focus, select:focus { outline: 2px solid var(--accent); outline-offset: -1px; }
  .list { flex: 1; min-height: 0; overflow-y: auto; margin: 0; padding: 0 6px 0 0; list-style: none; display: flex; flex-direction: column; gap: 4px; }
  .list li { flex: none; display: flex; align-items: center; gap: 12px; min-height: 46px; padding: 6px 12px; border-radius: var(--radius); background: var(--surface); }
  .list.paths { flex: 0 1 auto; max-height: 180px; }
  .list.paths li { min-height: 30px; }
  .list li.none { justify-content: center; color: var(--text-faint); font-size: 13px; }
  .text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .name, .detail { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .name { font-size: 13px; }
  .name.bad { color: var(--warn); }
  .detail { font-size: 12px; color: var(--text-faint); }
  .badges { display: flex; flex-wrap: wrap; gap: 4px; }
  .badge { flex: none; padding: 1px 6px; border-radius: 4px; background: var(--surface-active); font-size: 10px; color: var(--text-dim); }
  .badge.warn, .protected { color: var(--warn); }
  .btn.small { flex: none; height: 30px; padding: 0 14px; }
  .note { flex: none; margin: 0; font-size: 12px; line-height: 1.45; color: var(--text-faint); }
  h2 { margin: 0; font-size: 18px; font-weight: 600; }
  .desc { margin: 0; font-size: 13px; line-height: 1.5; color: var(--text-dim); }
  .message { flex: none; margin: 0; padding: 8px 12px; border-radius: var(--radius); background: rgba(70, 201, 139, 0.12); font-size: 13px; line-height: 1.5; }
  .message.bad { background: rgba(240, 179, 69, 0.12); color: var(--warn); }
  .link { flex: none; font-size: 12px; color: var(--accent-2); }
  .link:hover { text-decoration: underline; }
  .check, .check-space { flex: none; width: 16px; }
  input[type='checkbox'] { height: 16px; margin: 0; accent-color: var(--accent); }
  button:disabled, input:disabled, select:disabled { opacity: 0.5; }
</style>
