<script lang="ts">
  import { onDestroy } from 'svelte';
  import { cancel } from '../scan';
  import Icon from './Icon.svelte';
  import { formatBytes, formatCount } from '../format';
  import { t, tOr, type Key } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import {
    appsList,
    whenIdle,
    appsUninstall,
    forceTarget,
    uninstallPrepare,
    leftoversRemove,
    leftoversScan,
    reveal,
    type AppInfo,
    type BatchUninstallReport,
    type Leftover,
    type RemoveReport,
    type UninstallMode,
    type UninstallProgress,
    type UninstallResult,
  } from '../tools';

  type Step = 'list' | 'warning' | 'confirm' | 'prepare' | 'restore' | 'running' | 'results' | 'leftovers' | 'scanning' | 'removing' | 'done';
  type Sort = 'name' | 'size' | 'date' | 'used';
  type Filter = 'all' | 'desktop' | 'store' | 'builtIn' | 'companions' | 'large' | 'stale' | 'recent';
  const DAY = 86_400;
  const LARGE = 1024 ** 3;
  const STALE_DAYS = 90;
  const RECENT_DAYS = 30;
  type WaitSession = { active: boolean; finished: boolean; stopRequested: boolean; cancelSent: boolean };

  let apps = $state<AppInfo[]>([]);
  let loading = $state(true);
  let loadError = $state('');
  let query = $state('');
  let sort = $state<Sort>('name');
  let filter = $state<Filter>('all');
  let mode = $state<UninstallMode>('standard');
  let step = $state<Step>('list');
  let selected = $state<Record<string, boolean>>({});
  let targets = $state<AppInfo[]>([]);
  let app = $state<AppInfo | null>(null);
  let result = $state<UninstallResult | null>(null);
  let batch = $state<BatchUninstallReport | null>(null);
  let progress = $state<UninstallProgress | null>(null);
  let leftovers = $state<Leftover[]>([]);
  let picked = $state<Record<string, boolean>>({});
  let scanSucceeded = $state(false);
  let report = $state<RemoveReport | null>(null);
  let error = $state('');
  let forceAcknowledged = $state(false);
  let restoreError = $state<string | null>(null);
  let continueConfirmed = $state(false);
  let targetName = $state('');
  let targetPath = $state('');
  let creatingTarget = $state(false);
  let stopping = $state(false);
  let destroyed = false;
  let loadVersion = 0;
  let waitSession: WaitSession | null = null;

  const busy = $derived(creatingTarget || ['prepare', 'running', 'scanning', 'removing'].includes(step));
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const list = apps.filter((a) => matches(a, filter) &&
      (!q || a.name.toLowerCase().includes(q) || (a.publisher ?? '').toLowerCase().includes(q)),
    );
    const byName = (a: AppInfo, b: AppInfo) => a.name.localeCompare(b.name);
    if (sort === 'size') return [...list].sort((a, b) => (b.size ?? 0) - (a.size ?? 0) || byName(a, b));
    if (sort === 'date') return [...list].sort((a, b) => (b.installDate ?? '').localeCompare(a.installDate ?? '') || byName(a, b));
    // Lâu không mở lên trước; ứng dụng không có ghi nhận xếp cuối.
    if (sort === 'used') return [...list].sort((a, b) => (a.lastUsed ?? Infinity) - (b.lastUsed ?? Infinity) || byName(a, b));
    return [...list].sort(byName);
  });
  const selectable = $derived(shown.filter((a) => a.removable));
  const allSelected = $derived(selectable.length > 0 && selectable.every((a) => selected[a.id]));
  const selectedApps = $derived(apps.filter((a) => a.removable && selected[a.id]));
  const chosen = $derived(leftovers.filter((l) => picked[l.id]));

  const now = Math.floor(Date.now() / 1000);
  const daysAgo = (unix: number) => Math.max(0, Math.floor((now - unix) / DAY));
  const installedDaysAgo = (date: string | null) => {
    const time = date ? Date.parse(`${date}T00:00:00`) : NaN;
    return Number.isNaN(time) ? null : daysAgo(Math.floor(time / 1000));
  };

  function matches(a: AppInfo, f: Filter): boolean {
    switch (f) {
      case 'all': return true;
      case 'builtIn': return a.builtIn;
      case 'companions': return a.companions.length > 0;
      case 'large': return (a.size ?? 0) >= LARGE;
      // Chỉ ứng dụng có ghi nhận lần mở; không ghi nhận không có nghĩa là không dùng.
      case 'stale': return a.lastUsed !== null && daysAgo(a.lastUsed) > STALE_DAYS;
      case 'recent': { const d = installedDaysAgo(a.installDate); return d !== null && d <= RECENT_DAYS; }
      default: return a.kind === f;
    }
  }

  function lastUsedText(a: AppInfo): string {
    if (a.lastUsed === null) return '';
    const d = daysAgo(a.lastUsed);
    return d === 0 ? t('uninstall.usedToday') : t('uninstall.usedDaysAgo', { n: formatCount(d) });
  }

  const errorText = (e: unknown) => tOr(`err.${String(e)}`, String(e));
  const restoreErrorText = (e: string) => tOr(`uninstall.restoreError.${e}`, errorText(e));
  const targetFor = (id: string) => targets.find((a) => a.id === id);

  async function load() {
    if (!inTauri()) { loading = false; return; }
    const version = ++loadVersion;
    loading = true;
    loadError = '';
    try {
      const list = await whenIdle(appsList, () => !destroyed && version === loadVersion);
      if (destroyed || version !== loadVersion) return;
      apps = list;
      selected = Object.fromEntries(list.filter((a) => a.removable && selected[a.id]).map((a) => [a.id, true]));
    } catch (e) {
      if (!destroyed && version === loadVersion) loadError = errorText(e);
    } finally {
      if (!destroyed && version === loadVersion) loading = false;
    }
  }

  function changeMode(next: UninstallMode) {
    if (busy || step !== 'list') return;
    mode = next;
    selected = {};
    error = '';
  }

  function selectShown() {
    const checked = !allSelected;
    selected = { ...selected, ...Object.fromEntries(selectable.map((a) => [a.id, checked])) };
  }

  function start(list: AppInfo[]) {
    if (busy || loading || loadError || !list.length || list.some((a) => !a.removable) || (mode === 'forced' && (list.length !== 1 || list[0].kind !== 'desktop'))) return;
    targets = [...list];
    app = list.length === 1 ? list[0] : null;
    result = null;
    batch = null;
    progress = null;
    report = null;
    leftovers = [];
    picked = {};
    scanSucceeded = false;
    forceAcknowledged = false;
    restoreError = null;
    continueConfirmed = false;
    error = '';
    step = mode === 'forced' ? 'warning' : 'confirm';
  }

  async function createTarget(event: SubmitEvent) {
    event.preventDefault();
    if (busy || loading || loadError || mode !== 'forced') return;
    creatingTarget = true;
    error = '';
    try {
      const target = await forceTarget(targetName.trim(), targetPath.trim());
      if (destroyed) return;
      creatingTarget = false;
      start([target]);
    } catch (e) {
      if (!destroyed) error = errorText(e);
    } finally {
      if (!destroyed) creatingTarget = false;
    }
  }

  async function prepare(continueWithoutRestore = false) {
    if (busy || !targets.length || (mode === 'forced' && !forceAcknowledged)) return;
    if (continueWithoutRestore && (!restoreError || !continueConfirmed || step !== 'restore')) return;
    step = 'prepare';
    error = '';
    try {
      const preparation = await uninstallPrepare(targets.map((a) => a.id), mode, continueWithoutRestore);
      if (destroyed) return;
      if (!preparation.ready) {
        restoreError = preparation.restoreError;
        continueConfirmed = false;
        if (restoreError) step = 'restore';
        else { error = t('err.not_prepared'); step = 'confirm'; }
        return;
      }
      restoreError = null;
      continueConfirmed = false;
      if (mode === 'forced' && app) await scanLeftovers(app);
      else await uninstall();
    } catch (e) {
      if (destroyed) return;
      error = errorText(e);
      step = restoreError ? 'restore' : 'confirm';
    }
  }

  // Chờ sự kiện tiến độ xác nhận quyền sở hữu trước khi gửi cờ hủy toàn cục.
  async function cancelWait(session: WaitSession) {
    if (session !== waitSession || !session.active || session.finished || session.cancelSent) return;
    session.cancelSent = true;
    try {
      await cancel();
    } catch (e) {
      if (!destroyed && session === waitSession && !session.finished) {
        error = errorText(e);
        stopping = false;
        session.cancelSent = false;
      }
    }
  }

  async function uninstall() {
    if (destroyed || mode !== 'standard') return;
    step = 'running';
    stopping = false;
    const session: WaitSession = { active: false, finished: false, stopRequested: false, cancelSent: false };
    waitSession = session;
    try {
      const completed = await appsUninstall(targets.map((a) => a.id), (p) => {
        if (session.finished || session !== waitSession || p.total !== targets.length) return;
        const current = p.current;
        if (targets[current - 1]?.name !== p.name) return;
        session.active = true;
        if (!destroyed) progress = { ...p, current };
        if (destroyed || session.stopRequested) void cancelWait(session);
      });
      session.finished = true;
      if (destroyed) return;
      batch = completed;
      step = 'results';
      const single = completed.items[0];
      if (targets.length === 1 && single?.result && !single.error && !single.result.stillInstalled &&
          !completed.cancelled && !session.stopRequested) {
        result = single.result;
        await scanLeftovers(targets[0]);
      }
    } catch (e) {
      session.finished = true;
      if (destroyed) return;
      error = errorText(e);
      step = 'results';
    } finally {
      session.finished = true;
      if (waitSession === session) waitSession = null;
    }
  }

  function stopWaiting() {
    const session = waitSession;
    if (!session || session.finished) return;
    stopping = true;
    session.stopRequested = true;
    void cancelWait(session);
  }

  async function scanLeftovers(target: AppInfo) {
    if (destroyed || !target.removable) return;
    app = target;
    if (mode === 'standard') result = batch?.items.find((i) => i.id === target.id)?.result ?? null;
    step = 'scanning';
    error = '';
    leftovers = [];
    picked = {};
    report = null;
    scanSucceeded = false;
    try {
      const found = await leftoversScan(target.id, mode);
      if (destroyed) return;
      leftovers = found;
      picked = Object.fromEntries(found.map((l) => [l.id, mode === 'standard' && l.recommended && l.kind !== 'service' && l.kind !== 'task']));
      scanSucceeded = true;
      if (mode === 'standard') result = { exitCode: result?.exitCode ?? null, stillInstalled: false };
    } catch (e) {
      if (destroyed) return;
      error = errorText(e);
      if (String(e) === 'still_installed') result = { exitCode: result?.exitCode ?? null, stillInstalled: true };
    }
    if (!destroyed) step = 'leftovers';
  }

  async function remove() {
    if (busy || !app || !scanSucceeded || !chosen.length) return;
    step = 'removing';
    error = '';
    try {
      const removed = await leftoversRemove(app.id, chosen.map((l) => l.id), mode);
      if (destroyed) return;
      report = removed;
      step = 'done';
    } catch (e) {
      if (destroyed) return;
      error = errorText(e);
      if (String(e) === 'still_installed') {
        result = { exitCode: result?.exitCode ?? null, stillInstalled: true };
        leftovers = [];
        picked = {};
        scanSucceeded = false;
      }
      step = 'leftovers';
    }
  }

  function back() {
    if (busy) return;
    step = 'list';
    app = null;
    targets = [];
    selected = {};
    error = '';
    void load();
  }

  function backResults() {
    if (busy || mode !== 'standard' || !batch) return;
    step = 'results';
    error = '';
    app = null;
  }

  async function showLocation(path: string) {
    try { await reveal(path); }
    catch (e) { if (!destroyed) error = errorText(e); }
  }

  onDestroy(() => {
    destroyed = true;
    ++loadVersion;
    const session = waitSession;
    if (session && !session.finished) {
      session.stopRequested = true;
      void cancelWait(session);
    }
  });

  void load();
