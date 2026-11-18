<script lang="ts">
  import { onDestroy } from 'svelte';
  import { check, type Update } from '@tauri-apps/plugin-updater';
  import { relaunch } from '@tauri-apps/plugin-process';
  import { t } from '../i18n.svelte';
  import { inTauri } from '../tauri';

  /** Kiểm tra lại mỗi 6 giờ khi ứng dụng mở lâu. */
  const INTERVAL = 6 * 60 * 60 * 1000;

  let update = $state<Update | null>(null);
  let dismissed = $state(false);
  let installing = $state(false);
  let percent = $state<number | null>(null);
  let error = $state('');
  let destroyed = false;

  // Backend giả (?mock) không có máy chủ cập nhật.
  const enabled = inTauri() && !new URLSearchParams(window.location.search).has('mock');

  async function look() {
    if (!enabled || installing) return;
    try {
      const found = await check();
      if (destroyed) return;
      if (found && found.version !== update?.version) dismissed = false;
      update = found;
    } catch {
      // Không có mạng hoặc máy chủ lỗi: thử lại ở lần sau, không làm phiền người dùng.
    }
  }

  async function install() {
    if (!update || installing) return;
    installing = true;
    error = '';
    let total = 0;
    let received = 0;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started') total = event.data.contentLength ?? 0;
        else if (event.event === 'Progress') {
          received += event.data.chunkLength;
          percent = total ? Math.min(100, Math.round((received / total) * 100)) : null;
        }
      });
      // Trên Windows trình cài tự đóng ứng dụng; dòng này chỉ là dự phòng.
      await relaunch();
    } catch (e) {
      if (destroyed) return;
      error = String(e);
      installing = false;
      percent = null;
    }
  }

  const timer = enabled ? setInterval(look, INTERVAL) : undefined;
  onDestroy(() => {
    destroyed = true;
    if (timer) clearInterval(timer);
  });
  void look();
</script>

{#if update && !dismissed}
  <div class="banner" role="status">
    <span class="text">
      {#if installing}
        {percent === null ? t('update.downloading') : t('update.progress', { n: percent })}
      {:else}
        {t('update.available', { version: update.version })}
      {/if}
      {#if error}<span class="bad">{t('update.failed', { error })}</span>{/if}
    </span>
    {#if !installing}
      <button class="btn small primary" onclick={install}>{t('update.install')}</button>
      <button class="link" onclick={() => (dismissed = true)}>{t('update.later')}</button>
    {/if}
  </div>
{/if}

<style>
  .banner {
    flex: none;
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 24px 8px;
    padding: 8px 12px;
    border-radius: var(--radius);
    background: rgba(70, 201, 139, 0.12);
    font-size: 13px;
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .bad {
    font-size: 12px;
    color: var(--warn);
    overflow-wrap: anywhere;
  }

  .btn.small {
    flex: none;
    height: 28px;
    padding: 0 12px;
  }

  .link {
    flex: none;
    font-size: 12px;
    color: var(--accent-2);
  }
</style>
