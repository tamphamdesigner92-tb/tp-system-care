<script lang="ts">
  import Icon from './Icon.svelte';
  import { formatBytes, formatCount } from '../format';
  import { t, tOr, type Key } from '../i18n.svelte';
  import type { Finding, ModuleId, ScanReport } from '../scan';

  let {
    report,
    elevated,
    onfix,
    onback,
  }: {
    report: ScanReport;
    elevated: boolean | null;
    onfix: (ids: string[]) => void;
    onback: () => void;
  } = $props();

  const all = $derived(report.modules.flatMap((m) => m.findings));
  let selected = $state<Record<string, boolean>>({});

  // Chọn sẵn các mục engine khuyến nghị mỗi khi có kết quả quét mới.
  $effect(() => {
    selected = Object.fromEntries(all.map((f) => [f.id, f.recommended]));
  });

  const chosen = $derived(all.filter((f) => selected[f.id]));
  const reclaim = $derived(chosen.reduce((sum, f) => sum + f.bytes, 0));

  function moduleState(findings: Finding[]): boolean | 'mixed' {
    const on = findings.filter((f) => selected[f.id]).length;
    return on === 0 ? false : on === findings.length ? true : 'mixed';
  }

  function toggleModule(findings: Finding[]) {
    const next = moduleState(findings) !== true;
    for (const f of findings) selected[f.id] = next;
  }

  function title(f: Finding): string {
    return f.name ?? tOr(`kind.${f.kind}`, f.kind);
  }

  /** Dòng phụ: với mục có tên riêng thì ghi loại trước đường dẫn. */
  function subtitle(f: Finding): string {
    const kind = f.name ? tOr(`kind.${f.kind}`, f.kind) : '';
    return [kind, f.detail ?? ''].filter(Boolean).join(' · ');
  }

  function amount(f: Finding): string {
    if (f.bytes > 0) return formatBytes(f.bytes);
    return f.count > 1 ? t('result.count', { n: formatCount(f.count) }) : '';
  }

  const groupNotes: Partial<Record<ModuleId, Key>> = {
    startup: 'result.startupNote',
    vuln: 'result.vulnNote',
    security: 'result.securityNote',
  };

  const moduleLabel = (id: ModuleId) => t(`item.${id}` as Key);
</script>

<section class="results">
  <header>
    <div class="summary">
      <h1>
        {all.length > 0 ? t('result.title', { n: formatCount(all.length) }) : t('result.titleNone')}
      </h1>
      <p>
        {#if report.cancelled}{t('result.cancelled')}{/if}
        {#if reclaim > 0}{t('result.reclaim', { size: formatBytes(reclaim) })}{/if}
      </p>
    </div>
    <div class="actions">
      <button class="btn" onclick={onback}>{t('result.back')}</button>
      {#if all.length > 0}
        <button class="btn primary" disabled={chosen.length === 0} onclick={() => onfix(chosen.map((f) => f.id))}>
          {t('result.fix', { n: formatCount(chosen.length) })}
        </button>
      {/if}
    </div>
  </header>

  {#if all.length > 0}
    <div class="list">
      {#each report.modules.filter((m) => m.findings.length > 0) as mod (mod.module)}
        {@const state = moduleState(mod.findings)}
        <div class="group">
          <button
            class="group-head"
            role="checkbox"
            aria-checked={state}
            title={t('result.selectModule')}
            onclick={() => toggleModule(mod.findings)}
          >
            <span class="box" class:on={state === true} class:mixed={state === 'mixed'}>
              <Icon name={state === 'mixed' ? 'minimize' : 'check'} size={12} />
            </span>
            <span class="group-name">{moduleLabel(mod.module)}</span>
            <span class="group-count">{formatCount(mod.findings.length)}</span>
          </button>
          {#if groupNotes[mod.module]}
            <p class="group-note">{t(groupNotes[mod.module]!)}</p>
          {/if}
          {#each mod.findings as f (f.id)}
            <button class="row" role="checkbox" aria-checked={selected[f.id]} onclick={() => (selected[f.id] = !selected[f.id])}>
              <span class="box" class:on={selected[f.id]}><Icon name="check" size={12} /></span>
              <span class="text">
                <span class="title">{title(f)}</span>
                {#if subtitle(f)}<span class="detail" title={f.detail}>{subtitle(f)}</span>{/if}
              </span>
              {#if f.needsAdmin && elevated === false}
                <span class="tag warn">{t('result.needsAdmin')}</span>
              {/if}
              {#if f.module === 'vuln'}
                <span class="tag">{t('result.manual')}</span>
              {/if}
              {#if f.undoable}
                <span class="tag">{t('result.undoable')}</span>
              {/if}
              <span class="amount">{amount(f)}</span>
            </button>
          {/each}
        </div>
      {/each}
    </div>
    <p class="note">{t('result.note')}</p>
  {/if}
</section>

<style>
  .results {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 4px 24px 16px;
  }

  header {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
  }

  .summary {
    min-width: 0;
  }

  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
  }

  header p {
    margin: 4px 0 0;
    min-height: 18px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .actions {
    flex: none;
    display: flex;
    gap: 8px;
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding-right: 6px;
  }

  .group {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .group-head {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 32px;
    padding: 0 12px;
    text-align: left;
    font-weight: 600;
    font-size: 13px;
  }

  .group-count {
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--surface-active);
    font-size: 12px;
    font-weight: 400;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .group-note {
    margin: 0 12px 4px 38px;
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-faint);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 44px;
    padding: 6px 12px;
    border-radius: var(--radius);
    background: var(--surface);
    text-align: left;
    transition: background 0.15s;
  }

  .row:hover {
    background: var(--surface-hover);
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .title,
  .detail {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .title {
    font-size: 13px;
  }

  .detail {
    font-size: 12px;
    color: var(--text-faint);
  }

  .tag {
    flex: none;
    padding: 2px 8px;
    border-radius: 999px;
    border: 1px solid var(--border);
    font-size: 11px;
    color: var(--text-dim);
    white-space: nowrap;
  }

  .tag.warn {
    border-color: rgba(240, 179, 69, 0.4);
    color: var(--warn);
  }

  .amount {
    flex: none;
    min-width: 72px;
    text-align: right;
    font-size: 13px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .box {
    width: 16px;
    height: 16px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 4px;
    border: 1.5px solid var(--text-faint);
    color: transparent;
    transition:
      background 0.15s,
      border-color 0.15s;
  }

  .box.on,
  .box.mixed {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .note {
    flex: none;
    margin: 0;
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-faint);
  }
</style>
