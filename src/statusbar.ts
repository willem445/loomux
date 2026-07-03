// Bottom status bar: live CPU / memory / GPU / VRAM meters.
//
// Each metric is a labelled bar that fills to its percentage and shifts hue
// from green (idle) through amber to red (saturated), plus a monospace value.
// The DOM skeleton lives in index.html; this module just wires up the stream
// and updates widths, colours, and text in place.

import { onSystemMetrics, type SystemMetrics } from "./metrics";

interface Meter {
  root: HTMLElement;
  fill: HTMLElement;
  val: HTMLElement;
}

function meter(id: string): Meter {
  const root = document.getElementById(id)!;
  return {
    root,
    fill: root.querySelector(".metric-fill") as HTMLElement,
    val: root.querySelector(".metric-val") as HTMLElement,
  };
}

const GIB = 1024 ** 3;
const gbFromBytes = (b: number): string => (b / GIB).toFixed(1);
const gbFromMib = (mib: number): string => (mib / 1024).toFixed(1);
const clamp = (n: number): number => (n < 0 ? 0 : n > 100 ? 100 : n);

/** Green (120°) at idle → red (0°) at full load. */
function hueFor(pct: number): string {
  return `hsl(${Math.round(120 - (clamp(pct) / 100) * 120)}, 62%, 52%)`;
}

function update(m: Meter, pct: number, text: string): void {
  const p = clamp(pct);
  m.fill.style.width = `${p}%`;
  m.fill.style.background = hueFor(p);
  m.val.textContent = text;
}

/** Subscribe to the metrics stream and keep the bar in sync. */
export function initStatusBar(): void {
  const cpu = meter("m-cpu");
  const mem = meter("m-mem");
  const gpu = meter("m-gpu");
  const vram = meter("m-vram");

  void onSystemMetrics((s: SystemMetrics) => {
    update(cpu, s.cpu, `${Math.round(s.cpu)}%`);

    const memPct = s.mem_total ? (s.mem_used / s.mem_total) * 100 : 0;
    update(mem, memPct, `${gbFromBytes(s.mem_used)}/${gbFromBytes(s.mem_total)} GB`);

    if (s.gpu) {
      gpu.root.classList.remove("metric-na");
      vram.root.classList.remove("metric-na");
      gpu.root.title = s.gpu.name;
      vram.root.title = s.gpu.name;
      update(gpu, s.gpu.util, `${Math.round(s.gpu.util)}%`);
      const vramPct = s.gpu.vram_total_mb
        ? (s.gpu.vram_used_mb / s.gpu.vram_total_mb) * 100
        : 0;
      update(
        vram,
        vramPct,
        `${gbFromMib(s.gpu.vram_used_mb)}/${gbFromMib(s.gpu.vram_total_mb)} GB`
      );
    } else {
      gpu.root.classList.add("metric-na");
      vram.root.classList.add("metric-na");
      gpu.root.title = vram.root.title = "No NVIDIA GPU detected";
      update(gpu, 0, "n/a");
      update(vram, 0, "n/a");
    }
  });
}
