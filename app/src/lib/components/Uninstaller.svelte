<script lang="ts">
  import { onDestroy } from 'svelte';
  import { cancel } from '../scan';
  import Icon from './Icon.svelte';
  import { formatBytes, formatCount } from '../format';
  import { t, tOr, type Key } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import {
    appUninstall,
    appsList,
    leftoversRemove,
    leftoversScan,
    reveal,
    type AppInfo,
    type Leftover,
    type RemoveReport,
    type UninstallResult,
  } from '../tools';

  type Step = 'list' | 'confirm' | 'running' | 'leftovers' | 'scanning' | 'done';
  type Sort = 'name' | 'size' | 'date';

  let apps = $state<AppInfo[]>([]);
  let loading = $state(true);
  let query = $state('');
  let sort = $state<Sort>('name');
  let step = $state<Step>('list');
  let app = $state<AppInfo | null>(null);
  let result = $state<UninstallResult | null>(null);
  let leftovers = $state<Leftover[]>([]);
  let picked = $state<Record<string, boolean>>({});
  let report = $state<RemoveReport | null>(null);
  let error = $state('');
  let stopping = $state(false);
  let destroyed = false;

  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const list = apps.filter((a) => !q || a.name.toLowerCase().includes(q) || (a.publisher ?? '').toLowerCase().includes(q));
    if (sort === 'size') return [...list].sort((a, b) => (b.size ?? 0) - (a.size ?? 0));
    if (sort === 'date') return [...list].sort((a, b) => (b.installDate ?? '').localeCompare(a.installDate ?? ''));
    return list;
  });
  const chosen = $derived(leftovers.filter((l) => picked[l.id]));

  const errorText = (e: unknown) => tOr(`err.${e}`, String(e));

  async function load() {
    if (!inTauri()) return;
    loading = true;
    try {
      apps = await appsList();
    } finally {
      loading = false;
    }
  }

  function start(target: AppInfo) {
    app = target;
    result = null;
    report = null;
    leftovers = [];
    picked = {};
    error = '';
    step = 'confirm';
  }

  async function uninstall() {
    if (!app) return;
    step = 'running';
    stopping = false;
    try {
      result = await appUninstall(app.id);
      if (destroyed) return;
      if (!result.stillInstalled) await scanLeftovers();
      else step = 'leftovers';
    } catch (e) {
      if (destroyed) return;
      error = errorText(e);
      step = 'confirm';
    }
  }

  async function stopWaiting() {
    stopping = true;
    try {
      await cancel();
    } catch (e) {
      error = errorText(e);
      stopping = false;
    }
  }

  async function scanLeftovers() {
    if (!app) return;
    step = 'scanning';
    error = '';
    leftovers = [];
    picked = {};
    try {
      leftovers = await leftoversScan(app.id);
      picked = Object.fromEntries(leftovers.map((l) => [l.id, l.recommended]));
      result = { exitCode: result?.exitCode ?? null, stillInstalled: false };
    } catch (e) {
      error = errorText(e);
      if (e === 'still_installed') result = { exitCode: result?.exitCode ?? null, stillInstalled: true };
    }
    step = 'leftovers';
  }

  async function remove() {
    if (!app) return;
    step = 'scanning';
    try {
      report = await leftoversRemove(
        app.id,
        chosen.map((l) => l.id),
      );
      step = 'done';
    } catch (e) {
      error = errorText(e);
      if (e === 'still_installed') {
        result = { exitCode: result?.exitCode ?? null, stillInstalled: true };
        leftovers = [];
        picked = {};
      }
      step = 'leftovers';
    }
  }

  function back() {
    step = 'list';
    app = null;
    load();
  }

  onDestroy(() => {
    destroyed = true;
    if (step === 'running') void stopWaiting();
  });

  load();
</script>

