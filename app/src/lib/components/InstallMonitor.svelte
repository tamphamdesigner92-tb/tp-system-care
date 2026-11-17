<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { formatCount, formatTime } from '../format';
  import { t, tOr } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import { monitorList, monitorBegin, monitorFinish, monitorDetail, maintenanceCancel, type MonitorSummary, type MonitorReport } from '../tools';

  let name = $state('');
  let root = $state('');
  let vendor = $state('');
  let sessions = $state<MonitorSummary[]>([]);
  let report = $state<MonitorReport | null>(null);
  let working = $state<string | null>(null);
  let progress = $state({ path: '', count: 0 });
  let error = $state('');
  let stopping = $state(false);
  let loaded = $state(false);
  let alive = true;
  let request = $state<string | null>(null);
  const active = $derived(sessions.find((session) => session.finishedAt === null));
  const errorText = (e: unknown) => {
    const code = String(e).split(':', 1)[0];
    if (code === 'monitor_invalid_root') return t('err.invalid_monitor_target');
    if (code === 'monitor_invalid_vendor') return t('err.invalid_vendor');
    if (code === 'monitor_cancelled') return t('err.cancelled');
    return tOr(`err.${code}`, String(e));
  };
  const counts = $derived(report ? {
    file: { added: report.changes.filter((c) => c.kind === 'file' && c.change === 'added').length, modified: report.changes.filter((c) => c.kind === 'file' && c.change === 'modified').length, removed: report.changes.filter((c) => c.kind === 'file' && c.change === 'removed').length },
    registry: { added: report.changes.filter((c) => c.kind === 'registry' && c.change === 'added').length, modified: report.changes.filter((c) => c.kind === 'registry' && c.change === 'modified').length, removed: report.changes.filter((c) => c.kind === 'registry' && c.change === 'removed').length },
  } : null);

  async function refresh() {
    if (!alive || working) return;
    working = 'list'; error = '';
    try { const list = await monitorList(); if (alive) { sessions = list; loaded = true; } }
    catch (e) { if (alive) error = errorText(e); }
    finally { if (alive) working = null; }
  }
  onMount(() => { if (inTauri()) void refresh(); });
  onDestroy(() => { alive = false; if (request) void maintenanceCancel(request).catch(() => {}); });
  async function stop() {
    if (!request || stopping) return;
    stopping = true;
    try { await maintenanceCancel(request); }
    catch (e) { if (alive) { error = errorText(e); stopping = false; } }
  }
  async function capture(finishId: number | null) {
    if (working || !loaded || (finishId === null && (active || !name.trim() || !root.trim()))) return;
    if (finishId === null && (new TextEncoder().encode(name.trim()).length > 128 || /[\u0000-\u001f\u007f-\u009f]/.test(name))) {
      error = t('err.monitor_invalid_name');
      return;
    }
    const id = crypto.randomUUID(); request = id;
    working = finishId === null ? 'before' : 'after'; error = ''; report = null; stopping = false;
    progress = { path: '', count: 0 };
    try {
      if (finishId === null) {
        const summary = await monitorBegin(name.trim(), root.trim(), vendor.trim(), id, (path, count) => { if (alive && request === id) progress = { path, count }; });
        if (!alive) return;
        sessions = [summary, ...sessions.filter((s) => s.id !== summary.id)];
      } else {
        const result = await monitorFinish(finishId, id, (path, count) => { if (alive && request === id) progress = { path, count }; });
        if (!alive) return;
        report = result;
        sessions = sessions.map((s) => s.id === result.session.id ? result.session : s);
      }
    } catch (e) { if (alive) { error = errorText(e); report = null; } }
    finally { if (alive) { if (request === id) request = null; working = null; stopping = false; } }
  }
  async function detail(id: number) {
    if (working) return;
    working = 'detail'; error = ''; report = null;
    try { const value = await monitorDetail(id); if (alive) report = value; }
    catch (e) { if (alive) error = errorText(e); }
    finally { if (alive) working = null; }
  }
</script>

