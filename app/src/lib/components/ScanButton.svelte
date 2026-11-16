<script lang="ts">
  let {
    label,
    sub,
    compact = false,
    busy = false,
    onclick,
  }: { label: string; sub?: string; compact?: boolean; busy?: boolean; onclick?: () => void } = $props();
</script>

<button class="scan" class:compact class:busy disabled={busy} {onclick}>
  <svg class="ring" viewBox="0 0 240 240" aria-hidden="true">
    <defs>
      <linearGradient id="scan-arc" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="#9fd8ff" />
        <stop offset="1" stop-color="#4f8cff" />
      </linearGradient>
    </defs>
    <circle class="track" cx="120" cy="120" r="108" />
    <!-- Chu vi ≈ 678.6: ba cung dài ngắn khác nhau, cách nhau bằng khe hở -->
    <circle class="arc" cx="120" cy="120" r="108" stroke-dasharray="250 26 150 26 190 36.6" />
  </svg>
  <span class="disc">
    <span class="text">{label}</span>
    {#if sub}<span class="sub">{sub}</span>{/if}
  </span>
</button>

<style>
  .scan {
    position: relative;
    width: 230px;
    height: 230px;
    flex: none;
    border-radius: 50%;
    display: grid;
    place-items: center;
    transition:
      width 0.3s ease,
      height 0.3s ease;
  }

  .scan.compact {
    width: 150px;
    height: 150px;
  }

  .ring {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    animation: spin 14s linear infinite;
    filter: drop-shadow(0 0 10px rgba(91, 157, 255, 0.35));
  }

  .scan:hover .ring {
    animation-duration: 4s;
  }

  .scan.busy {
    cursor: default;
  }

  .scan.busy .ring {
    animation-duration: 1.6s;
  }

  .scan.busy:hover .disc {
    filter: none;
  }

  .scan.busy:active .disc {
    transform: none;
  }

  .track {
    fill: none;
    stroke: rgba(91, 157, 255, 0.18);
    stroke-width: 10;
  }

  .arc {
    fill: none;
    stroke: url(#scan-arc);
    stroke-width: 10;
  }

  .disc {
    width: 80%;
    height: 80%;
    border-radius: 50%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    background: radial-gradient(circle at 50% 30%, #3b4470 0%, #262c50 55%, #1c2140 100%);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.12),
      0 12px 32px rgba(0, 0, 0, 0.45);
    transition:
      transform 0.15s ease,
      filter 0.15s ease;
  }

  .scan:hover .disc {
    filter: brightness(1.15);
  }

  .scan:active .disc {
    transform: scale(0.97);
  }

  .text {
    font-size: 44px;
    font-weight: 300;
    letter-spacing: 0.01em;
    transition: font-size 0.3s ease;
  }

  .compact .text {
    font-size: 28px;
  }

  .text {
    font-variant-numeric: tabular-nums;
  }

  .sub {
    font-size: 12px;
    color: var(--text-dim);
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
