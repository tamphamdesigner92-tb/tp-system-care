<script lang="ts">
  import Icon from '../lib/components/Icon.svelte';
  import { t } from '../lib/i18n.svelte';
  import type { PageDef } from '../lib/pages';

  let { page }: { page: PageDef } = $props();
</script>

<section class="placeholder">
  <div class="badge"><Icon name={page.icon} size={36} /></div>
  <h1>{t(page.label)}</h1>
  {#if page.desc}
    <p>{t(page.desc)}</p>
  {/if}
  <span class="tag">{t('page.soon')}</span>
</section>

<style>
  .placeholder {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 0 48px 48px;
    text-align: center;
  }

  .badge {
    width: 72px;
    height: 72px;
    display: grid;
    place-items: center;
    border-radius: 20px;
    background: var(--surface);
    color: var(--accent-2);
  }

  h1 {
    margin: 8px 0 0;
    font-size: 22px;
    font-weight: 600;
  }

  p {
    margin: 0;
    max-width: 460px;
    line-height: 1.5;
    color: var(--text-dim);
  }

  .tag {
    margin-top: 8px;
    padding: 4px 12px;
    border-radius: 999px;
    border: 1px solid var(--border);
    font-size: 12px;
    color: var(--text-faint);
  }
</style>