<div class="uninstaller">
  {#if !inTauri()}
    <p class="empty">{t('tool.needApp')}</p>
  {:else if step === 'list'}
    <div class="toolbar">
      <input class="search" type="search" placeholder={t('uninstall.search')} bind:value={query} />
      <div class="sorts" role="tablist">
        {#each ['name', 'size', 'date'] as const as s (s)}
          <button role="tab" aria-selected={sort === s} class:active={sort === s} onclick={() => (sort = s)}>
            {t(`uninstall.sort.${s}` as Key)}
          </button>
        {/each}
      </div>
    </div>
    {#if loading}
      <p class="empty">{t('uninstall.loading')}</p>
    {:else}
      <ul class="list">
        {#each shown as a (a.id)}
          <li>
            <div class="text">
              <span class="name">{a.name}</span>
              <span class="detail">
                {[a.publisher, a.version, a.installDate].filter(Boolean).join(' · ')}
              </span>
            </div>
            <span class="amount">{a.size ? formatBytes(a.size) : ''}</span>
            <button class="btn small" onclick={() => start(a)}>{t('uninstall.action')}</button>
          </li>
        {:else}
          <li class="none">{t('uninstall.none')}</li>
        {/each}
      </ul>
      <p class="note">{t('uninstall.count', { n: formatCount(apps.length) })} · {t('uninstall.storeLater')}</p>
    {/if}
  {:else if app}
    <div class="panel">
      {#if step !== 'running' && step !== 'scanning'}
        <button class="link back" onclick={back}><Icon name="back" size={14} /> {t('uninstall.back')}</button>
      {/if}
      <h2>{app.name}</h2>
      <p class="sub">{[app.publisher, app.version].filter(Boolean).join(' · ')}</p>

      {#if error}<p class="message bad" role="alert">{error}</p>{/if}

      {#if step === 'confirm'}
        <p class="desc">{t('uninstall.confirm')}</p>
        <div class="row">
          <button class="btn" onclick={back}>{t('uninstall.cancel')}</button>
          <button class="btn primary" onclick={uninstall}>{t('uninstall.run')}</button>
        </div>
      {:else if step === 'running'}
        <p class="desc">{t('uninstall.running')}</p>
        <p class="note">{t('uninstall.stopWaitingHint')}</p>
        <div class="row"><button class="btn" disabled={stopping} onclick={stopWaiting}>{t('uninstall.stopWaiting')}</button></div>
      {:else if step === 'scanning'}
        <p class="desc">{t('uninstall.working')}</p>
      {:else if step === 'leftovers'}
        {#if result?.stillInstalled}
          <p class="message bad">{t('uninstall.stillInstalled')}</p>
        {/if}
        {#if result?.stillInstalled || (error && leftovers.length === 0)}
          <div class="row">
            <button class="btn" onclick={back}>{t('uninstall.back')}</button>
            <button class="btn primary" onclick={scanLeftovers}>{t('uninstall.scan')}</button>
          </div>
        {:else if leftovers.length === 0}
          <p class="message">{t('uninstall.clean')}</p>
          <div class="row"><button class="btn primary" onclick={back}>{t('uninstall.finish')}</button></div>
        {:else}
          <p class="desc">{t('uninstall.leftoversHint')}</p>
          <ul class="list leftovers">
            {#each leftovers as l (l.id)}
              <li>
                <button class="check" role="checkbox" aria-checked={picked[l.id]} onclick={() => (picked[l.id] = !picked[l.id])}>
                  <span class="box" class:on={picked[l.id]}><Icon name="check" size={12} /></span>
                </button>
                <div class="text">
                  <span class="name" title={l.path}>{l.path}</span>
                  <span class="detail">
                    {t(`uninstall.kind.${l.kind}` as Key)}{l.recommended ? '' : ` · ${t('uninstall.notPreselected')}`}
                  </span>
                </div>
                {#if l.kind === 'folder'}
                  <button class="link" onclick={() => reveal(l.path)}>{t('tool.reveal')}</button>
                {/if}
                <span class="amount">{l.bytes > 0 ? formatBytes(l.bytes) : ''}</span>
              </li>
            {/each}
          </ul>
          <p class="note">{t('uninstall.removeNote')}</p>
          <div class="row">
            <button class="btn" onclick={back}>{t('uninstall.skip')}</button>
            <button class="btn primary" disabled={chosen.length === 0} onclick={remove}>
              {t('uninstall.remove', { n: formatCount(chosen.length) })}
            </button>
          </div>
        {/if}
      {:else if step === 'done' && report}
        <p class="message" class:bad={report.failed > 0}>
          {t('uninstall.removed', { n: formatCount(report.done), size: formatBytes(report.bytes) })}
          {#if report.failed > 0}{t('uninstall.removeFailed', { n: formatCount(report.failed) })}{/if}
        </p>
        <div class="row"><button class="btn primary" onclick={back}>{t('uninstall.finish')}</button></div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .uninstaller {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .empty {
    flex: 1;
    display: grid;
    place-items: center;
    margin: 0;
    color: var(--text-faint);
  }

  .toolbar {
    flex: none;
    display: flex;
    gap: 10px;
    align-items: center;
  }

  .search {
    flex: 1;
    min-width: 0;
    height: 34px;
    padding: 0 12px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
    font: inherit;
    font-size: 13px;
  }

  .search:focus {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
  }

  .sorts {
    display: flex;
    padding: 2px;
    border-radius: 999px;
    border: 1px solid var(--border);
  }

  .sorts button {
    height: 28px;
    padding: 0 12px;
    border-radius: 999px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .sorts button.active {
    background: var(--surface-active);
    color: var(--text);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    margin: 0;
    padding: 0 6px 0 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .list li {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 46px;
    padding: 6px 12px;
    border-radius: var(--radius);
    background: var(--surface);
  }

  .list li.none {
    justify-content: center;
    color: var(--text-faint);
    font-size: 13px;
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .name,
  .detail {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .name {
    font-size: 13px;
  }

  .detail {
    font-size: 12px;
    color: var(--text-faint);
  }

  .amount {
    flex: none;
    min-width: 64px;
    text-align: right;
    font-size: 13px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .btn.small {
    flex: none;
    height: 30px;
    padding: 0 14px;
  }

  .note {
    flex: none;
    margin: 0;
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-faint);
  }

  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .back {
    align-self: flex-start;
    display: flex;
    align-items: center;
    gap: 4px;
  }

  h2 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
  }

  .sub,
  .desc {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
    color: var(--text-dim);
  }

  .row {
    display: flex;
    gap: 8px;
  }

  .message {
    margin: 0;
    padding: 8px 12px;
    border-radius: var(--radius);
    background: rgba(70, 201, 139, 0.12);
    font-size: 13px;
  }

  .message.bad {
    background: rgba(240, 179, 69, 0.12);
    color: var(--warn);
  }

  .link {
    flex: none;
    font-size: 12px;
    color: var(--accent-2);
  }

  .link:hover {
    text-decoration: underline;
  }

  .check {
    flex: none;
    display: grid;
    place-items: center;
  }

  .box {
    width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
    border-radius: 4px;
    border: 1.5px solid var(--text-faint);
    color: transparent;
  }

  .box.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
</style>
