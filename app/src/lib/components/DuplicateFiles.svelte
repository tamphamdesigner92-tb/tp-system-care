<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { formatBytes, formatCount, formatTime } from '../format';
  import { t, tOr } from '../i18n.svelte';
  import { inTauri } from '../tauri';
  import { duplicateScan, duplicateRecycle, duplicateReveal, largeLocations, maintenanceCancel,
    type DuplicateGroup, type DuplicateScan, type DuplicateRemoval, type Location } from '../tools';

  let locations = $state<Location[]>([]);
  let root = $state('');
  let minMb = $state(10);
  let groups = $state<DuplicateGroup[]>([]);
  let scan = $state<DuplicateScan | null>(null);
  let report = $state<DuplicateRemoval | null>(null);
  let picked = $state<Record<string, boolean>>({});
  let working = $state<'scan' | 'remove' | null>(null);
  let stopping = $state(false);
  let progress = $state({ path: '', count: 0 });
  let error = $state('');
  let request: string | null = null;
  let alive = true;
  const chosen = $derived(groups.flatMap((g) => g.files.filter((f) => picked[f.id]).map((f) => ({ ...f, size: g.size }))));
  const bytes = $derived(chosen.reduce((sum, f) => sum + f.size, 0));
  const extras = $derived(groups.reduce((sum, g) => sum + g.size * Math.max(0, g.files.length - 1), 0));
  const errorText = (e: unknown) => tOr(`err.${e}`, String(e));
  const kept = (g: DuplicateGroup) => g.files.filter((f) => !picked[f.id]).length;

  onMount(() => {
    if (!inTauri()) return;
    largeLocations().then((list) => {
      if (!alive) return;
      locations = list;
      root = list[0]?.path ?? '';
    }).catch((e) => { if (alive) error = errorText(e); });
  });
  onDestroy(() => {
    alive = false;
    if (request) void maintenanceCancel(request).catch(() => {});
  });

  async function stop() {
    if (!request || stopping) return;
    stopping = true;
    try { await maintenanceCancel(request); }
    catch (e) { if (alive) { error = errorText(e); stopping = false; } }
  }
  async function start() {
    if (working || !root) return;
    const id = crypto.randomUUID();
    request = id;
    working = 'scan'; stopping = false; error = ''; groups = []; picked = {}; scan = null; report = null;
    progress = { path: '', count: 0 };
    try {
      const result = await duplicateScan(root, minMb, id, (path, count) => {
        if (alive && request === id) progress = { path, count };
      });
      if (!alive) return;
      scan = result; groups = result.groups;
    } catch (e) {
      if (alive) { error = errorText(e); groups = []; scan = null; picked = {}; }
    } finally {
      if (alive) { if (request === id) request = null; working = null; stopping = false; }
    }
  }
  async function recycle() {
    if (working || !chosen.length) return;
    if (groups.some((g) => kept(g) === 0)) { error = t('err.keep_one'); return; }
    const id = crypto.randomUUID();
    const ids = chosen.map((f) => f.id);
    request = id; working = 'remove'; stopping = false; error = ''; report = null;
    try {
      const result = await duplicateRecycle(ids, id);
      if (!alive) return;
      report = result;
      const removed = new Set(result.removedIds);
      groups = groups.map((g) => ({ ...g, files: g.files.filter((f) => !removed.has(f.id)) }));
      for (const key of result.removedIds) delete picked[key];
    } catch (e) {
      if (alive) { error = errorText(e); groups = []; scan = null; picked = {}; }
    } finally {
      if (alive) { if (request === id) request = null; working = null; stopping = false; }
    }
  }
  async function show(id: string) {
    try { await duplicateReveal(id); }
    catch (e) { if (alive) { error = errorText(e); groups = []; scan = null; picked = {}; } }
  }
</script>

