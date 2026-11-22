import { useMemo, useRef, useState } from 'react';
import { hex, sensorName, type Fan, type Point } from './api';
import { Tip } from './components';

/** Colour per thermal mode, from the theme. */
export const MODE_COLOR: Record<string, string> = {
  Quiet: 'var(--hue-sage-mark)', Balanced: 'var(--ink)', Performance: 'var(--hue-amber-mark)', 'Full Speed': 'var(--hue-red-mark)', 'Battery Saver': 'var(--hue-silver-a)', 'G-Mode': 'var(--hue-purple-mark)',
};

const sensorOf = (p: Point, sensor: number | null) => (sensor === 6 ? p.gpu_c : p.cpu_c);

/**
 * The fan's real response curve: every reading as a dot at (its sensor's temperature, the fan's speed),
 * coloured by the mode it was in, with the median speed per 2 °C drawn through it. This is what the
 * firmware actually does in each mode, the thing a custom curve would change.
 */
export function FanResponse({ points, fan, fanIndex }: { points: Point[]; fan: Fan; fanIndex: number }) {
  const W = 520, H = 230, T0 = 40, T1 = 105;
  const max = Math.max(1000, fan.max_rpm ?? 0, ...points.map((p) => p.fans[fanIndex] ?? 0)) * 1.08;
  const x = (t: number) => ((t - T0) / (T1 - T0)) * W;
  const y = (r: number) => H - (r / max) * H;
  const dots = useMemo(() => points.flatMap((p) => {
    const t = sensorOf(p, fan.sensor), r = p.fans[fanIndex];
    return t != null && r != null ? [{ t, r, mode: p.profile ?? '?' }] : [];
  }), [points, fan.sensor, fanIndex]);
  const median = useMemo(() => {
    const bins = new Map<number, number[]>();
    for (const d of dots) {
      const b = Math.floor(d.t / 2) * 2;
      bins.set(b, [...(bins.get(b) ?? []), d.r]);
    }
    return [...bins.entries()].sort((a, b) => a[0] - b[0]).map(([b, rs]) => { const s = [...rs].sort((p, q) => p - q); return { t: b + 1, r: s[Math.floor(s.length / 2)], n: s.length }; });
  }, [dots]);
  const ref = useRef<SVGSVGElement>(null);
  const [hov, setHov] = useState<{ t: number; r: number; n: number } | null>(null);
  const modes = [...new Set(dots.map((d) => d.mode))];
  return (
    <div className="fgraph">
      <svg ref={ref} viewBox={`-44 -10 ${W + 56} ${H + 34}`} onPointerMove={(e) => {
        const r = ref.current?.getBoundingClientRect();
        if (!r) return;
        const t = T0 + (((e.clientX - r.left) / r.width) * (W + 56) - 44) / W * (T1 - T0);
        setHov(median.reduce<typeof hov>((best, m) => (best == null || Math.abs(m.t - t) < Math.abs(best.t - t) ? m : best), null));
      }} onPointerLeave={() => setHov(null)}>
        {[50, 60, 70, 80, 90, 100].map((t) => <g key={t}><line x1={x(t)} x2={x(t)} y1={0} y2={H} className="grid-l" /><text x={x(t)} y={H + 16} className="axis" textAnchor="middle">{t}°</text></g>)}
        {[0, 0.25, 0.5, 0.75, 1].map((f) => <g key={f}><line x1={0} x2={W} y1={y(max * f / 1.08)} y2={y(max * f / 1.08)} className="grid-l" /><text x={-8} y={y(max * f / 1.08) + 4} className="axis" textAnchor="end">{Math.round(max * f / 1.08 / 100) * 100}</text></g>)}
        {fan.max_rpm != null && <g><line x1={0} x2={W} y1={y(fan.max_rpm)} y2={y(fan.max_rpm)} className="maxline" /><text x={W - 4} y={y(fan.max_rpm) - 5} className="axis" textAnchor="end">reported max {fan.max_rpm}</text></g>}
        <rect x={x(95)} y={0} width={x(T1) - x(95)} height={H} className="hotband" />
        {dots.map((d, i) => <circle key={i} cx={x(d.t) + ((i * 7919) % 9) / 9 - 0.5} cy={y(d.r)} r={2.6} style={{ fill: MODE_COLOR[d.mode] ?? 'var(--muted)' }} className="sdot" />)}
        <polyline points={median.map((m) => `${x(m.t)},${y(m.r)}`).join(' ')} className="medline" />
        {hov && <g><line x1={x(hov.t)} x2={x(hov.t)} y1={0} y2={H} className="cross" /><circle cx={x(hov.t)} cy={y(hov.r)} r={5} className="medpt" /></g>}
      </svg>
      <div className="legend">
        {modes.map((m) => <span key={m}><i style={{ background: MODE_COLOR[m] ?? 'var(--muted)' }} />{m}</span>)}
        <span><i className="medkey" />median</span>
        {hov && <span className="mono">at {hov.t - 1}–{hov.t + 1} °C: {hov.r} rpm ({hov.n} readings)</span>}
      </div>
    </div>
  );
}

