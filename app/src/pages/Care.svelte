<script lang="ts">
  import Icon from '../lib/components/Icon.svelte';
  import ScanButton from '../lib/components/ScanButton.svelte';
  import ScanResults from '../lib/components/ScanResults.svelte';
  import { formatBytes, formatCount } from '../lib/format';
  import { t, type Key } from '../lib/i18n.svelte';
  import {
    MODULES,
    advise,
    cancel,
    fix,
    health as loadHealth,
    scan,
    type Advice,
    type FixReport,
    type Health,
    type ModuleId,
    type ScanReport,
  } from '../lib/scan';
  import { inTauri } from '../lib/tauri';

  let { elevated, onrescue }: { elevated: boolean | null; onrescue: () => void } = $props();

  type Mode = 'ai' | 'manual';
  type Phase = 'idle' | 'scanning' | 'results' | 'fixing' | 'done';
  type ModuleStatus = { state: 'waiting' | 'running' | 'finished'; count: number; bytes: number };

  const label = (id: ModuleId) => t(`item.${id}` as Key);

  let mode = $state<Mode>('ai');
  let phase = $state<Phase>('idle');
  let selected = $state<Record<string, boolean>>(Object.fromEntries(MODULES.map((id) => [id, true])));
  let notice = $state('');
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;

  let queue = $state<ModuleId[]>([]);
  let status = $state<Record<string, ModuleStatus>>({});
  let currentPath = $state('');
  let stopping = $state(false);
  let report = $state<ScanReport | null>(null);
  let fixProgress = $state({ current: 0, total: 0 });
  let fixReport = $state<FixReport | null>(null);

  /** Hạng mục Chế độ AI sẽ quét và điểm sức khỏe, lấy từ lịch sử quét. */
  let advice = $state<Advice[]>([]);
  let health = $state<Health | null>(null);

  const selectedCount = $derived(MODULES.filter((id) => selected[id]).length);
  const allSelected = $derived(selectedCount === MODULES.length);
  const finished = $derived(queue.filter((id) => status[id]?.state === 'finished').length);
  const percent = $derived(queue.length ? Math.round((finished / queue.length) * 100) : 0);

  function toggleAll() {
    const next = !allSelected;
    for (const id of MODULES) selected[id] = next;
  }

  async function refresh() {
    if (!inTauri()) return;
    try {
      [advice, health] = await Promise.all([advise(), loadHealth()]);
    } catch {
      // Không đọc được lịch sử: Chế độ AI sẽ quét mọi hạng mục.
      advice = [];
    }
  }
  refresh();

  function notify(text: string) {
    notice = text;
    clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => (notice = ''), 4000);
  }

  /** Quét các hạng mục; trả về báo cáo, hoặc null nếu thất bại. */
  async function runScan(modules: ModuleId[]): Promise<ScanReport | null> {
    queue = modules;
    status = Object.fromEntries(modules.map((id) => [id, { state: 'waiting', count: 0, bytes: 0 }]));
    currentPath = '';
    stopping = false;
    phase = 'scanning';
    try {
      return await scan(modules, (p) => {
        if (p.type === 'path') currentPath = p.path;
        else if (p.type === 'moduleStarted') status[p.module].state = 'running';
        else status[p.module] = { state: 'finished', count: p.count, bytes: p.bytes };
      });
    } catch (error) {
      phase = 'idle';
      notify(t('care.failed', { error: String(error) }));
      return null;
    }
  }

  async function startScan() {
    if (!inTauri()) return notify(t('care.needApp'));
    const modules =
      mode === 'manual'
        ? MODULES.filter((id) => selected[id])
        : advice.length > 0
          ? advice.map((a) => a.module)
          : [...MODULES];
    if (modules.length === 0) return notify(t('care.pickOne'));

    const result = await runScan(modules);
    if (!result) return;
    report = result;
    phase = 'results';
  }

  /** Quét mọi hạng mục rồi sửa ngay các mục được khuyến nghị, không qua bước xem. */
  async function oneClick() {
    if (!inTauri()) return notify(t('care.needApp'));
    const result = await runScan([...MODULES]);
    if (!result) return;
    report = result;
    const ids = result.modules.flatMap((m) => m.findings.filter((f) => f.recommended).map((f) => f.id));
    // Bị dừng giữa chừng hoặc không có gì để sửa: để người dùng xem kết quả.
    if (result.cancelled || ids.length === 0) {
      phase = 'results';
      return;
    }
    await startFix(ids);
  }

  async function stop() {
    stopping = true;
    await cancel();
  }

  async function startFix(ids: string[]) {
    fixProgress = { current: 0, total: ids.length };
    phase = 'fixing';
    try {
      fixReport = await fix(ids, (current, total) => (fixProgress = { current, total }));
      phase = 'done';
    } catch (error) {
      phase = 'results';
      notify(t('care.failed', { error: String(error) }));
    }
  }

  function reset() {
    report = null;
    fixReport = null;
    phase = 'idle';
    refresh();
  }
</script>

