<script lang="ts">
  import { onDestroy } from 'svelte';
  import { cancel } from '../scan';
  import { formatCount } from '../format';
  import { t, tOr } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import { updatePrepare, updatesHistory, updatesList, updateUninstall, whenIdle, type UpdateResult, type WindowsUpdate } from '../tools';

  type Step = 'list' | 'confirm' | 'prepare' | 'restore' | 'running' | 'result';

  let updates = $state<WindowsUpdate[]>([]);
  let loading = $state(true);
  let loadError = $state('');
  let query = $state('');
  let step = $state<Step>('list');
  let target = $state<WindowsUpdate | null>(null);
  let restoreError = $state<string | null>(null);
  let continueConfirmed = $state(false);
  let result = $state<UpdateResult | null>(null);
  let error = $state('');
  let stopping = $state(false);
  let stopped = $state(false);
  let destroyed = false;
  let version = 0;

  const busy = $derived(loading || step === 'prepare' || step === 'running');
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return updates.filter((u) => !q || u.id.toLowerCase().includes(q) || (u.description ?? '').toLowerCase().includes(q));
  });
  const errorText = (e: unknown) => tOr(`err.${String(e)}`, String(e));
  const restoreErrorText = (e: string) => tOr(`uninstall.restoreError.${e}`, errorText(e));

  /** Diễn giải mã thoát của wusa.exe. */
  function outcome(r: UpdateResult): { text: string; ok: boolean } {
    const code = r.exitCode;
    if (r.restartRequired) return { text: t('wu.result.restart'), ok: true };
    if (code === 0 && !r.stillInstalled) return { text: t('wu.result.removed'), ok: true };
    if (code === 2359303) return { text: t('wu.result.notInstalled'), ok: false };
    if (code === 1223 || code === 2147943623) return { text: t('wu.result.cancelled'), ok: false };
    if (code === null) return { text: t(stopped ? 'wu.result.stopped' : 'wu.result.unknown'), ok: false };
    if (code === 0) return { text: t('wu.result.stillListed'), ok: false };
    return { text: t('wu.result.failed', { code: `0x${code.toString(16).toUpperCase()}` }), ok: false };
  }

  async function load() {
    if (!inTauri()) { loading = false; return; }
    const current = ++version;
    loading = true;
    loadError = '';
    try {
      const list = await whenIdle(updatesList, () => !destroyed && current === version);
      if (destroyed || current !== version) return;
      updates = list;
    } catch (e) {
      if (!destroyed && current === version) loadError = errorText(e);
    } finally {
      if (!destroyed && current === version) loading = false;
    }
  }

  function start(update: WindowsUpdate) {
    if (busy) return;
    target = update;
    restoreError = null;
    continueConfirmed = false;
    result = null;
    error = '';
    step = 'confirm';
  }

  async function prepare(continueWithoutRestore = false) {
    if (busy || !target) return;
    if (continueWithoutRestore && (!restoreError || !continueConfirmed)) return;
    step = 'prepare';
    error = '';
    try {
      const preparation = await updatePrepare(target.id, continueWithoutRestore);
      if (destroyed) return;
      if (!preparation.ready) {
        restoreError = preparation.restoreError;
        continueConfirmed = false;
        step = restoreError ? 'restore' : 'confirm';
        if (!restoreError) error = t('err.not_prepared');
        return;
      }
      await run();
    } catch (e) {
      if (destroyed) return;
      error = errorText(e);
      step = restoreError ? 'restore' : 'confirm';
    }
  }

  async function run() {
    if (destroyed || !target) return;
    step = 'running';
    stopping = false;
    stopped = false;
    try {
      const r = await updateUninstall(target.id);
      if (destroyed) return;
      result = r;
    } catch (e) {
      if (destroyed) return;
      error = errorText(e);
    }
    if (!destroyed) step = 'result';
  }

  async function stopWaiting() {
    if (step !== 'running' || stopping) return;
    stopping = true;
    stopped = true;
    try { await cancel(); }
    catch (e) { if (!destroyed) { error = errorText(e); stopping = false; } }
  }

  async function openHistory() {
    try { await updatesHistory(); }
    catch (e) { if (!destroyed) error = errorText(e); }
  }

  function back() {
    if (busy) return;
    step = 'list';
    target = null;
    error = '';
    void load();
  }

  onDestroy(() => {
    destroyed = true;
    ++version;
    if (step === 'running') void cancel().catch(() => {});
  });
  void load();
</script>

