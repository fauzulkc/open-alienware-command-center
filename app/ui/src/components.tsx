import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import type { Point } from './api';
import { drawnRevPerSec, MAX_FRAME_S, rotorPercent } from './logic';

// ---------------------------------------------------------------- small pieces

/** A hover read-out: plain words about what a number is and where it comes from. */
export function Tip({ text, children, wide }: { text: string; children: ReactNode; wide?: boolean }) {
  return (
    <span className={wide ? 'tip wide-tip' : 'tip'} data-tip={text} tabIndex={0}>
      {children}
    </span>
  );
}

export function Section({ title, children, right, className }: { title: string; children: ReactNode; right?: ReactNode; className?: string }) {
  return (
    <section className={`panel ${className ?? ''}`}>
      <header className="panel-h">
        <span className="lbl">{title}</span>
        {right}
      </header>
      {children}
    </section>
  );
}

export function LockedWhy({ reasons }: { reasons: string[] }) {
  if (reasons.length === 0) return null;
  return (
    <div className="locked-why">
      {reasons.map((r) => <p key={r}>{r}</p>)}
    </div>
  );
}

/** Alien Cockpit's mark: a cockpit visor (its own, not Wiregent's or Dell's). */
export function Mark() {
  return (
    <svg viewBox="0 0 32 32" aria-hidden="true" className="mark">
      <rect width="32" height="32" rx="7.5" fill="var(--ink)" />
      <path d="M8 18.5c0-5.2 3.6-9 8-9s8 3.8 8 9c0 1.6-1.3 2.5-2.8 2.1L16 19.3l-5.2 1.3C9.3 21 8 20.1 8 18.5Z" fill="var(--canvas)" />
      <path d="M11.2 16.4c1.3-.9 3-1.4 4.8-1.4s3.5.5 4.8 1.4" fill="none" stroke="var(--pos-signal)" strokeWidth="1.6" strokeLinecap="round" />
    </svg>
  );
}

// ---------------------------------------------------------------- gauge

const tone = (c: number | null | undefined) => (c == null ? 'var(--faint)' : c >= 95 ? 'var(--neg-major)' : c >= 85 ? 'var(--caution-major)' : 'var(--pos-major)');

/** A 240° arc gauge from `min` to `max`, with soft zone marks at 85 and 95. */
export function Gauge({ label, value, min = 30, max = 105, unit = '°C', sub, tip }: { label: string; value: number | null; min?: number; max?: number; unit?: string; sub?: ReactNode; tip: string }) {
  const R = 66, C = 80, A0 = 150, SPAN = 240;
  const f = value == null ? 0 : Math.min(1, Math.max(0, (value - min) / (max - min)));
  const pt = (a: number, r = R) => [C + r * Math.cos((a * Math.PI) / 180), C + r * Math.sin((a * Math.PI) / 180)];
  const arc = (from: number, to: number, r = R) => {
    const [x0, y0] = pt(from, r), [x1, y1] = pt(to, r);
    return `M${x0} ${y0} A${r} ${r} 0 ${to - from > 180 ? 1 : 0} 1 ${x1} ${y1}`;
  };
  const zone = (t: number) => A0 + ((t - min) / (max - min)) * SPAN;
  return (
    <Tip text={tip}>
      <div className="gauge">
        <svg viewBox="0 0 160 140">
          <path d={arc(A0, A0 + SPAN)} className="g-track" />
          <path d={arc(zone(85), zone(95), R + 9)} className="g-zone warm" />
          <path d={arc(zone(95), A0 + SPAN, R + 9)} className="g-zone hot" />
          {value != null && <path d={arc(A0, A0 + Math.max(0.5, f * SPAN))} className="g-fill" style={{ stroke: tone(value) }} />}
        </svg>
        <div className="g-read">
          <span className="g-val" style={{ color: tone(value) }}>{value ?? '—'}<small>{unit}</small></span>
          <span className="lbl">{label}</span>
          {sub && <span className="g-sub">{sub}</span>}
        </div>
      </div>
    </Tip>
  );
}

// ---------------------------------------------------------------- smooth chart with hover

export interface Series { name: string; color: string; values: (number | null)[]; unit: string }