/** Speed and temperature on one chart, each on its own axis, so you can see the fan react. */
export function SpeedAndTemp({ points, fan, fanIndex }: { points: Point[]; fan: Fan; fanIndex: number }) {
  const W = 800, H = 170;
  const rpm = points.map((p) => p.fans[fanIndex] ?? null);
  const temp = points.map((p) => sensorOf(p, fan.sensor));
  const rmax = Math.max(1000, fan.max_rpm ?? 0, ...rpm.map((v) => v ?? 0)) * 1.08;
  const n = Math.max(2, points.length);
  const line = (vals: (number | null)[], lo: number, hi: number) => {
    let d = '', pen = false;
    vals.forEach((v, i) => {
      if (v == null) { pen = false; return; }
      const win = vals.slice(Math.max(0, i - 2), i + 3).filter((q): q is number => q != null);
      const e = win.reduce((a, b) => a + b, 0) / win.length;
      d += `${pen ? 'L' : 'M'}${(i / (n - 1)) * W},${H - ((e - lo) / (hi - lo)) * H}`;
      pen = true;
    });
    return d;
  };
  const last = (vals: (number | null)[]) => [...vals].reverse().find((v) => v != null);
  return (
    <div className="chart">
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none">
        {[0.25, 0.5, 0.75].map((f) => <line key={f} x1={0} x2={W} y1={H * f} y2={H * f} className="grid-l" />)}
        <path d={line(temp, 30, 105)} fill="none" style={{ stroke: fan.sensor === 6 ? 'var(--hue-purple-mark)' : 'var(--hue-red-mark)' }} strokeWidth={2} strokeDasharray="5 4" vectorEffect="non-scaling-stroke" />
        <path d={line(rpm, 0, rmax)} fill="none" style={{ stroke: 'var(--ink)' }} strokeWidth={2.2} vectorEffect="non-scaling-stroke" />
      </svg>
      <div className="legend">
        <span><i style={{ background: 'var(--ink)' }} />{hex(fan.id)} speed <b className="mono">{last(rpm) ?? '—'} rpm</b> <span className="muted">(0–{Math.round(rmax / 1.08)})</span></span>
        <span><i style={{ background: fan.sensor === 6 ? 'var(--hue-purple-mark)' : 'var(--hue-red-mark)' }} />{sensorName(fan.sensor)} temperature <b className="mono">{last(temp) ?? '—'}°</b> <span className="muted">(30–105 °C)</span></span>
      </div>
    </div>
  );
}

/** How hard each fan works: average share of its reported max, and the time it spent flat out. */
export function Duty({ points, fans }: { points: Point[]; fans: Fan[] }) {
  return (
    <ul className="duty">
      {fans.map((f, i) => {
        const vals = points.map((p) => p.fans[i]).filter((v): v is number => v != null);
        const max = f.max_rpm || Math.max(1, ...vals);
        const avg = vals.length ? vals.reduce((a, b) => a + b, 0) / vals.length / max : 0;
        const full = vals.length ? vals.filter((v) => v >= max * 0.95).length / vals.length : 0;
        const zero = vals.length ? vals.filter((v) => v === 0).length / vals.length : 0;
        return (
          <li key={f.id}>
            <Tip text={`${hex(f.id)} follows the ${sensorName(f.sensor)}. Average ${Math.round(avg * 100)}% of its reported max (${max} rpm); at 95% or more for ${Math.round(full * 100)}% of the time; stopped ${Math.round(zero * 100)}% of the time.`}>
              <span className="mono">{hex(f.id)} <span className="muted">{sensorName(f.sensor)}</span></span>
            </Tip>
            <span className="d-bar"><i style={{ width: `${Math.min(100, avg * 100)}%` }} /><b style={{ left: `${Math.min(100, avg * 100)}%` }} /></span>
            <span className="mono d-val">{Math.round(avg * 100)}%</span>
            <span className="muted small">{zero > 0.9 ? 'stopped' : full > 0.5 ? 'mostly flat out' : `flat out ${Math.round(full * 100)}%`}</span>
          </li>
        );
      })}
    </ul>
  );
}