<div class="updates">
  {#if !inTauri()}
    <p class="empty">{t('tool.needApp')}</p>
  {:else if step === 'list'}
    <div class="toolbar">
      <input class="search" type="search" aria-label={t('wu.search')} placeholder={t('wu.search')} bind:value={query} disabled={busy} />
      <button class="btn small" disabled={busy} onclick={load}>{t('ext.reload')}</button>
      <button class="btn small" onclick={openHistory}>{t('wu.history')}</button>
    </div>
    {#if error}<p class="message bad" role="alert">{error}</p>{/if}
    {#if loading}
      <p class="empty">{t('wu.loading')}</p>
    {:else if loadError}
      <p class="message bad" role="alert">{loadError}</p>
    {:else}
      <ul class="list">
        {#each shown as u (u.id)}
          <li>
            <div class="text">
              <span class="name">{u.id}</span>
              <span class="detail">{[u.description, u.installedOn].filter(Boolean).join(' · ')}</span>
            </div>
            <button class="btn small" disabled={busy} onclick={() => start(u)}>{t('wu.action')}</button>
          </li>
        {:else}
          <li class="none">{t('wu.none')}</li>
        {/each}
      </ul>
      <p class="note">{t('wu.count', { n: formatCount(updates.length) })} · {t('wu.note')}</p>
    {/if}
  {:else if target}
    <div class="panel">
      {#if !busy}
        <button class="link" onclick={back}>{t('uninstall.back')}</button>
      {/if}
      <h2>{target.id}</h2>
      <p class="sub">{[target.description, target.installedOn].filter(Boolean).join(' · ')}</p>
      {#if error}<p class="message bad" role="alert">{error}</p>{/if}

      {#if step === 'confirm'}
        <p class="desc">{t('wu.confirm')}</p>
        <p class="message bad">{t('wu.warning')}</p>
        <p class="note">{t('uninstall.restorePreflight')}</p>
        <div class="row">
          <button class="btn" onclick={back}>{t('uninstall.cancel')}</button>
          <button class="btn primary" onclick={() => prepare()}>{t('wu.run')}</button>
        </div>
      {:else if step === 'prepare'}
        <p class="desc" role="status">{t('uninstall.preparing')}</p>
      {:else if step === 'restore'}
        <p class="message bad" role="alert">{t('wu.restoreFailed', { error: restoreErrorText(restoreError ?? '') })}</p>
        <label class="check-label"><input type="checkbox" bind:checked={continueConfirmed} />{t('uninstall.continueAcknowledge')}</label>
        <div class="row">
          <button class="btn" onclick={back}>{t('uninstall.cancel')}</button>
          <button class="btn" onclick={() => prepare()}>{t('uninstall.retryRestore')}</button>
          <button class="btn primary" disabled={!continueConfirmed} onclick={() => prepare(true)}>{t('uninstall.continueWithoutRestore')}</button>
        </div>
      {:else if step === 'running'}
        <p class="desc" role="status">{t('wu.running')}</p>
        <p class="note">{t('wu.stopHint')}</p>
        <div class="row"><button class="btn" disabled={stopping} onclick={stopWaiting}>{t(stopping ? 'scan.stopping' : 'uninstall.stopWaiting')}</button></div>
      {:else if step === 'result'}
        {#if result}
          {@const o = outcome(result)}
          <p class="message" class:bad={!o.ok}>{o.text}</p>
          {#if !o.ok}<p class="note">{t('wu.fallback')}</p>{/if}
        {/if}
        <div class="row">
          <button class="btn primary" onclick={back}>{t('uninstall.back')}</button>
          {#if !result || !outcome(result).ok}<button class="btn" onclick={openHistory}>{t('wu.history')}</button>{/if}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .updates, .panel { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 10px; }
  .empty { flex: 1; display: grid; place-items: center; margin: 0; color: var(--text-faint); }
  .toolbar, .row { flex: none; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .search { flex: 1; min-width: 0; height: 34px; padding: 0 12px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); color: var(--text); font: inherit; font-size: 13px; }
  .search:focus { outline: 2px solid var(--accent); outline-offset: -1px; }
  .list { flex: 1; min-height: 0; overflow-y: auto; margin: 0; padding: 0 6px 0 0; list-style: none; display: flex; flex-direction: column; gap: 4px; }
  .list li { flex: none; display: flex; align-items: center; gap: 12px; min-height: 46px; padding: 6px 12px; border-radius: var(--radius); background: var(--surface); }
  .list li.none { justify-content: center; color: var(--text-faint); font-size: 13px; }
  .text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .name, .detail { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .name { font-size: 13px; }
  .detail { font-size: 12px; color: var(--text-faint); }
  .btn.small { flex: none; height: 30px; padding: 0 14px; }
  .note { flex: none; margin: 0; font-size: 12px; line-height: 1.45; color: var(--text-faint); }
  h2 { margin: 0; font-size: 18px; font-weight: 600; }
  .sub, .desc { margin: 0; font-size: 13px; line-height: 1.5; color: var(--text-dim); }
  .message { flex: none; margin: 0; padding: 8px 12px; border-radius: var(--radius); background: rgba(70, 201, 139, 0.12); font-size: 13px; line-height: 1.5; }
  .message.bad { background: rgba(240, 179, 69, 0.12); color: var(--warn); }
  .link { align-self: flex-start; font-size: 12px; color: var(--accent-2); }
  .link:hover { text-decoration: underline; }
  input[type='checkbox'] { width: 16px; height: 16px; margin: 0; accent-color: var(--accent); }
  .check-label { display: flex; gap: 8px; align-items: center; font-size: 13px; line-height: 1.5; }
  button:disabled, input:disabled { opacity: 0.5; }
</style>