/** Monotone cubic (Fritsch–Carlson): smooth, never overshoots the readings. Gaps break the line. */
function smoothPath(xs: number[], ys: (number | null)[]): string {
  let d = '';
  let run: [number, number][] = [];
  const flush = () => {
    if (run.length === 1) d += `M${run[0][0]},${run[0][1]}`;
    if (run.length > 1) {
      const n = run.length, dx: number[] = [], m: number[] = [], t: number[] = new Array(n);
      for (let i = 0; i < n - 1; i++) { dx[i] = run[i + 1][0] - run[i][0]; m[i] = (run[i + 1][1] - run[i][1]) / (dx[i] || 1); }
      t[0] = m[0]; t[n - 1] = m[n - 2];
      for (let i = 1; i < n - 1; i++) t[i] = m[i - 1] * m[i] <= 0 ? 0 : (3 * (dx[i - 1] + dx[i])) / ((2 * dx[i] + dx[i - 1]) / m[i - 1] + (dx[i] + 2 * dx[i - 1]) / m[i]);
      d += `M${run[0][0]},${run[0][1]}`;
      for (let i = 0; i < n - 1; i++) {
        const h = dx[i] / 3;
        d += `C${run[i][0] + h},${run[i][1] + h * t[i]} ${run[i + 1][0] - h},${run[i + 1][1] - h * t[i + 1]} ${run[i + 1][0]},${run[i + 1][1]}`;
      }
    }
    run = [];
  };
  ys.forEach((y, i) => (y == null ? flush() : run.push([xs[i], y])));
  flush();
  return d;
}

