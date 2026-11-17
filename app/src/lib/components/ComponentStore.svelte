<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { formatBytes } from '../format';
  import { t, tOr } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import { componentAnalyze, componentPrepare, componentStart, componentStatus, maintenanceCancel,
    type ComponentAnalysis, type ComponentStatus } from '../tools';

  let analysis = $state<ComponentAnalysis | null>(null);
  let status = $state<ComponentStatus>({ state: 'idle', report: null, error: null });
  let working = $state<'analyze' | 'prepare' | 'start' | null>(null);
  let ready = $state(false);
  let confirmed = $state(false);
  let acknowledged = $state(false);
  let restoreError = $state<string | null>(null);
  let error = $state('');
  let stopping = $state(false);
  let alive = true;
  let request: string | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let statusKnown = $state(false);
  const errorText = (e: unknown) => tOr(`err.${e}`, String(e));
  const blocked = $derived(!!working || status.state === 'running' || !statusKnown);

  async function poll() {
    if (!alive) return;
    try {
      if (!working) {
        const value = await componentStatus();
        if (alive && !working) { status = value; statusKnown = true; }
      }
    } catch (e) { if (alive) error = errorText(e); }
    finally { if (alive) timer = setTimeout(poll, 1000); }
  }
  onMount(() => { if (inTauri()) void poll(); });
  onDestroy(() => {
    alive = false;
    if (timer) clearTimeout(timer);
    if (request) void maintenanceCancel(request).catch(() => {});
  });

  async function analyze() {
    if (blocked) return;
    const id = crypto.randomUUID(); request = id;
    analysis = null; error = ''; ready = false; restoreError = null; confirmed = false; acknowledged = false;
    working = 'analyze'; stopping = false;
    try {
      const value = await componentAnalyze(id);
      if (alive) analysis = value;
    } catch (e) { if (alive) { error = errorText(e); analysis = null; } }
    finally { if (alive) { if (request === id) request = null; working = null; stopping = false; } }
  }
  async function stop() {
    if (!request || stopping) return;
    stopping = true;
    try { await maintenanceCancel(request); }
    catch (e) { if (alive) { error = errorText(e); stopping = false; } }
  }
  async function prepare(bypass = false) {
    if (blocked || !analysis?.cleanupRecommended || !confirmed || (bypass && (!restoreError || !acknowledged))) return;
    working = 'prepare'; ready = false; error = '';
    try {
      const value = await componentPrepare(bypass);
      if (!alive) return;
      ready = value.ready; restoreError = value.restoreError; acknowledged = false;
    } catch (e) { if (alive) { error = errorText(e); ready = false; } }
    finally { if (alive) working = null; }
  }
  async function start() {
    if (blocked || !ready || !confirmed || !analysis?.cleanupRecommended) return;
    working = 'start'; error = '';
    try {
      const value = await componentStart(true);
      if (!alive) return;
      status = value; statusKnown = true; ready = false; restoreError = null; analysis = null; confirmed = false;
    } catch (e) { if (alive) { error = errorText(e); ready = false; } }
    finally { if (alive) working = null; }
  }
</script>