{#if phase === 'results' && report}
  <ScanResults {report} {elevated} onfix={startFix} onback={reset} />
{:else if phase === 'scanning'}
  <section class="care progress">
    <ScanButton label={`${percent}%`} sub={t('scan.scanning')} compact busy />
    <ul class="modules" class:two={queue.length > 5}>
      {#each queue as id (id)}
        {@const s = status[id]}
        <li class={s.state}>
          <span class="dot"></span>
          <span class="module-name">{label(id)}</span>
          <span class="module-state">
            {#if s.state === 'waiting'}
              {t('scan.waiting')}
            {:else if s.state === 'running'}
              {t('scan.running')}
            {:else if s.count === 0}
              {t('scan.clean')}
            {:else}
              {t('scan.found', { n: formatCount(s.count) })}{s.bytes > 0 ? ` · ${formatBytes(s.bytes)}` : ''}
            {/if}
          </span>
        </li>
      {/each}
    </ul>
    <p class="path" title={currentPath}>{currentPath}</p>
    <button class="btn" disabled={stopping} onclick={stop}>
      {stopping ? t('scan.stopping') : t('scan.stop')}
    </button>
  </section>
{:else if phase === 'fixing'}
  <section class="care progress">
    <ScanButton
      label={`${fixProgress.total ? Math.round((fixProgress.current / fixProgress.total) * 100) : 0}%`}
      compact
      busy
    />
    <p class="hint">{t('fix.running', fixProgress)}</p>
  </section>
{:else if phase === 'done' && fixReport}
  <section class="care done">
    <div class="badge" class:warn={fixReport.done === 0 && fixReport.manual === 0}><Icon name="check" size={36} /></div>
    <h1>
      {fixReport.done > 0 || fixReport.manual === 0
        ? fixReport.done > 0
          ? t('fix.title', { n: formatCount(fixReport.done) })
          : t('fix.titleNone')
        : t('fix.manual', { n: formatCount(fixReport.manual) })}
    </h1>
    {#if fixReport.bytes > 0}
      <p class="freed">{t('fix.freed', { size: formatBytes(fixReport.bytes) })}</p>
    {/if}
    {#if fixReport.failed > 0}
      <p class="warn-text">{t('fix.failed', { n: formatCount(fixReport.failed) })}</p>
    {/if}
    {#if fixReport.manual > 0}
      <p class="hint">{t('fix.manual', { n: formatCount(fixReport.manual) })}</p>
    {/if}
    {#if fixReport.undoable > 0}
      <p class="hint">{t('fix.undoable', { n: formatCount(fixReport.undoable) })}</p>
    {/if}
    <div class="row">
      {#if fixReport.recordId !== null}
        <button class="btn" onclick={onrescue}>{t('fix.openRescue')}</button>
      {/if}
      <button class="btn primary" onclick={reset}>{t('fix.done')}</button>
    </div>
  </section>
{:else}
  <section class="care" class:manual={mode === 'manual'}>
    <div class="stage">
      <ScanButton label={t('care.scan')} compact={mode === 'manual'} onclick={startScan} />
    </div>

    {#if mode === 'manual'}
      <div class="picker">
        <div class="picker-head">
          <span class="hint">{t('care.manualHint')}</span>
          <button class="check all" role="checkbox" aria-checked={allSelected} onclick={toggleAll}>
            <span class="box" class:on={allSelected}><Icon name="check" size={12} /></span>
            {t('care.selectAll')} ({selectedCount}/{MODULES.length})
          </button>
        </div>
        <div class="grid">
          {#each MODULES as id (id)}
            <button class="check" role="checkbox" aria-checked={selected[id]} onclick={() => (selected[id] = !selected[id])}>
              <span class="box" class:on={selected[id]}><Icon name="check" size={12} /></span>
              <span class="check-label">{label(id)}</span>
            </button>
          {/each}
        </div>
      </div>
    {:else}
      <div class="ai">
        <p class="health">
          {#if health && health.score !== null}
            <span class="score" class:mid={health.score < 80} class:low={health.score < 50}>{health.score}</span>
            <span class="health-text">
              <strong>{t('health.label')}</strong>
              <span>
                {health.issues > 0 ? t('health.issues', { n: formatCount(health.issues) }) : t('health.clean')}{health.scanned <
                health.total
                  ? ` · ${t('health.partial', { scanned: health.scanned, total: health.total })}`
                  : ''}
              </span>
            </span>
          {:else}
            <span class="hint">{t('health.none')}</span>
          {/if}
        </p>
        {#if advice.length > 0}
          <p class="plan">
            <span class="hint">{t('care.aiPlan')}</span>
            {#each advice as a (a.module)}
              <span class="chip" title={t(`reason.${a.reason}` as Key)}>{label(a.module)}</span>
            {/each}
          </p>
        {:else}
          <p class="hint">{t('care.aiHint')}</p>
        {/if}
        <button class="btn" title={t('care.oneClickHint')} onclick={oneClick}>{t('care.oneClick')}</button>
      </div>
    {/if}

    <div class="modes" role="tablist">
      <button role="tab" aria-selected={mode === 'ai'} class:active={mode === 'ai'} onclick={() => (mode = 'ai')}>
        {t('care.modeAi')}
      </button>
      <button
        role="tab"
        aria-selected={mode === 'manual'}
        class:active={mode === 'manual'}
        onclick={() => (mode = 'manual')}
      >
        {t('care.modeManual')}
      </button>
    </div>
  </section>
{/if}

{#if notice}
  <div class="notice" role="status">{notice}</div>
{/if}

<style>
  .care {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 0 32px 28px;
  }

  .stage {
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
  }

  .manual .stage {
    flex: none;
    padding: 4px 0 16px;
  }

  .hint {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
  }

/* Chế độ AI */
  .ai {
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    margin-bottom: 18px;
  }

  .ai p {
    margin: 0;
  }

  .health {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .score {
    min-width: 46px;
    height: 46px;
    padding: 0 8px;
    display: grid;
    place-items: center;
    border-radius: 999px;
    border: 2px solid var(--ok);
    font-size: 18px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--ok);
  }

  .score.mid {
    border-color: var(--warn);
    color: var(--warn);
  }

  .score.low {
    border-color: var(--danger);
    color: var(--danger);
  }

  .health-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 13px;
  }

  .health-text span {
    font-size: 12px;
    color: var(--text-dim);
  }

  .plan {
    max-width: 640px;
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    align-items: center;
    gap: 6px;
  }

  .chip {
    padding: 3px 10px;
    border-radius: 999px;
    background: var(--surface);
    border: 1px solid var(--border);
    font-size: 12px;
    white-space: nowrap;
  }

  /* Đang quét / đang sửa */
  .progress {
    justify-content: center;
    gap: 18px;
  }

  .modules {
    width: 100%;
    max-width: 440px;
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .modules.two {
    max-width: 640px;
    display: grid;
    grid-template-columns: 1fr 1fr;
  }

  .modules li {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 34px;
    padding: 0 12px;
    border-radius: var(--radius);
    background: var(--surface);
    font-size: 13px;
  }

  .dot {
    width: 8px;
    height: 8px;
    flex: none;
    border-radius: 50%;
    background: var(--text-faint);
  }

  .running .dot {
    background: var(--accent);
    animation: pulse 1s ease-in-out infinite;
  }

  .finished .dot {
    background: var(--ok);
  }

  .module-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .waiting .module-name {
    color: var(--text-dim);
  }

  .module-state {
    flex: none;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .path {
    width: 100%;
    max-width: 560px;
    height: 16px;
    margin: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    text-align: center;
    font-size: 12px;
    color: var(--text-faint);
  }

  /* Kết quả sửa */
  .done {
    justify-content: center;
    gap: 10px;
    text-align: center;
  }

  .badge {
    width: 72px;
    height: 72px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: rgba(70, 201, 139, 0.14);
    color: var(--ok);
  }

  .badge.warn {
    background: rgba(240, 179, 69, 0.14);
    color: var(--warn);
  }

  h1 {
    margin: 8px 0 0;
    font-size: 22px;
    font-weight: 600;
  }

  .freed {
    margin: 0;
    font-size: 15px;
    color: var(--accent-2);
  }

  .warn-text {
    margin: 0;
    font-size: 13px;
    color: var(--warn);
  }

  .row {
    display: flex;
    gap: 8px;
    margin-top: 14px;
  }

  /* Chọn hạng mục */
  .picker {
    flex: 1;
    min-height: 0;
    width: 100%;
    max-width: 660px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow-y: auto;
    padding-bottom: 16px;
  }

  .picker-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 8px;
  }

  .check {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    height: 36px;
    padding: 0 12px;
    border-radius: var(--radius);
    background: var(--surface);
    font-size: 13px;
    text-align: left;
    transition: background 0.15s;
  }

  button.check:hover {
    background: var(--surface-hover);
  }

  .check.all {
    flex: none;
    background: none;
    padding: 0 4px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .check-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
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

  .box.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .modes {
    flex: none;
    display: flex;
    padding: 3px;
    border-radius: 999px;
    background: rgba(0, 0, 0, 0.35);
    border: 1px solid var(--border);
  }

  .modes button {
    height: 36px;
    padding: 0 22px;
    border-radius: 999px;
    font-size: 13px;
    color: var(--text-dim);
    white-space: nowrap;
    transition:
      background 0.2s,
      color 0.2s;
  }

  .modes button:hover {
    color: var(--text);
  }

  .modes button.active {
    background: var(--accent-grad);
    color: #0b1020;
    font-weight: 600;
  }

  .notice {
    position: fixed;
    left: calc(50% + var(--sidebar-w) / 2);
    bottom: 84px;
    transform: translateX(-50%);
    max-width: calc(100% - var(--sidebar-w) - 48px);
    padding: 10px 16px;
    border-radius: var(--radius);
    background: #232a4d;
    border: 1px solid var(--border);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    font-size: 13px;
    text-align: center;
    animation: rise 0.2s ease;
  }

  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, 6px);
    }
  }

  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
</style>