</script>

<div class="uninstaller">
  {#if !inTauri()}
    <p class="empty">{t('tool.needApp')}</p>
  {:else if step === 'list'}
    <div class="sorts modes" role="tablist" aria-label={t('uninstall.modeLabel')}>
      {#each ['standard', 'forced'] as const as m (m)}
        <button role="tab" aria-selected={mode === m} class:active={mode === m} disabled={busy} onclick={() => changeMode(m)}>
          {t(`uninstall.mode.${m}` as Key)}
        </button>
      {/each}
    </div>
    <div class="toolbar">
      <input class="search" type="search" aria-label={t('uninstall.search')} placeholder={t('uninstall.search')} bind:value={query} disabled={busy} />
      <select aria-label={t('uninstall.filterLabel')} bind:value={filter} disabled={busy}>
        {#each ['all', 'desktop', 'store', 'builtIn', 'large', 'stale', 'recent', 'companions'] as const as f (f)}
          <option value={f}>{t(`uninstall.filter.${f}` as Key)}</option>
        {/each}
      </select>
      <div class="sorts" role="tablist" aria-label={t('uninstall.sortLabel')}>
        {#each ['name', 'size', 'date', 'used'] as const as s (s)}
          <button role="tab" aria-selected={sort === s} class:active={sort === s} disabled={busy} onclick={() => (sort = s)}>
            {t(`uninstall.sort.${s}` as Key)}
          </button>
        {/each}
      </div>
    </div>
    {#if error}<p class="message bad" role="alert">{error}</p>{/if}
    {#if loading}
      <p class="empty">{t('uninstall.loading')}</p>
    {:else if loadError}
      <p class="message bad" role="alert">{loadError}</p>
      <div class="row"><button class="btn" disabled={busy} onclick={load}>{t('uninstall.reload')}</button></div>
    {:else}
      {#if mode === 'standard'}
        <div class="row selection">
          <label class="check-label"><input type="checkbox" checked={allSelected} disabled={busy || !selectable.length} onchange={selectShown} />{t('uninstall.selectShown')}</label>
          <button class="btn small primary" disabled={busy || !selectedApps.length} onclick={() => start(selectedApps)}>
            {t('uninstall.batchAction', { n: formatCount(selectedApps.length) })}
          </button>
          <button class="link" disabled={busy || !selectedApps.length} onclick={() => (selected = {})}>{t('uninstall.clearSelection')}</button>
        </div>
      {:else}
        <p class="message bad">{t('uninstall.forcedHint')}</p>
      {/if}
      <ul class="list">
        {#each shown as a (a.id)}
          <li>
            {#if mode === 'standard'}
              <input class="check" type="checkbox" aria-label={t('uninstall.selectApp', { name: a.name })} checked={!!selected[a.id]} disabled={busy || !a.removable} onchange={() => (selected[a.id] = !selected[a.id])} />
            {/if}
            <div class="text">
              <span class="name">{a.name}</span>
              <span class="detail">{[a.publisher, a.version, a.installDate, lastUsedText(a)].filter(Boolean).join(' · ')}</span>
              <span class="badges">
                <span class="badge">{t(`uninstall.type.${a.kind}` as Key)}</span>
                {#if a.builtIn}<span class="badge">{t('uninstall.builtIn')}</span>{/if}
                {#if !a.removable}<span class="badge protected">{t('uninstall.protected')}</span>{/if}
                {#if a.companions.length}<span class="badge protected" title={t('uninstall.companionsHint', { names: a.companions.join(', ') })}>{t('uninstall.companions')}</span>{/if}
              </span>
              {#if a.companions.length}<span class="detail bad">{t('uninstall.companionsWith', { names: a.companions.join(', ') })}</span>{/if}
            </div>
            <span class="amount">{a.size ? formatBytes(a.size) : ''}</span>
            <button class="btn small" disabled={busy || !a.removable || (mode === 'forced' && a.kind === 'store')} title={!a.removable ? t('uninstall.protectedHint') : undefined} onclick={() => start([a])}>
              {t(mode === 'standard' ? 'uninstall.action' : 'uninstall.forceAction')}
            </button>
          </li>
        {:else}
          <li class="none">{t('uninstall.none')}</li>
        {/each}
      </ul>
      <p class="note">{t('uninstall.count', { n: formatCount(apps.length) })} · {t('uninstall.capabilityNote')}</p>
      {#if sort === 'used' || filter === 'stale'}<p class="note">{t('uninstall.usageNote')}</p>{/if}
      {#if mode === 'forced'}
        <form class="force-form" onsubmit={createTarget}>
          <h3>{t('uninstall.unlisted')}</h3>
          <p class="note">{t('uninstall.unlistedHint')}</p>
          <div class="toolbar">
            <label>{t('uninstall.targetName')}<input class="search" required bind:value={targetName} disabled={busy} /></label>
            <label>{t('uninstall.targetPath')}<input class="search" required bind:value={targetPath} disabled={busy} placeholder="C:\Programs\OldApp" /></label>
            <button class="btn small" type="submit" disabled={busy || !targetName.trim() || !targetPath.trim()}>{t(creatingTarget ? 'uninstall.working' : 'uninstall.forceAction')}</button>
          </div>
        </form>
      {/if}
    {/if}
  {:else}
    <div class="panel">
      {#if !busy}
        <div class="row">
          <button class="link back" onclick={back}><Icon name="back" size={14} /> {t('uninstall.back')}</button>
          {#if mode === 'standard' && batch && step !== 'results'}
            <button class="link" onclick={backResults}>{t('uninstall.backResults')}</button>
          {/if}
        </div>
      {/if}
      <h2>{app?.name ?? t('uninstall.batchTitle', { n: formatCount(targets.length) })}</h2>
      {#if app}<p class="sub">{[app.publisher, app.version, app.installLocation].filter(Boolean).join(' · ')}</p>{/if}
      {#if error}<p class="message bad" role="alert">{error}</p>{/if}

      {#if step === 'warning'}
        <p class="message bad">{t('uninstall.forceWarning')}</p>
        <label class="check-label"><input type="checkbox" bind:checked={forceAcknowledged} />{t('uninstall.forceAcknowledge')}</label>
        <div class="row">
          <button class="btn" onclick={back}>{t('uninstall.cancel')}</button>
          <button class="btn primary" disabled={!forceAcknowledged} onclick={() => (step = 'confirm')}>{t('uninstall.forceReview')}</button>
        </div>
      {:else if step === 'confirm'}
        <p class="desc">{t(mode === 'forced' ? 'uninstall.forceConfirm' : 'uninstall.confirm')}</p>
        {#if app?.companions.length}<p class="note">{t('uninstall.companionsHint', { names: app.companions.join(', ') })}</p>{/if}
        {#if targets.length > 1}
          <ul class="list queue">{#each targets as a (a.id)}<li><span class="name">{a.name}</span></li>{/each}</ul>
          <p class="note">{t('uninstall.batchHint')}</p>
        {/if}
        <p class="note">{t('uninstall.restorePreflight')}</p>
        <div class="row">
          <button class="btn" onclick={back}>{t('uninstall.cancel')}</button>
          <button class="btn primary" onclick={() => prepare()}>{t(mode === 'forced' ? 'uninstall.forceRun' : 'uninstall.run')}</button>
        </div>
      {:else if step === 'prepare'}
        <p class="desc" role="status">{t('uninstall.preparing')}</p>
      {:else if step === 'restore'}
        <p class="message bad" role="alert">{t('uninstall.restoreFailed', { error: restoreErrorText(restoreError ?? '') })}</p>
        <label class="check-label"><input type="checkbox" bind:checked={continueConfirmed} />{t('uninstall.continueAcknowledge')}</label>
        <div class="row">
          <button class="btn" onclick={back}>{t('uninstall.cancel')}</button>
          <button class="btn" onclick={() => prepare()}>{t('uninstall.retryRestore')}</button>
          <button class="btn primary" disabled={!continueConfirmed} onclick={() => prepare(true)}>{t('uninstall.continueWithoutRestore')}</button>
        </div>
      {:else if step === 'running'}
        <p class="desc" role="status">{progress ? t('uninstall.progress', { current: formatCount(progress.current), total: formatCount(progress.total), name: progress.name }) : t('uninstall.running')}</p>
        {#if progress}<progress value={progress.current - 1} max={progress.total} aria-label={t('uninstall.batchProgress')}></progress>{/if}
        <p class="note">{t('uninstall.stopWaitingHint')}</p>
        <div class="row"><button class="btn" disabled={stopping} onclick={stopWaiting}>{t(stopping ? 'scan.stopping' : 'uninstall.stopWaiting')}</button></div>
      {:else if step === 'results'}
        {#if batch}
          {#if batch.cancelled}<p class="message bad">{t('uninstall.batchCancelled')}</p>{/if}
          <p class="note">{t('uninstall.resultsHint')}</p>
          <ul class="list results">
            {#each batch.items as item (item.id)}
              <li>
                <div class="text">
                  <span class="name">{item.name}</span>
                  <span class="detail" class:bad={!!item.error || !!item.result?.stillInstalled}>
                    {item.error ? errorText(item.error) : item.result ? t(item.result.stillInstalled ? 'uninstall.resultInstalled' : 'uninstall.resultRemoved') : t('uninstall.resultUnknown')}
                    {#if item.result?.exitCode != null} · {t('uninstall.exitCode', { code: String(item.result.exitCode) })}{/if}
                  </span>
                </div>
                {#if targetFor(item.id)}<button class="btn small" onclick={() => scanLeftovers(targetFor(item.id)!)}>{t('uninstall.scan')}</button>{/if}
              </li>
            {/each}
            {#each batch.pending as id (id)}
              <li><div class="text"><span class="name">{targetFor(id)?.name ?? id}</span><span class="detail">{t('uninstall.pending')}</span></div></li>
            {/each}
          </ul>
        {:else}
          <p class="message bad">{t('uninstall.resultsUnknown')}</p>
        {/if}
        <div class="row"><button class="btn primary" onclick={back}>{t('uninstall.back')}</button></div>
      {:else if step === 'scanning' || step === 'removing'}
        <p class="desc" role="status">{t(step === 'scanning' ? 'uninstall.scanning' : 'uninstall.removing')}</p>
      {:else if step === 'leftovers' && app}
        {#if mode === 'forced'}<p class="message bad">{t('uninstall.forcedPreview')}</p>{/if}
        {#if mode === 'standard' && result?.stillInstalled}<p class="message bad">{t('uninstall.stillInstalled')}</p>{/if}
        {#if !scanSucceeded}
          <div class="row"><button class="btn primary" onclick={() => scanLeftovers(app!)}>{t('uninstall.scan')}</button></div>
        {:else if leftovers.length === 0}
          <p class="message">{t('uninstall.clean')}</p>
          <div class="row"><button class="btn primary" onclick={back}>{t('uninstall.finish')}</button></div>
        {:else}
          <p class="desc">{t('uninstall.leftoversHint')}</p>
          <ul class="list leftovers">
            {#each leftovers as l (l.id)}
              <li>
                <input class="check" type="checkbox" aria-label={t('uninstall.selectLeftover', { kind: t(`uninstall.kind.${l.kind}` as Key), path: l.path })} checked={!!picked[l.id]} onchange={() => (picked[l.id] = !picked[l.id])} />
                <div class="text">
                  <span class="name" title={l.path}>{l.path}</span>
                  <span class="detail">{t(`uninstall.kind.${l.kind}` as Key)}{mode === 'forced' || !l.recommended || l.kind === 'service' || l.kind === 'task' ? ` · ${t('uninstall.notPreselected')}` : ''}</span>
                </div>
                {#if l.kind === 'folder' || l.kind === 'shortcut'}<button class="link" onclick={() => showLocation(l.path)}>{t('tool.reveal')}</button>{/if}
                <span class="amount">{l.bytes > 0 ? formatBytes(l.bytes) : ''}</span>
              </li>
            {/each}
          </ul>
          <p class="note">{t('uninstall.serviceTaskWarning')}</p>
          <p class="note">{t('uninstall.removeNote')}</p>
          <div class="row">
            <button class="btn" onclick={mode === 'standard' && batch ? backResults : back}>{t('uninstall.skip')}</button>
            <button class="btn primary" disabled={chosen.length === 0} onclick={remove}>{t('uninstall.remove', { n: formatCount(chosen.length) })}</button>
          </div>
        {/if}
      {:else if step === 'done' && report && app}
        <p class="message" class:bad={report.failed > 0}>
          {t('uninstall.removed', { n: formatCount(report.done), size: formatBytes(report.bytes) })}
          {#if report.failed > 0}{t('uninstall.removeFailed', { n: formatCount(report.failed) })}{/if}
        </p>
        <div class="row">
          <button class="btn primary" onclick={back}>{t('uninstall.finish')}</button>
          <button class="btn" onclick={() => scanLeftovers(app!)}>{t('uninstall.scan')}</button>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .uninstaller, .panel { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 10px; }
  .empty { flex: 1; display: grid; place-items: center; margin: 0; color: var(--text-faint); }
  .toolbar, .row { flex: none; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .search, select { min-width: 0; height: 34px; padding: 0 12px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); color: var(--text); font: inherit; font-size: 13px; }
  .toolbar > .search { flex: 1; }
  .search:focus, select:focus { outline: 2px solid var(--accent); outline-offset: -1px; }
  .sorts { display: flex; padding: 2px; border-radius: 999px; border: 1px solid var(--border); }
  .modes { align-self: flex-start; flex: none; }
  .sorts button { height: 28px; padding: 0 12px; border-radius: 999px; font-size: 12px; color: var(--text-dim); }
  .sorts button.active { background: var(--surface-active); color: var(--text); }
  .list { flex: 1; min-height: 0; overflow-y: auto; margin: 0; padding: 0 6px 0 0; list-style: none; display: flex; flex-direction: column; gap: 4px; }
  .list li { flex: none; display: flex; align-items: center; gap: 12px; min-height: 46px; padding: 6px 12px; border-radius: var(--radius); background: var(--surface); }
  .list li.none { justify-content: center; color: var(--text-faint); font-size: 13px; }
  .text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .name, .detail { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .name { font-size: 13px; }
  .detail { font-size: 12px; color: var(--text-faint); }
  .detail.bad { color: var(--warn); }
  .badges { display: flex; flex-wrap: wrap; gap: 4px; }
  .badge { padding: 1px 6px; border-radius: 4px; background: var(--surface-active); font-size: 10px; color: var(--text-dim); }
  .protected { color: var(--warn); }
  .amount { flex: none; min-width: 64px; text-align: right; font-size: 13px; color: var(--text-dim); font-variant-numeric: tabular-nums; }
  .btn.small { flex: none; height: 30px; padding: 0 14px; }
  .note { flex: none; margin: 0; font-size: 12px; line-height: 1.45; color: var(--text-faint); }
  .back { align-self: flex-start; display: flex; align-items: center; gap: 4px; }
  h2 { margin: 0; font-size: 18px; font-weight: 600; }
  h3 { margin: 0; font-size: 13px; font-weight: 600; }
  .sub, .desc { margin: 0; font-size: 13px; line-height: 1.5; color: var(--text-dim); overflow-wrap: anywhere; }
  .message { flex: none; margin: 0; padding: 8px 12px; border-radius: var(--radius); background: rgba(70, 201, 139, 0.12); font-size: 13px; line-height: 1.5; }
  .message.bad { background: rgba(240, 179, 69, 0.12); color: var(--warn); }
  .link { flex: none; font-size: 12px; color: var(--accent-2); }
  .link:hover { text-decoration: underline; }
  .check { flex: none; }
  input[type='checkbox'] { width: 16px; height: 16px; margin: 0; accent-color: var(--accent); }
  .check-label { display: flex; gap: 8px; align-items: center; font-size: 13px; line-height: 1.5; }
  .check-label input { flex: none; }
  .force-form { flex: none; display: flex; flex-direction: column; gap: 6px; padding: 10px; border: 1px solid var(--border); border-radius: var(--radius); }
  .force-form label { flex: 1; min-width: 150px; display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--text-dim); }
  .force-form .btn { align-self: flex-end; }
  progress { width: 100%; height: 8px; accent-color: var(--accent); }
  button:disabled, input:disabled { opacity: 0.5; }
</style>