<section class="panel">
  <h1>{t('component.title')}</h1>
  <p class="note">{t('component.desc')}</p>
  <p class="warning">{t('component.safety')}</p>
  {#if !inTauri()}<p>{t('tool.needApp')}</p>{:else}
    <div class="controls">
      <button class="btn primary" disabled={blocked} onclick={analyze}>{t('component.analyze')}</button>
      {#if working === 'analyze'}<button class="btn" disabled={stopping} onclick={stop}>{t(stopping ? 'scan.stopping' : 'scan.stop')}</button>{/if}
    </div>
    {#if working}<p class="note" role="status">{t(working === 'analyze' ? 'component.analyzing' : working === 'prepare' ? 'uninstall.preparing' : 'component.starting')}</p>{/if}
    {#if error}<p class="warning" role="alert">{error}</p>{/if}
    {#if statusKnown}
      <p class="state" role="status">{tOr(`component.state.${status.state}`, status.state)}</p>
      {#if status.state === 'running'}<p class="warning">{t('component.runningNote')}</p>{/if}
      {#if status.report}
        <p class="note">{t('uninstall.exitCode', { code: status.report.exitCode })}</p>
        {#if status.report.rebootRequired}<p class="warning">{t('component.reboot')}</p>{/if}
        <details><summary>{t('component.output')}</summary><pre>{status.report.output}</pre></details>
      {/if}
      {#if status.error}<p class="warning" role="alert">{status.state === 'completed' ? t('component.completedWarning') : t('component.failedNote')} {errorText(status.error)}</p>{/if}
    {/if}
    {#if analysis}
      <dl>
        <div><dt>{t('component.actual')}</dt><dd>{formatBytes(analysis.actualSize)}</dd></div>
        <div><dt>{t('component.shared')}</dt><dd>{formatBytes(analysis.sharedSize)}</dd></div>
        <div><dt>{t('component.backups')}</dt><dd>{formatBytes(analysis.backupsSize)}</dd></div>
        <div><dt>{t('component.cache')}</dt><dd>{formatBytes(analysis.cacheSize)}</dd></div>
        <div><dt>{t('component.packages')}</dt><dd>{analysis.reclaimablePackages}</dd></div>
        <div><dt>{t('component.last')}</dt><dd>{analysis.lastCleanup || t('maintenance.unknown')}</dd></div>
      </dl>
      <p class="note">{t('component.sizesNote')}</p>
      <p class="state">{t(analysis.cleanupRecommended ? 'component.recommended' : 'component.notRecommended')}</p>
      <details><summary>{t('component.output')}</summary><pre>{analysis.output}</pre></details>
      {#if analysis.cleanupRecommended}
        <label class="check"><input type="checkbox" bind:checked={confirmed} disabled={blocked || ready} />{t('component.confirm')}</label>
        {#if restoreError}<p class="warning" role="alert">{t('component.restoreFailed', { error: tOr(`uninstall.restoreError.${restoreError}`, restoreError) })}</p>{/if}
        {#if restoreError && !ready}
          <button class="btn" disabled={blocked || !confirmed} onclick={() => prepare()}>{t('uninstall.retryRestore')}</button>
          <label class="check"><input type="checkbox" bind:checked={acknowledged} disabled={blocked} />{t('uninstall.continueAcknowledge')}</label>
          <button class="btn" disabled={blocked || !confirmed || !acknowledged} onclick={() => prepare(true)}>{t('uninstall.continueWithoutRestore')}</button>
        {:else if !ready}
          <p class="note">{t('uninstall.restorePreflight')}</p>
          <button class="btn" disabled={blocked || !confirmed} onclick={() => prepare()}>{t('component.prepare')}</button>
        {/if}
        {#if ready}<button class="btn primary" disabled={blocked || !confirmed} onclick={start}>{t('component.start')}</button>{/if}
      {/if}
    {/if}
  {/if}
</section>

<style>
  .panel { flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 10px; padding-right: 8px; }
  h1 { margin: 0; font-size: 20px; } p { margin: 0; line-height: 1.5; font-size: 12px; }
  .note { color: var(--text-dim); } .warning { color: var(--warn); background: rgba(240,179,69,.1); border-radius: 8px; padding: 8px 12px; }
  .controls { display: flex; gap: 8px; } .panel > .btn { align-self: flex-start; flex-shrink: 0; }
  .state { color: var(--accent-2); font-size: 13px; } .check { display: flex; align-items: flex-start; gap: 8px; font-size: 12px; line-height: 1.5; } input { accent-color: var(--accent); }
  dl { margin: 0; display: grid; grid-template-columns: 1fr 1fr; gap: 8px; } dl div { padding: 10px; border-radius: 8px; background: var(--surface); }
  dt { color: var(--text-dim); font-size: 12px; } dd { margin: 5px 0 0; font-size: 14px; overflow-wrap: anywhere; }
  details { font-size: 12px; } summary { cursor: pointer; } pre { white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; font-size: 11px; }
</style>
