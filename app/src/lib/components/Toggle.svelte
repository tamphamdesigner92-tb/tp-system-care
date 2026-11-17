<script lang="ts">
  let {
    checked,
    disabled = false,
    label,
    onchange,
  }: { checked: boolean; disabled?: boolean; label: string; onchange: () => void } = $props();
</script>

<button class="toggle" class:on={checked} role="switch" aria-checked={checked} aria-label={label} {disabled} onclick={onchange}>
  <span class="knob"></span>
</button>

<style>
  .toggle {
    position: relative;
    width: 38px;
    height: 22px;
    flex: none;
    border-radius: 999px;
    background: var(--surface-active);
    border: 1px solid var(--border);
    transition: background 0.2s;
  }

  .toggle.on {
    background: var(--accent);
    border-color: var(--accent);
  }

  .toggle:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: #fff;
    transition: transform 0.2s;
  }

  .on .knob {
    transform: translateX(16px);
  }
</style>