<section class="panel">
  <p class="warning">{t('monitor.warning')}</p>
  <p class="note">{t('monitor.scope')}</p>
  {#if !inTauri()}<p>{t('tool.needApp')}</p>{:else}
    <div class="controls"><button class="btn" disabled={!!working} onclick={refresh}>{t('monitor.reload')}</button>
      {#if request}<button class="btn" disabled={stopping} onclick={stop}>{t(stopping ? 'scan.stopping' : 'scan.stop')}</button>{/if}
    </div>
    {#if error}<p class="warning" role="alert">{error}</p>{/if}
    {#if working}<p class="note" role="status">{t('uninstall.working')} {formatCount(progress.count)} <span class="path">{progress.path}</span></p>{/if}
    {#if active}
      <div class="card">
        <h2>{t('monitor.active', { name: active.name })}</h2>
        <p class="path">{active.root}</p>
        <p class="note">{formatTime(active.startedAt / 1000)} · {t('monitor.beforeCount', { n: formatCount(active.filesBefore) })}</p>
        <p>{t('monitor.installYourself')}</p>
        <p class="note">{t('monitor.finishNote')}</p>
        <button class="btn primary" disabled={!!working} onclick={() => capture(active.id)}>{t('monitor.finish')}</button>
      </div>
    {:else}
      <form onsubmit={(e) => { e.preventDefault(); void capture(null); }}>
        <h2>{t('monitor.before')}</h2>
        <label>{t('uninstall.targetName')}<input type="text" bind:value={name} required maxlength={128} disabled={!!working || !loaded} /></label>
        <label>{t('monitor.root')}<input type="text" bind:value={root} required placeholder="C:\Tools\Example App" disabled={!!working || !loaded} /></label>
        <p class="note">{t('monitor.rootNote')}</p>
        <label>{t('monitor.vendor')}<input type="text" bind:value={vendor} maxlength={64} placeholder="ExampleVendor" disabled={!!working || !loaded} /></label>
        <p class="note">{t('monitor.vendorNote')}</p>
        <button class="btn primary" type="submit" disabled={!!working || !loaded || !name.trim() || !root.trim()}>{t('monitor.begin')}</button>
      </form>
    {/if}
    <h2>{t('monitor.history')}</h2>
    <ul class="history">
      {#each sessions as session (session.id)}
        <li><div><strong>{session.name}</strong><p class="note path">{session.root} · {formatTime(session.startedAt / 1000)}</p>
          <p class="note">{session.finishedAt === null ? t('monitor.pending') : t('monitor.finished', { n: formatCount(session.changeCount) })} · {t(session.complete ? 'monitor.complete' : 'monitor.incomplete')}</p></div>
          <button class="btn" disabled={!!working || session.finishedAt === null} onclick={() => detail(session.id)}>{t('monitor.detail')}</button>
        </li>
      {:else}<li>{t('monitor.empty')}</li>{/each}
    </ul>
    {#if report}
      <h2>{t('monitor.report', { name: report.session.name })}</h2>
      <p class="note">{t(report.session.complete ? 'monitor.complete' : 'monitor.incomplete')} · {t('monitor.finished', { n: formatCount(report.session.changeCount) })}</p>
      {#if report.truncated}<p class="warning">{t('maintenance.truncated')}</p>{/if}
      {#each report.warnings as warning}<p class="warning">{tOr(`monitor.warning.${warning}`, tOr(`err.${warning}`, warning))}</p>{/each}
      {#if counts}<div class="counts">
        <p>{t('monitor.kind.file')}: {t('monitor.counts', counts.file)}</p>
        <p>{t('monitor.kind.registry')}: {t('monitor.counts', counts.registry)}</p>
      </div>{/if}
      <ul class="changes">
        {#each report.changes as change, i (i)}
          <li><strong>{tOr(`monitor.kind.${change.kind}`, change.kind)} · {tOr(`monitor.change.${change.change}`, change.change)}</strong>
            <p class="path">{change.path}</p>
            {#if change.kind === 'registry'}<p class="note">{t('monitor.valueName')}: {change.valueName || t('monitor.defaultValue')}</p>{/if}
          </li>
        {:else}<li>{t('monitor.noChanges')}</li>{/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .panel { flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 10px; padding-right: 8px; }
  h2 { margin: 0; font-size: 16px; } p { margin: 0; font-size: 12px; line-height: 1.5; } .note { color: var(--text-dim); }
  .warning { color: var(--warn); background: rgba(240,179,69,.1); padding: 8px 12px; border-radius: 8px; }
  .controls { display: flex; gap: 8px; } .card, form { display: flex; flex-direction: column; gap: 10px; background: var(--surface); padding: 12px; border-radius: 8px; }
  label { display: flex; flex-direction: column; gap: 4px; font-size: 12px; } input { height: 34px; border: 1px solid var(--border); border-radius: 6px; padding: 0 8px; background: #1a2040; color: var(--text); user-select: text; }
  form .btn, .card .btn { align-self: flex-start; flex-shrink: 0; } ul { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 6px; }
  li { padding: 10px 12px; border-radius: 8px; background: var(--surface); font-size: 12px; } .history li { display: flex; align-items: center; gap: 10px; } .history li div { flex: 1; min-width: 0; } strong { font-weight: 600; }
  .path { overflow-wrap: anywhere; user-select: text; } .counts { display: flex; flex-wrap: wrap; gap: 12px; }
</style>