/** Area chart with a hover crosshair. `onHover` gets the index of the moment under the pointer (null on leave). */
export function Chart({ series, points, min, max, height = 180, onHover, hover }: { series: Series[]; points: Point[]; min?: number; max?: number; height?: number; onHover?: (i: number | null) => void; hover?: number | null }) {
  const W = 800, H = height, PAD = 6;
  const ref = useRef<SVGSVGElement>(null);
  const all = series.flatMap((s) => s.values.filter((v): v is number => v != null));
  const lo = min ?? Math.max(0, Math.min(...all, Infinity) - 5);
  const hi = max ?? Math.max(...all, lo + 1) * 1.08;
  const n = Math.max(2, points.length);
  const xs = useMemo(() => Array.from({ length: n }, (_, i) => (i / (n - 1)) * W), [n]);
  const y = (v: number) => PAD + (H - 2 * PAD) * (1 - (v - lo) / (hi - lo || 1));
  const move = (e: React.PointerEvent) => {
    const r = ref.current?.getBoundingClientRect();
    if (!r || !onHover) return;
    onHover(Math.max(0, Math.min(n - 1, Math.round(((e.clientX - r.left) / r.width) * (n - 1)))));
  };
  const hx = hover != null ? xs[hover] : null;
  const p = hover != null ? points[hover] : null;
  return (
    <div className="chart">
      <svg ref={ref} viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" onPointerMove={move} onPointerLeave={() => onHover?.(null)}>
        {[0.25, 0.5, 0.75].map((f) => <line key={f} x1={0} x2={W} y1={H * f} y2={H * f} className="grid-l" />)}
        {series.map((s, i) => {
          // Readings arrive in whole degrees and RPM steps; a centred 5-point average draws the trend without
          // the staircase. The hover read-out and legend keep the exact readings.
          const eased = s.values.map((v, k) => {
            if (v == null) return null;
            const win = s.values.slice(Math.max(0, k - 2), k + 3).filter((x): x is number => x != null);
            return win.reduce((a, b) => a + b, 0) / win.length;
          });
          const line = smoothPath(xs, eased.map((v) => (v == null ? null : y(v))));
          const first = s.values.findIndex((v) => v != null);
          const last = s.values.length - 1 - [...s.values].reverse().findIndex((v) => v != null);
          const area = first >= 0 ? `${line}L${xs[last]},${H}L${xs[first]},${H}Z` : '';
          return (
            <g key={s.name}>
              {i === 0 && area && <path d={area} style={{ fill: s.color }} fillOpacity={0.035} />}
              <path d={line} fill="none" style={{ stroke: s.color }} strokeWidth={2} vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
            </g>
          );
        })}
        {hx != null && <line x1={hx} x2={hx} y1={0} y2={H} className="cross" vectorEffect="non-scaling-stroke" />}
        {hx != null && series.map((s) => (s.values[hover!] != null ? <circle key={s.name} cx={hx} cy={y(s.values[hover!]!)} r={4} style={{ fill: s.color }} className="dot-pt" /> : null))}
      </svg>
      {p && hx != null && (
        <div className="chart-tip" style={{ left: `${(hx / W) * 100}%` }}>
          <span className="lbl">{new Date(p.at).toLocaleTimeString()} {p.profile ? `· ${p.profile}` : ''}</span>
          {series.map((s) => <span key={s.name}><i style={{ background: s.color }} />{s.name} <b className="mono">{s.values[hover!] != null ? `${Math.round(s.values[hover!]!)}${s.unit}` : '—'}</b></span>)}
          {p.top.cpu[0] && <span className="muted">Busiest: {p.top.cpu[0].name} {p.top.cpu[0].value.toFixed(1)}%</span>}
        </div>
      )}
      <div className="legend">
        {series.map((s) => {
          const last = [...s.values].reverse().find((v) => v != null);
          return <span key={s.name}><i style={{ background: s.color }} />{s.name} <b className="mono">{last != null ? `${Math.round(last)}${s.unit}` : '—'}</b></span>;
        })}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- fan rotor with inertia

/**
 * A fan drawn live: its target speed is the measured RPM; the rotor spins up and coasts down with inertia
 * (time constants like a real laptop fan's), so changes look physical. A screen refreshes ~60 times a second
 * and a fan at 4000 RPM turns ~67 times a second, so the drawn speed is scaled (it would strobe otherwise);
 * blur ghosts grow with speed. The number under it is the real reading.
 */
export function FanRotor({ rpm, max, label, id, note, flag }: { rpm: number | null; max: number | null; label: string; id?: string; note?: string; flag?: 'stopped' | 'fixed' }) {
  const g = useRef<SVGGElement>(null);
  const ghosts = useRef<SVGGElement>(null);
  const target = useRef(0);
  target.current = rpm ?? 0;
  useEffect(() => {
    let raf = 0, last = performance.now(), angle = 0, omega = 0;
    const step = (now: number) => {
      const dt = Math.min(MAX_FRAME_S, (now - last) / 1000);
      last = now;
      // Drawn revolutions per second, capped at 2: with 11 blades a gap is 32.7°, and at 60 frames a second a
      // rotor turning near one gap per frame looks frozen (the wagon-wheel strobe). 4600 RPM at the old scale
      // was 34.5° a frame and read as stopped; 2 rev/s is 12° a frame, clearly turning. Speed shows in the blur.
      const goal = drawnRevPerSec(target.current);
      const tau = goal > omega ? 0.9 : 2.6; // spins up faster than it coasts down
      omega += (goal - omega) * (1 - Math.exp(-dt / tau));
      angle = (angle + omega * 360 * dt) % 360;
      g.current?.setAttribute('transform', `rotate(${angle} 50 50)`);
      if (ghosts.current) {
        ghosts.current.setAttribute('transform', `rotate(${angle - Math.min(16, omega * 8)} 50 50)`);
        ghosts.current.style.opacity = String(Math.min(0.45, omega * 0.22));
      }
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  }, []);
  const blades = Array.from({ length: 11 }, (_, i) => i * (360 / 11));
  const blade = 'M50 50 C52 40 58 31 67 27 C63 34 58 42 54 51 Z';
  const pct = rotorPercent(rpm, max);
  return (
    <Tip text={note ?? `${label}${id ? ` (fan ${id})` : ''}: ${rpm ?? '—'} RPM measured${max ? `, ${pct ?? 0}% of the ${max} RPM it reports as its maximum (AWCC's "zone speed")` : ''}.`}>
      <div className={`rotor ${flag ?? ''}`}>
        <svg viewBox="0 0 100 100">
          <circle cx="50" cy="50" r="46" className="r-ring" />
          {pct != null && <circle cx="50" cy="50" r="46" className="r-arc" pathLength={100} strokeDasharray={`${pct} 100`} transform="rotate(-90 50 50)" />}
          <circle cx="50" cy="50" r="40" className="r-well" />
          <g ref={ghosts} className="r-ghost">{blades.map((a) => <path key={a} d={blade} transform={`rotate(${a} 50 50)`} />)}</g>
          <g ref={g} className="r-blades">{blades.map((a) => <path key={a} d={blade} transform={`rotate(${a} 50 50)`} />)}</g>
          <circle cx="50" cy="50" r="9" className="r-hub" />
        </svg>
        <span className="r-pct mono">{pct != null ? `${pct}%` : '—'}</span>
        <span className="r-rpm mono">{rpm ?? '—'}<small> rpm</small></span>
        <span className="lbl">{label}{id && <span className="r-id"> · {id}</span>}</span>
        {flag && <span className={`r-flag ${flag}`}>{flag === 'stopped' ? 'not spinning' : 'reading fixed'}</span>}
      </div>
    </Tip>
  );
}

// ---------------------------------------------------------------- mode dock

export const MODES: { id: string; name: string; blurb: string }[] = [
  { id: '0xA3', name: 'Quiet', blurb: 'Fans kept low; the firmware limits power to stay quiet.' },
  { id: '0xA0', name: 'Balanced', blurb: 'The everyday mode.' },
  { id: '0xA1', name: 'Performance', blurb: 'More power; fans ramp sooner.' },
  { id: '0xA4', name: 'Full Speed', blurb: 'Fans run flat out.' },
  { id: '0xA5', name: 'Battery Saver', blurb: 'Lowest power, longest battery.' },
];

export function ModeDock({ current, enabled, pending, onPick, available }: { current: string | null; enabled: boolean; pending: string | null; onPick: (id: string) => void; available: string[] }) {
  const g = current === '0xAB';
  return (
    <div className="dock">
      <div className="seg" role="radiogroup" aria-label="Thermal mode">
        {MODES.filter((m) => available.includes(m.id)).map((m) => (
          <Tip key={m.id} text={m.blurb}>
            <button role="radio" aria-checked={current === m.id} className={`${current === m.id ? 'on' : ''} ${pending === m.id ? 'pending' : ''}`} disabled={!enabled || pending != null} onClick={() => onPick(m.id)}>
              {m.name}
            </button>
          </Tip>
        ))}
      </div>
      {available.includes('0xAB') && (
        <Tip text="Maximum power and fans. The boost key (Fn+F1) toggles it the same way.">
          <button className={`gmode ${g ? 'on' : ''} ${pending === '0xAB' ? 'pending' : ''}`} disabled={!enabled || pending != null} onClick={() => onPick(g ? '__back' : '0xAB')}>
            <span className="gm-dot" />G-Mode
          </button>
        </Tip>
      )}
    </div>
  );
}

/** Fan-curve editor: drag points (temperature → boost %). Saved as a setting; applying waits for fan control. */
export function CurveEditor({ points, onChange }: { points: [number, number][]; onChange: (p: [number, number][]) => void }) {
  const W = 420, H = 180, T0 = 20, T1 = 105;
  const ref = useRef<SVGSVGElement>(null);
  const [drag, setDrag] = useState<number | null>(null);
  const x = (t: number) => ((t - T0) / (T1 - T0)) * W;
  const y = (b: number) => H - (b / 100) * H;
  const move = (e: React.PointerEvent) => {
    if (drag == null || !ref.current) return;
    const r = ref.current.getBoundingClientRect();
    const t = Math.round(T0 + ((e.clientX - r.left) / r.width) * (T1 - T0));
    const b = Math.round(100 - ((e.clientY - r.top) / r.height) * 100);
    const lo = drag > 0 ? points[drag - 1][0] + 1 : T0;
    const hi = drag < points.length - 1 ? points[drag + 1][0] - 1 : T1;
    onChange(points.map((p, i) => (i === drag ? ([Math.min(hi, Math.max(lo, t)), Math.min(100, Math.max(0, b))] as [number, number]) : p)));
  };
  const path = smoothPath(points.map((p) => x(p[0])), points.map((p) => y(p[1])));
  return (
    <svg ref={ref} className="curve" viewBox={`-30 -10 ${W + 40} ${H + 30}`} onPointerMove={move} onPointerUp={() => setDrag(null)} onPointerLeave={() => setDrag(null)}>
      {[40, 60, 80, 100].map((t) => <g key={t}><line x1={x(t)} x2={x(t)} y1={0} y2={H} className="grid-l" /><text x={x(t)} y={H + 16} className="axis" textAnchor="middle">{t}°</text></g>)}
      {[0, 50, 100].map((b) => <text key={b} x={-8} y={y(b) + 4} className="axis" textAnchor="end">{b}%</text>)}
      <path d={path} className="curve-line" />
      {points.map((p, i) => (
        <circle key={i} cx={x(p[0])} cy={y(p[1])} r={6.5} className="curve-pt" onPointerDown={(e) => { (e.target as Element).setPointerCapture(e.pointerId); setDrag(i); }}>
          <title>{`${p[0]} °C → ${p[1]} %`}</title>
        </circle>
      ))}
    </svg>
  );
}

// ---------------------------------------------------------------- power and load on one chart

export interface LoadSeries { key: string; name: string; color: string; unit: string; lo: number; hi: number; values: (number | null)[]; dashed?: boolean; digits?: number }

/**
 * Several measures on one time axis, each drawn on its own scale (its range is in the legend), so their
 * rises and falls line up: power against CPU load, GPU load, RAM and temperature. Hover shows every real
 * value at that moment and the busiest program then. Click a legend entry to hide or show a line.
 */
export function LoadChart({ series, points, height = 210, hover, onHover }: { series: LoadSeries[]; points: Point[]; height?: number; hover?: number | null; onHover?: (i: number | null) => void }) {
  const W = 800, H = height, PAD = 8;
  const ref = useRef<SVGSVGElement>(null);
  const [hidden, setHidden] = useState<Record<string, boolean>>({});
  const n = Math.max(2, points.length);
  const xs = useMemo(() => Array.from({ length: n }, (_, i) => (i / (n - 1)) * W), [n]);
  const y = (s: LoadSeries, v: number) => PAD + (H - 2 * PAD) * (1 - Math.min(1, Math.max(0, (v - s.lo) / (s.hi - s.lo || 1))));
  const move = (e: React.PointerEvent) => {
    const r = ref.current?.getBoundingClientRect();
    if (!r || !onHover) return;
    onHover(Math.max(0, Math.min(n - 1, Math.round(((e.clientX - r.left) / r.width) * (n - 1)))));
  };
  const hx = hover != null ? xs[hover] : null;
  const p = hover != null ? points[hover] : null;
  const fmt = (s: LoadSeries, v: number | null | undefined) => (v == null ? '—' : `${v.toFixed(s.digits ?? 0)}${s.unit}`);
  return (
    <div className="chart">
      <svg ref={ref} viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" onPointerMove={move} onPointerLeave={() => onHover?.(null)}>
        {[0.25, 0.5, 0.75].map((f) => <line key={f} x1={0} x2={W} y1={H * f} y2={H * f} className="grid-l" />)}
        {series.filter((s) => !hidden[s.key]).map((s) => {
          const eased = s.values.map((v, k) => {
            if (v == null) return null;
            const win = s.values.slice(Math.max(0, k - 2), k + 3).filter((q): q is number => q != null);
            return win.reduce((a, b) => a + b, 0) / win.length;
          });
          return <path key={s.key} d={smoothPath(xs, eased.map((v) => (v == null ? null : y(s, v))))} fill="none" style={{ stroke: s.color }} strokeWidth={s.dashed ? 1.6 : 2.2} strokeDasharray={s.dashed ? '5 4' : undefined} vectorEffect="non-scaling-stroke" strokeLinejoin="round" />;
        })}
        {hx != null && <line x1={hx} x2={hx} y1={0} y2={H} className="cross" vectorEffect="non-scaling-stroke" />}
        {hx != null && series.filter((s) => !hidden[s.key] && s.values[hover!] != null).map((s) => <circle key={s.key} cx={hx} cy={y(s, s.values[hover!]!)} r={4} style={{ fill: s.color }} className="dot-pt" />)}
      </svg>
      {p && hx != null && (
        <div className="chart-tip" style={{ left: `${(hx / W) * 100}%` }}>
          <span className="lbl">{new Date(p.at).toLocaleTimeString()} {p.profile ? `· ${p.profile}` : ''}</span>
          {series.filter((s) => !hidden[s.key]).map((s) => <span key={s.key}><i style={{ background: s.color }} />{s.name} <b className="mono">{fmt(s, s.values[hover!])}</b></span>)}
          {p.top.cpu[0] && <span className="muted">Busiest: {p.top.cpu[0].name} {p.top.cpu[0].value.toFixed(1)}% CPU</span>}
        </div>
      )}
      <div className="legend toggles">
        {series.map((s) => {
          const last = [...s.values].reverse().find((v) => v != null);
          return (
            <button key={s.key} className={hidden[s.key] ? 'off' : ''} onClick={() => setHidden({ ...hidden, [s.key]: !hidden[s.key] })} title={`${s.name}: scale ${s.lo}–${s.hi}${s.unit}. Click to ${hidden[s.key] ? 'show' : 'hide'}.`}>
              <i style={{ background: s.color }} />{s.name} <b className="mono">{fmt(s, last)}</b> <span className="muted">{s.lo}–{s.hi}{s.unit}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
