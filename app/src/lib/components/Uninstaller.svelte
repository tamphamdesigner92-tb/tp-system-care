<script lang="ts">
  import { onDestroy } from 'svelte';
  import { cancel } from '../scan';
  import Icon, { type IconName } from './Icon.svelte';
  import { formatBytes, formatCount, formatTime } from '../format';
  import { APP_SIGNALS, appSignals, filterApps, knownUsage, type AppSort, type AppFilter, type AppSignal } from '../appFilters';
  import { t, tOr, type Key } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import {
    appsList,
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
  type Sort = AppSort;
  type Filter = AppFilter;
  type WaitSession = { active: boolean; finished: boolean; stopRequested: boolean; cancelSent: boolean };
  const categories: { id: 'all' | 'recent' | 'large' | 'unused' | 'peers' | 'store' | 'builtIn'; icon: IconName; filter: Filter; signal?: AppSignal }[] = [
    { id: 'all', icon: 'apps', filter: 'all' },
    { id: 'recent', icon: 'calendar', filter: 'all', signal: 'recent' },
    { id: 'large', icon: 'drive', filter: 'all', signal: 'large' },
    { id: 'unused', icon: 'clock', filter: 'all', signal: 'unused' },
    { id: 'peers', icon: 'package', filter: 'all', signal: 'peers' },
    { id: 'store', icon: 'toolbox', filter: 'store' },
    { id: 'builtIn', icon: 'protect', filter: 'builtIn' },
  ];

  let apps = $state<AppInfo[]>([]);
  let loading = $state(true);
  let loadError = $state('');
  let query = $state('');
  let sort = $state<Sort>('name');
  let direction = $state<'asc' | 'desc'>('asc');
  let filtersOpen = $state(false);
  let expanded = $state<string | null>(null);
  let forceFormOpen = $state(false);
  let filter = $state<Filter>('all');
  let advanced = $state<Partial<Record<AppSignal, boolean>>>({});
  let observedAt = $state(Math.floor(Date.now() / 1000));
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
  const shown = $derived(filterApps(apps, query, filter, advanced, sort, observedAt, direction));
  const activeCategory = $derived(categories.find((category) => category.filter === filter && APP_SIGNALS.every((signal) => !!advanced[signal] === (category.signal === signal))));
  const categoryCounts = $derived(new Map(categories.map((category) => [category.id, filterApps(apps, '', category.filter, category.signal ? { [category.signal]: true } : {}, 'name', observedAt).length])));
  const filterCount = $derived(APP_SIGNALS.filter((signal) => advanced[signal]).length + Number(filter !== 'all'));
  const inventoryBytes = $derived(apps.reduce((sum, item) => sum + (item.size ?? 0), 0));
  const appNames = $derived(new Map(apps.map((item) => [item.id, item.name])));
  const selectable = $derived(shown.filter((a) => a.removable));
  const allSelected = $derived(selectable.length > 0 && selectable.every((a) => selected[a.id]));
  const selectedApps = $derived(apps.filter((a) => a.removable && selected[a.id]));
  const selectedBytes = $derived(selectedApps.reduce((sum, item) => sum + (item.size ?? 0), 0));
  const hiddenSelected = $derived(selectedApps.filter((item) => !shown.some((visible) => visible.id === item.id)).length);
  const chosen = $derived(leftovers.filter((l) => picked[l.id]));

  const errorText = (e: unknown) => tOr(`err.${String(e)}`, String(e));
  const restoreErrorText = (e: string) => tOr(`uninstall.restoreError.${e}`, errorText(e));
  const targetFor = (id: string) => targets.find((a) => a.id === id);

  async function load() {
    if (!inTauri()) { loading = false; return; }
    const version = ++loadVersion;
    loading = true;
    loadError = '';
    try {
      const list = await appsList();
      if (destroyed || version !== loadVersion) return;
      apps = list;
      observedAt = Math.floor(Date.now() / 1000);
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
    expanded = null;
    forceFormOpen = false;
    error = '';
  }

  function chooseCategory(category: typeof categories[number]) {
    filter = category.filter;
    advanced = category.signal ? { [category.signal]: true } : {};
    expanded = null;
  }

  function resetFilters() {
    query = '';
    filter = 'all';
    advanced = {};
    expanded = null;
  }

  function changeSort(next: Sort) {
    direction = sort === next ? (direction === 'asc' ? 'desc' : 'asc') : next === 'size' || next === 'date' ? 'desc' : 'asc';
    sort = next;
  }

  function initials(name: string) {
    return name.trim().split(/\s+/).slice(0, 2).map((word) => Array.from(word)[0]).join('').toUpperCase();
  }

  function tileHue(name: string) {
    return [211, 164, 265, 32, 345][Array.from(name).reduce((sum, char) => sum + char.codePointAt(0)!, 0) % 5];
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
    <div class="library-layout">
      <aside class="library-nav">
        <div class="library-label"><Icon name="apps" size={16} /><span>{t('uninstall.library')}</span></div>
        <nav aria-label={t('uninstall.categories')}>
          {#each categories as category (category.id)}
            <button class="category" class:current={activeCategory?.id === category.id} aria-current={activeCategory?.id === category.id ? 'true' : undefined} disabled={busy || loading} onclick={() => chooseCategory(category)}>
              <Icon name={category.icon} size={17} />
              <span>{t(`uninstall.category.${category.id}`)}</span>
              <span class="category-count">{loading || loadError ? '—' : formatCount(categoryCounts.get(category.id) ?? 0)}</span>
            </button>
          {/each}
        </nav>
        <div class="inventory-summary">
          <span class="disk-symbol"><Icon name="drive" size={20} /></span>
          <div><strong>{loading || loadError ? '—' : formatBytes(inventoryBytes)}</strong><span>{t('uninstall.inventorySize')}</span></div>
        </div>
        <p class="library-note"><Icon name="protect" size={15} /><span>{t('uninstall.safeFlow')}</span></p>
      </aside>

      <div class="library-main">
        <div class="library-heading">
          <div class="heading-copy">
            <h2>{t(activeCategory ? `uninstall.category.${activeCategory.id}` : 'uninstall.filters')}<span class="heading-count">{loading || loadError ? '—' : formatCount(shown.length)}</span></h2>
            <p>{t('uninstall.manageHint')}</p>
          </div>
          {#if mode === 'standard'}
            <button class="btn primary uninstall-primary" disabled={busy || loading || !!loadError || !selectedApps.length} onclick={() => start(selectedApps)}>
              <Icon name="trash" size={16} />{t('uninstall.batchAction', { n: formatCount(selectedApps.length) })}
            </button>
          {:else}
            <span class="forced-label"><Icon name="info" size={15} />{t('uninstall.mode.forced')}</span>
          {/if}
        </div>

        <div class="library-toolbar">
          <label class="search-box"><Icon name="search" size={17} /><input type="search" aria-label={t('uninstall.search')} placeholder={t('uninstall.search')} bind:value={query} disabled={busy || loading} /></label>
          <button class="filter-button" class:active={filtersOpen || filterCount > 0} aria-expanded={filtersOpen} aria-controls="uninstall-filters" disabled={busy || loading} onclick={() => (filtersOpen = !filtersOpen)}><Icon name="filter" size={16} />{t('uninstall.filters')}{#if filterCount}<span>{filterCount}</span>{/if}</button>
          <button class="icon-button reload" aria-label={t('uninstall.reload')} title={t('uninstall.reload')} disabled={busy || loading} onclick={load}><Icon name="refresh" size={17} /></button>
          <div class="mode-switch" role="group" aria-label={t('uninstall.modeLabel')}>
            {#each ['standard', 'forced'] as const as m (m)}
              <button aria-pressed={mode === m} class:active={mode === m} disabled={busy || loading} onclick={() => changeMode(m)}>{t(`uninstall.mode.${m}`)}</button>
            {/each}
          </div>
        </div>

        {#if filtersOpen}
          <div id="uninstall-filters" class="filter-panel">
            <div class="filter-selects">
              <label>{t('uninstall.filterLabel')}<select bind:value={filter} disabled={busy}>{#each ['all', 'desktop', 'store', 'builtIn'] as const as f (f)}<option value={f}>{t(`uninstall.filter.${f}`)}</option>{/each}</select></label>
              <label>{t('uninstall.sortLabel')}<select value={sort} onchange={(event) => changeSort(event.currentTarget.value as Sort)} disabled={busy}>{#each ['name', 'size', 'date', 'lastUsed'] as const as s (s)}<option value={s}>{t(`uninstall.sort.${s}`)}</option>{/each}</select></label>
            </div>
            <fieldset class="advanced" disabled={busy}>
              <legend>{t('uninstall.advanced')}</legend>
              {#each APP_SIGNALS as signal (signal)}<label class="check-label"><input type="checkbox" checked={!!advanced[signal]} onchange={() => (advanced[signal] = !advanced[signal])} />{t(`uninstall.signal.${signal}`)}</label>{/each}
            </fieldset>
            <button class="link" onclick={resetFilters}>{t('uninstall.clearFilters')}</button>
          </div>
        {/if}
        {#if filterCount > 0 && !filtersOpen}<p class="signal-note"><Icon name="info" size={15} /><span>{t('uninstall.signalNote')}</span></p>{/if}
        {#if error}<p class="message bad" role="alert">{error}</p>{/if}
        {#if mode === 'forced'}
          <div class="forced-intro">
            <p><Icon name="info" size={16} /><span>{t('uninstall.forcedHint')}</span></p>
            <button class="link" aria-expanded={forceFormOpen} aria-controls="force-target-form" disabled={busy || loading || !!loadError} onclick={() => (forceFormOpen = !forceFormOpen)}>{t('uninstall.forceEntry')}<span class:rotated={forceFormOpen}><Icon name="chevronDown" size={14} /></span></button>
          </div>
          {#if forceFormOpen}
            <form id="force-target-form" class="force-form" onsubmit={createTarget}>
              <p class="note">{t('uninstall.unlistedHint')}</p>
              <div class="toolbar">
                <label>{t('uninstall.targetName')}<input class="search" required bind:value={targetName} disabled={busy} /></label>
                <label>{t('uninstall.targetPath')}<input class="search" required bind:value={targetPath} disabled={busy} placeholder="C:\Programs\OldApp" /></label>
                <button class="btn small" type="submit" disabled={busy || loading || !!loadError || !targetName.trim() || !targetPath.trim()}>{t(creatingTarget ? 'uninstall.working' : 'uninstall.forceAction')}</button>
              </div>
            </form>
          {/if}
        {/if}

        <div class="table-scroll" aria-busy={loading}>
          <table class="app-table" aria-label={t('toolbox.uninstaller')}>
            <colgroup><col class="check-col" /><col /><col class="size-col" /><col class="date-col" /><col class="action-col" /></colgroup>
            <thead><tr>
              <th scope="col" class="checkbox-cell">{#if mode === 'standard'}<input type="checkbox" aria-label={t('uninstall.selectShown')} checked={allSelected} indeterminate={!allSelected && selectable.some((item) => selected[item.id])} disabled={busy || loading || !!loadError || !selectable.length} onchange={selectShown} />{/if}</th>
              {#each ['name', 'size', 'date'] as const as column (column)}
                <th scope="col" aria-sort={sort === column ? direction === 'asc' ? 'ascending' : 'descending' : 'none'}>
                  <button class="column-sort" class:sorted={sort === column} disabled={busy || loading} onclick={() => changeSort(column)}>{t(`uninstall.sort.${column}`)}<span class:ascending={sort === column && direction === 'asc'} class:muted={sort !== column}><Icon name="chevronDown" size={12} /></span></button>
                </th>
              {/each}
              <th scope="col" class="action-heading"><span class="sr-only">{t('uninstall.actionColumn')}</span><Icon name="trash" size={15} /></th>
            </tr></thead>
            <tbody>
              {#if loading}
                <tr><td colspan="5" class="table-state"><span class="loading-spinner"></span><p role="status">{t('uninstall.loading')}</p></td></tr>
              {:else if loadError}
                <tr><td colspan="5" class="table-state"><span class="state-symbol warning"><Icon name="info" size={28} /></span><p role="alert">{loadError}</p><button class="btn" disabled={busy} onclick={load}>{t('uninstall.reload')}</button></td></tr>
              {:else}
                {#each shown as a (a.id)}
                  <tr class="app-row" class:picked={!!selected[a.id] && mode === 'standard'} class:expanded={expanded === a.id}>
                    <td class="checkbox-cell">{#if mode === 'standard'}<input type="checkbox" aria-label={t('uninstall.selectApp', { name: a.name })} checked={!!selected[a.id]} disabled={busy || !a.removable} onchange={() => (selected[a.id] = !selected[a.id])} />{/if}</td>
                    <td class="app-name-cell">
                      <button class="app-identity" aria-label={t('uninstall.showDetails', { name: a.name })} aria-expanded={expanded === a.id} disabled={busy} onclick={() => (expanded = expanded === a.id ? null : a.id)}>
                        <span class="app-tile" style={`--tile-hue:${tileHue(a.name)}`} aria-hidden="true">{initials(a.name)}</span>
                        <span class="app-copy"><span class="app-title" title={a.name}>{a.name}</span><span class="app-publisher" title={a.publisher ?? ''}>{a.publisher ?? t(`uninstall.type.${a.kind}`)}</span></span>
                        {#if !a.removable}<span class="protected-icon" title={t('uninstall.protectedHint')} aria-label={t('uninstall.protected')}><Icon name="protect" size={14} /></span>{/if}
                        <span class="row-chevron" class:rotated={expanded === a.id}><Icon name="chevronDown" size={13} /></span>
                      </button>
                    </td>
                    <td class="size-cell">{a.size !== null ? formatBytes(a.size) : '—'}</td>
                    <td class="date-cell">{a.installDate ?? '—'}</td>
                    <td class="action-cell"><button class="row-uninstall" aria-label={`${t(mode === 'standard' ? 'uninstall.action' : 'uninstall.forceAction')} ${a.name}`} title={!a.removable ? t('uninstall.protectedHint') : t(mode === 'standard' ? 'uninstall.action' : 'uninstall.forceAction')} disabled={busy || !a.removable || (mode === 'forced' && a.kind === 'store')} onclick={() => start([a])}><Icon name="trash" size={18} /></button></td>
                  </tr>
                  {#if expanded === a.id}
                    <tr class="app-details"><td colspan="5">
                      <div class="details-body">
                        <div class="detail-top"><span class="badge">{t(`uninstall.type.${a.kind}`)}</span>{#if a.builtIn}<span class="badge">{t('uninstall.builtIn')}</span>{/if}{#if !a.removable}<span class="badge protected">{t('uninstall.protected')}</span>{/if}<span class="app-version">{a.version ?? '—'}</span></div>
                        <p>{t('uninstall.lastObserved', { time: knownUsage(a.lastUsed) ? formatTime(a.lastUsed) : t('maintenance.unknown') })}</p>
                        {#if a.installLocation}<div class="detail-location"><span>{a.installLocation}</span><button class="link" disabled={busy} onclick={() => showLocation(a.installLocation!)}><Icon name="back" size={13} />{t('tool.reveal')}</button></div>{/if}
                        {#if a.installedTogether.some((id) => id !== a.id)}<p>{t('uninstall.peerNames', { names: a.installedTogether.filter((id) => id !== a.id).map((id) => appNames.get(id) ?? id).join(', ') })}</p>{/if}
                        <div class="badges">{#each appSignals(a, observedAt) as signal (signal)}<span class="badge">{t(`uninstall.signal.${signal}`)}</span>{/each}</div>
                        <p class="details-note">{t('uninstall.signalNote')}</p>
                      </div>
                    </td></tr>
                  {/if}
                {:else}
                  <tr><td colspan="5" class="table-state"><span class="state-symbol"><Icon name="search" size={28} /></span><strong>{t('uninstall.noMatches')}</strong><p>{t(apps.length ? 'uninstall.noMatchesHint' : 'uninstall.noAppsHint')}</p>{#if apps.length}<button class="btn" onclick={resetFilters}>{t('uninstall.clearFilters')}</button>{:else}<button class="btn" onclick={load}>{t('uninstall.reload')}</button>{/if}</td></tr>
                {/each}
              {/if}
            </tbody>
          </table>
        </div>

        <footer class="library-footer">
          <span>{t('uninstall.visibleCount', { shown: loading || loadError ? '—' : formatCount(shown.length), total: loading || loadError ? '—' : formatCount(apps.length) })}</span>
          {#if selectedApps.length && mode === 'standard'}
            <span class="selected-summary">{t('uninstall.selectedSummary', { n: formatCount(selectedApps.length), size: formatBytes(selectedBytes) })}</span>
            <button class="link" disabled={busy} onclick={() => (selected = {})}>{t('uninstall.clearSelection')}</button>
          {/if}
        </footer>
        {#if hiddenSelected && mode === 'standard'}<p class="hidden-selection" role="status"><Icon name="info" size={14} />{t('uninstall.hiddenSelected', { n: formatCount(hiddenSelected) })}</p>{/if}
        <details class="safety-details"><summary><Icon name="protect" size={14} />{t('uninstall.safeFlow')}<Icon name="chevronDown" size={12} /></summary><p>{t('uninstall.capabilityNote')}</p><p>{t('uninstall.signalNote')}</p></details>
      </div>
    </div>
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
  .uninstaller, .panel { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; gap: 10px; }
  .empty { flex: 1; display: grid; place-items: center; margin: 0; color: var(--text-dim); }
  .library-layout { flex: 1; min-height: 0; display: grid; grid-template-columns: 174px minmax(0, 1fr); border: 1px solid var(--border); border-radius: 12px; overflow: hidden; background: rgba(12, 17, 33, .4); }
  .library-nav { display: flex; flex-direction: column; min-height: 0; padding: 22px 10px 16px; border-right: 1px solid var(--border); background: rgba(6, 10, 22, .28); overflow-y: auto; }
  .library-label { display: flex; align-items: center; gap: 9px; padding: 0 10px 19px; font-size: 10px; font-weight: 600; letter-spacing: 1.4px; color: var(--text-dim); }
  .library-nav nav { display: flex; flex-direction: column; gap: 5px; }
  .category { display: flex; align-items: center; gap: 9px; min-height: 39px; padding: 9px 10px; border-radius: 6px; color: var(--text-dim); font-size: 12px; text-align: left; transition: background .15s, color .15s; }
  .category > span:first-of-type { flex: 1; }
  .category:hover { background: var(--surface); color: var(--text); }
  .category.current { background: rgba(91, 157, 255, .12); color: var(--accent-2); box-shadow: inset 2px 0 var(--accent); }
  .category-count { margin-left: auto; font-size: 10px; font-variant-numeric: tabular-nums; color: var(--text-faint); }
  .category.current .category-count { color: var(--accent-2); }
  .inventory-summary { display: flex; gap: 10px; align-items: center; margin: auto 8px 0; padding-top: 24px; }
  .disk-symbol { display: grid; place-items: center; width: 34px; height: 38px; color: var(--accent-2); background: var(--surface); border-radius: 8px; }
  .inventory-summary div { min-width: 0; display: flex; flex-direction: column; gap: 4px; }
  .inventory-summary strong { font-size: 18px; font-weight: 600; letter-spacing: -.3px; }
  .inventory-summary div > span { font-size: 10px; line-height: 1.5; color: var(--text-faint); }
  .library-note { display: flex; gap: 7px; padding: 14px 8px 0; margin: 16px 0 0; border-top: 1px solid var(--border); color: var(--text-faint); font-size: 10px; line-height: 1.7; }
  .library-note > span { flex: 1; }
  .library-main { display: flex; flex-direction: column; min-width: 0; min-height: 0; gap: 12px; padding: 22px 20px 12px; overflow-y: auto; }
  .library-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex: none; }
  .heading-copy { min-width: 0; }
  .heading-copy h2 { display: flex; align-items: center; gap: 9px; font-size: 20px; letter-spacing: -.4px; }
  .heading-count { padding: 3px 7px; border: 1px solid var(--border); border-radius: 5px; font-size: 11px; font-weight: 400; color: var(--text-dim); letter-spacing: 0; }
  .heading-copy p { margin: 7px 0 0; font-size: 11px; color: var(--text-dim); line-height: 1.5; }
  .uninstall-primary { display: flex; align-items: center; gap: 7px; height: 34px; border-radius: 7px; padding: 0 12px; font-size: 12px; }
  .library-toolbar { flex: none; display: flex; align-items: center; flex-wrap: wrap; gap: 7px; margin-top: 6px; }
  .search-box { display: flex; align-items: center; gap: 8px; flex: 1; min-width: 140px; height: 34px; padding: 0 10px; background: rgba(4, 8, 18, .36); border: 1px solid var(--border); border-radius: 7px; color: var(--text-faint); }
  .search-box:focus-within { border-color: var(--accent); }
  .search-box input { width: 100%; min-width: 0; border: none; outline: none; font: inherit; font-size: 12px; background: transparent; color: var(--text); }
  .search-box input::placeholder { color: var(--text-faint); }
  .filter-button, .icon-button { height: 34px; display: flex; align-items: center; justify-content: center; gap: 6px; border: 1px solid var(--border); border-radius: 7px; color: var(--text-dim); font-size: 12px; }
  .filter-button { padding: 0 10px; }
  .filter-button > span { display: grid; place-items: center; font-size: 10px; min-width: 16px; height: 16px; border-radius: 4px; background: rgba(91, 157, 255, .18); }
  .filter-button.active { color: var(--accent-2); border-color: rgba(91, 157, 255, .35); }
  .icon-button { width: 34px; flex: none; }
  .icon-button:hover, .filter-button:hover { background: var(--surface); }
  .mode-switch { display: flex; align-items: center; gap: 2px; padding: 3px; border: 1px solid var(--border); border-radius: 7px; margin-left: auto; }
  .mode-switch button { min-height: 26px; padding: 0 8px; border-radius: 4px; font-size: 11px; color: var(--text-dim); white-space: nowrap; }
  .mode-switch button.active { background: var(--surface-active); color: var(--text); }
  .filter-panel { flex: none; display: flex; flex-direction: column; align-items: flex-start; gap: 12px; padding: 14px; border: 1px solid var(--border); border-radius: 8px; background: var(--surface); }
  .filter-selects { display: flex; flex-wrap: wrap; gap: 12px; width: 100%; }
  .filter-selects label { flex: 1; min-width: 140px; display: flex; flex-direction: column; gap: 6px; font-size: 11px; color: var(--text-dim); }
  .advanced { display: flex; flex-wrap: wrap; gap: 9px 14px; border: none; padding: 0; margin: 0; }
  .advanced legend { margin-bottom: 10px; font-size: 11px; color: var(--text-dim); }
  .advanced .check-label { font-size: 11px; }
  .signal-note { display: flex; align-items: flex-start; gap: 7px; flex: none; margin: 0; font-size: 10px; line-height: 1.6; color: var(--text-dim); }
  .signal-note > span { flex: 1; }
  .table-scroll { flex: 1; min-height: 130px; overflow: auto; border: 1px solid var(--border); border-radius: 8px; background: rgba(255, 255, 255, .018); scrollbar-gutter: stable; }
  .app-table { width: 100%; border-collapse: separate; border-spacing: 0; table-layout: fixed; text-align: left; }
  .check-col { width: 34px; } .size-col { width: 80px; } .date-col { width: 98px; } .action-col { width: 42px; }
  .app-table th { position: sticky; top: 0; z-index: 1; height: 37px; padding: 0 8px; background: #1c2339; border-bottom: 1px solid var(--border); font-weight: 400; }
  .column-sort { display: flex; align-items: center; gap: 5px; font-size: 11px; color: var(--text-dim); white-space: nowrap; }
  .column-sort.sorted { color: var(--accent-2); }
  .column-sort > span { display: flex; } .column-sort .muted { opacity: .25; } .column-sort .ascending { transform: rotate(180deg); }
  .app-table td { padding: 4px 8px; border-bottom: 1px solid rgba(255, 255, 255, .04); }
  .app-table .checkbox-cell { text-align: center; padding: 0 6px 0 10px; }
  .checkbox-cell input { vertical-align: middle; }
  .app-row { transition: background .12s; }
  .app-row:hover { background: rgba(255, 255, 255, .035); }
  .app-row.picked { background: rgba(91, 157, 255, .075); }
  .app-row.expanded { background: rgba(91, 157, 255, .05); }
  .app-identity { display: flex; align-items: center; gap: 10px; width: 100%; min-height: 45px; text-align: left; }
  .app-tile { flex: none; display: grid; place-items: center; width: 32px; height: 32px; border-radius: 8px; font-size: 12px; font-weight: 600; letter-spacing: -.5px; color: hsl(var(--tile-hue) 85% 78%); background: hsl(var(--tile-hue) 55% 55% / .16); border: 1px solid hsl(var(--tile-hue) 55% 65% / .12); }
  .app-copy { min-width: 0; flex: 1; display: flex; flex-direction: column; gap: 5px; }
  .app-title, .app-publisher { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .app-title { font-size: 13px; font-weight: 500; } .app-publisher { font-size: 11px; color: var(--text-faint); }
  .row-chevron { display: flex; flex: none; color: var(--text-faint); opacity: 0; transition: transform .15s, opacity .15s; }
  .app-identity:hover .row-chevron, .app-identity:focus-visible .row-chevron, .app-row.expanded .row-chevron { opacity: 1; }
  .rotated { display: inline-flex; transform: rotate(180deg); }
  .protected-icon { display: flex; color: var(--text-dim); }
  .size-cell, .date-cell { font-size: 11px; color: var(--text-dim); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .app-table .action-cell { padding: 4px; } .app-table .action-heading { color: var(--text-faint); padding-left: 11px; }
  .row-uninstall { display: grid; place-items: center; height: 30px; width: 30px; border-radius: 6px; color: var(--accent-2); }
  .row-uninstall:hover:not(:disabled) { color: #ffb5b8; background: rgba(229, 72, 77, .12); }
  .row-uninstall:disabled { color: var(--text-faint); opacity: .35; cursor: not-allowed; }
  .app-table .app-details td { padding: 0; background: rgba(91, 157, 255, .04); }
  .details-body { margin: 0 16px 12px 42px; border-left: 2px solid rgba(91, 157, 255, .3); padding: 12px 14px; display: flex; flex-direction: column; gap: 10px; }
  .details-body p { margin: 0; font-size: 11px; line-height: 1.6; color: var(--text-dim); overflow-wrap: anywhere; user-select: text; }
  .details-body p.details-note { font-size: 10px; color: var(--text-faint); }
  .detail-top { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .app-version { margin-left: auto; color: var(--text-dim); font-size: 11px; }
  .detail-location { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 12px; font-size: 11px; color: var(--text-dim); }
  .detail-location > span { overflow-wrap: anywhere; min-width: 0; user-select: text; } .detail-location button { display: inline-flex; align-items: center; gap: 4px; }
  .app-table .table-state { height: 220px; padding: 26px 18px; text-align: center; color: var(--text-dim); }
  .table-state p { font-size: 12px; line-height: 1.6; margin: 10px 0 16px; } .table-state strong { display: block; font-size: 14px; font-weight: 500; }
  .state-symbol { display: grid; place-items: center; width: 52px; height: 52px; margin: 0 auto 16px; border-radius: 14px; background: var(--surface); color: var(--text-faint); } .state-symbol.warning { color: var(--warn); }
  .loading-spinner { display: inline-block; width: 22px; height: 22px; border: 2px solid var(--border); border-top-color: var(--accent); border-radius: 50%; animation: spin .8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .library-footer { flex: none; display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; min-height: 22px; color: var(--text-faint); font-size: 10px; font-variant-numeric: tabular-nums; }
  .selected-summary { margin-left: auto; color: var(--accent-2); } .library-footer .link { font-size: 10px; }
  .hidden-selection { flex: none; margin: -4px 0 0; display: flex; align-items: center; gap: 6px; font-size: 10px; color: var(--warn); }
  .safety-details { flex: none; color: var(--text-faint); font-size: 10px; }
  .safety-details summary { display: flex; align-items: center; gap: 6px; cursor: pointer; list-style: none; }
  .safety-details summary::-webkit-details-marker { display: none; }
  .safety-details p { margin: 8px 0 0; font-size: 11px; line-height: 1.6; }
  .forced-label { display: flex; align-items: center; gap: 6px; color: var(--warn); font-size: 11px; }
  .forced-intro { flex: none; border: 1px solid rgba(240, 179, 69, .15); border-radius: 8px; padding: 10px 12px; background: rgba(240, 179, 69, .04); }
  .forced-intro p { display: flex; align-items: flex-start; gap: 7px; color: var(--warn); font-size: 11px; line-height: 1.6; margin: 0 0 7px; } .forced-intro p > span { flex: 1; }
  .forced-intro button { display: flex; align-items: center; gap: 6px; font-size: 11px; }
  .panel { overflow-y: auto; padding: 20px; border: 1px solid var(--border); border-radius: 12px; background: rgba(255, 255, 255, .025); }
  .toolbar, .row { flex: none; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .search, select { min-width: 0; height: 34px; padding: 0 10px; border-radius: 6px; border: 1px solid var(--border); background: #171e33; color: var(--text); font: inherit; font-size: 12px; }
  .search:focus, select:focus { outline: 2px solid var(--accent); outline-offset: -1px; }
  .list { flex: 1; min-height: 100px; overflow-y: auto; margin: 0; padding: 0 6px 0 0; list-style: none; display: flex; flex-direction: column; gap: 4px; }
  .list li { flex: none; display: flex; align-items: center; gap: 12px; min-height: 46px; padding: 8px 12px; border-radius: var(--radius); background: var(--surface); }
  .text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .name, .detail { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .name { font-size: 13px; } .detail { font-size: 12px; color: var(--text-dim); } .detail.bad { color: var(--warn); }
  .badges { display: flex; flex-wrap: wrap; gap: 4px; }
  .badge { padding: 2px 6px; border-radius: 4px; background: var(--surface-active); font-size: 10px; color: var(--text-dim); }
  .protected { color: var(--warn); }
  .amount { flex: none; min-width: 64px; text-align: right; font-size: 13px; color: var(--text-dim); font-variant-numeric: tabular-nums; }
  .btn.small { flex: none; height: 30px; padding: 0 14px; }
  .note { flex: none; margin: 0; font-size: 12px; line-height: 1.45; color: var(--text-dim); }
  .back { align-self: flex-start; display: flex; align-items: center; gap: 4px; }
  h2 { margin: 0; font-size: 18px; font-weight: 600; }
  .sub, .desc { margin: 0; font-size: 13px; line-height: 1.5; color: var(--text-dim); overflow-wrap: anywhere; }
  .message { flex: none; margin: 0; padding: 8px 12px; border-radius: var(--radius); background: rgba(70, 201, 139, 0.12); font-size: 13px; line-height: 1.5; }
  .message.bad { background: rgba(240, 179, 69, 0.12); color: var(--warn); }
  .link { flex: none; font-size: 12px; color: var(--accent-2); } .link:hover { text-decoration: underline; }
  .check { flex: none; }
  input[type='checkbox'] { width: 15px; height: 15px; margin: 0; accent-color: var(--accent); cursor: pointer; }
  .check-label { display: flex; align-items: flex-start; gap: 8px; font-size: 13px; line-height: 1.5; } .check-label input { flex: none; margin-top: 2px; }
  .force-form { flex: none; display: flex; flex-direction: column; gap: 8px; padding: 12px; border: 1px solid var(--border); border-radius: var(--radius); }
  .force-form label { flex: 1; min-width: 150px; display: flex; flex-direction: column; gap: 5px; font-size: 11px; color: var(--text-dim); }
  .force-form .btn { align-self: flex-end; }
  progress { width: 100%; height: 8px; accent-color: var(--accent); }
  button:disabled, input:disabled { opacity: .5; cursor: default; }
  .sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
  @media (max-width: 1100px) {
    .library-layout { grid-template-columns: 158px minmax(0, 1fr); }
    .library-main { padding: 18px 14px 12px; }
    .library-nav { padding-inline: 7px; } .category { gap: 7px; padding-inline: 8px; font-size: 11px; }
    .heading-copy h2 { font-size: 18px; }
    .library-heading { flex-wrap: wrap; } .heading-copy p { display: none; }
    .size-col { width: 72px; } .date-col { width: 88px; } .action-col { width: 38px; }
    .app-identity { gap: 8px; } .app-tile { width: 28px; height: 28px; font-size: 11px; }
  }
  @media (max-width: 860px) {
    .library-layout { display: flex; flex-direction: column; }
    .library-nav { flex: none; padding: 8px; border-right: none; border-bottom: 1px solid var(--border); overflow: visible; }
    .library-nav nav { flex-direction: row; overflow-x: auto; } .category { flex: none; white-space: nowrap; }
    .library-label, .inventory-summary, .library-note { display: none; }
    .library-main { flex: 1; padding: 14px 12px 10px; }
  }
  @media (max-height: 650px) { .inventory-summary, .library-note { display: none; } }
</style>