<section class="panel">
  <h1>{t('duplicate.title')}</h1>
  <p class="note">{t('duplicate.desc')}</p>
  {#if !inTauri()}<p>{t('tool.needApp')}</p>{:else}
    <div class="controls">
      <label class="root">{t('clean.where')}<select bind:value={root} disabled={!!working}>
        {#each locations as l (l.path)}<option value={l.path}>{l.kind === 'profile' ? t('clean.profile') : l.path}</option>{/each}
      </select></label>
      <label>{t('clean.minSize')}<select bind:value={minMb} disabled={!!working}>
        {#each [1, 10, 50] as size}<option value={size}>{formatBytes(size * 1024 * 1024)}</option>{/each}
      </select></label>
      <button class="btn primary" disabled={!!working || !root} onclick={start}>{t('clean.find')}</button>
      {#if working}<button class="btn" disabled={stopping} onclick={stop}>{t(stopping ? 'scan.stopping' : 'scan.stop')}</button>{/if}
    </div>
    {#if error}<p class="message bad" role="alert">{error}</p>{/if}
    {#if working}<p class="note" role="status">{t(working === 'scan' ? 'clean.scanning' : 'uninstall.removing', { n: formatCount(progress.count) })}<span class="path">{progress.path}</span></p>{/if}
    {#if report}<p class="message" class:bad={report.failed > 0} role="status">
      {t('clean.recycled', { n: formatCount(report.done), size: formatBytes(report.bytes) })}
      {#if report.failed}{t('clean.recycleFailed', { n: formatCount(report.failed) })}{/if}
      {#if report.cancelled}{t('result.cancelled')}{/if}
      {#if report.recordId !== null}{t('maintenance.record', { id: report.recordId })}{/if}
    </p>
      {#if report.recovery.length}<div class="message" class:bad={report.failed > 0} role={report.failed > 0 ? 'alert' : 'status'}>
        {#each report.recovery as detail}<p class="path">{t(detail.startsWith('recycled: ') ? 'maintenance.recycledLocation' : 'maintenance.recovery')} {detail}</p>{/each}
      </div>{/if}
    {/if}
    {#if scan}
      <p class="note">{t('duplicate.stats', { n: formatCount(scan.filesChecked), skipped: formatCount(scan.skipped), groups: formatCount(groups.filter((g) => g.files.length > 1).length), size: formatBytes(extras) })}
        {#if scan.cancelled} {t('result.cancelled')}{/if}{#if scan.truncated} {t('maintenance.truncated')}{/if}
      </p>
      <button class="btn primary" disabled={!!working || !chosen.length || groups.some((g) => kept(g) === 0)} onclick={recycle}>{t('clean.recycle', { n: formatCount(chosen.length), size: formatBytes(bytes) })}</button>
      {#each groups as g (g.id)}
        <fieldset>
          <legend>{t('duplicate.group', { n: g.files.length, size: formatBytes(g.size), extra: formatBytes(g.size * Math.max(0, g.files.length - 1)) })}</legend>
          <p class="note">{t('duplicate.keep')}</p>
          {#each g.files as f (f.id)}
            <div class="row">
              <label class="file"><input type="checkbox" checked={!!picked[f.id]} disabled={!!working || (!picked[f.id] && kept(g) <= 1)} onchange={() => (picked[f.id] = !picked[f.id])} aria-label={t('clean.selectFile', { path: f.path })} />
                <span class="path">{f.path}<small>{formatTime(f.modified)}</small></span>
              </label>
              <button class="btn" disabled={!!working} onclick={() => show(f.id)}>{t('tool.reveal')}</button>
            </div>
          {/each}
        </fieldset>
      {:else}<p class="note">{t('duplicate.none')}</p>{/each}
    {/if}
    <p class="note">{t('duplicate.note')}</p>
  {/if}
</section>

<style>
  .panel { flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; align-items: stretch; gap: 10px; padding-right: 8px; }
  h1 { margin: 0; font-size: 20px; } p { margin: 0; }
  .note { color: var(--text-dim); font-size: 12px; line-height: 1.5; }
  .controls, .row { display: flex; align-items: flex-end; gap: 10px; } .row { align-items: center; margin: 8px 0; }
  label { display: flex; flex-direction: column; gap: 5px; font-size: 12px; } .root, .file { flex: 1; min-width: 0; }
  select { background: #1a2040; color: var(--text); border: 1px solid var(--border); border-radius: 6px; height: 36px; padding: 0 8px; }
  fieldset { margin: 0; border: 1px solid var(--border); border-radius: 8px; background: var(--surface); min-width: 0; }
  legend { font-size: 13px; } .file { flex-direction: row; align-items: center; gap: 8px; }
  input { accent-color: var(--accent); flex: none; } .path { overflow-wrap: anywhere; user-select: text; } small { display: block; color: var(--text-faint); margin-top: 3px; }
  .message { padding: 8px 12px; border-radius: 8px; background: rgba(70,201,139,.12); font-size: 12px; line-height: 1.5; }
  .bad { color: var(--warn); background: rgba(240,179,69,.12); } .panel > .btn { align-self: flex-start; flex-shrink: 0; }
</style>
