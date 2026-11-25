// The screens' decisions, kept out of the components so they can be tested on their own (logic.test.ts).

import { hex, sensorName, type Alert, type Fan, type KeyboardLight, type KeyEffect, type KeyInfo, type LightingSettings, type LiveKind, type LiveSettings, type MouseMode, type TapKind, type ReactiveSettings, type Rgb, type Snapshot, type Status } from './api';

/** Above this a sensor counts as hot for the fan checks (the reader's fan rules use the same 80 °C). */
export const HOT_C = 80;

export function alertText(a: Alert): string {
  if (a.kind === 'fan-fault') return `Fan ${hex(a.fan ?? 0)} stood still while its sensor was hot and its partner spun: likely a fan fault.`;
  if (a.kind === 'stuck-reading') return `Fan ${hex(a.fan ?? 0)} reports a speed that never changes (${a.rpm} RPM): its reading isn't a real measurement.`;
  if (a.kind === 'zone-stopped') return `Every ${sensorName(a.sensor ?? null)} fan reads 0 RPM while it is hot: nothing is cooling it. Ease the load and have the fans checked.`;
  if (a.kind === 'sensor-stale') return `No valid ${a.name} temperature for 30 s or more.`;
  return a.kind;
}

const tempOf = (s: Snapshot, sensor: number | null) => s.sensors.find((x) => x.id === sensor)?.celsius ?? null;

/** Sensors that are hot right now while every fan cooling them reads 0 RPM. Read from the live snapshot, so it
 * holds for as long as it's true (the reader's alert fires only on the tick the episode crosses 30 s). */
export function stoppedZones(s: Snapshot | null | undefined): { sensor: number; celsius: number; fans: number[] }[] {
  if (!s) return [];
  const bySensor = new Map<number, Fan[]>();
  for (const f of s.fans) if (f.sensor != null) bySensor.set(f.sensor, [...(bySensor.get(f.sensor) ?? []), f]);
  const out: { sensor: number; celsius: number; fans: number[] }[] = [];
  for (const [sensor, fans] of bySensor) {
    const c = tempOf(s, sensor);
    if (c != null && c > HOT_C && fans.every((f) => f.rpm === 0)) out.push({ sensor, celsius: c, fans: fans.map((f) => f.id) });
  }
  return out;
}

const sensorC = (s: Status | null, name: string) => s?.snapshot?.sensors.find((x) => x.name === name)?.celsius ?? null;

/** One plain sentence about the state of the machine, worst first. */
export function verdict(status: Status | null): { tone: 'hot' | 'warm' | 'ok' | 'muted'; text: string } {
  const cpu = sensorC(status, 'CPU');
  const w = status?.power?.cpuPackageW ?? null;
  if (!status?.snapshot) return { tone: 'muted', text: 'No live firmware readings yet. Windows-side numbers are below.' };
  const z = stoppedZones(status.snapshot)[0];
  if (z) return { tone: 'hot', text: `Every ${sensorName(z.sensor)} fan reads 0 RPM at ${z.celsius} °C: nothing is cooling the ${sensorName(z.sensor)}. Ease the load and have the fans checked.` };
  if (cpu != null && w != null && cpu >= 95 && w <= 35) return { tone: 'hot', text: `Running hot for the load: ${cpu} °C at only ${w.toFixed(0)} W points to cooling (a fan, clogged fins or dried paste).` };
  if (status.snapshot.alerts.length) return { tone: 'warm', text: alertText(status.snapshot.alerts[0]) };
  if (cpu != null && cpu >= 90) return { tone: 'warm', text: `Working hard: CPU at ${cpu} °C.` };
  return { tone: 'ok', text: 'All normal.' };
}

/** A fan rotor's warning: "stopped" when it reads 0 RPM while its own sensor is hot (a GPU fan idling at 0 on
 * a cool GPU is normal and gets nothing), "fixed" when the reader found its speed reading never moves. */
export function fanFlag(f: Fan, s: Snapshot, stuck: number[]): { flag?: 'stopped' | 'fixed'; note?: string } {
  const c = tempOf(s, f.sensor);
  if (f.rpm === 0 && c != null && c > HOT_C) return { flag: 'stopped', note: `${hex(f.id)} reads 0 RPM while the ${sensorName(f.sensor)} is at ${c} °C. Check this fan by eye and ear.` };
  if (stuck.includes(f.id)) return { flag: 'fixed', note: `${hex(f.id)} reports a speed that never changes (${f.rpm ?? '—'} RPM): it isn't a real measurement.` };
  return {};
}

/** The chart history the window keeps: new points appended, at most `cap` kept. A first point older than the
 * newest kept one means the app restarted its clock: start over from what came. Returns `prev` itself when
 * nothing changed, so React doesn't redraw the charts for nothing. */
export function mergeHistory<P extends { t: number }>(prev: P[], incoming: P[], cap = 600): P[] {
  if (incoming.length === 0) return prev;
  const last = prev[prev.length - 1]?.t;
  const base = last != null && incoming[0].t <= last ? [] : prev;
  return [...base, ...incoming].slice(-cap);
}

/** AWCC's names for the fans: CPU zone 1, CPU zone 2, GPU zone 1… numbered per sensor in id order. */
export function zoneName(fans: Fan[], f: Fan): string {
  const same = fans.filter((x) => x.sensor === f.sensor).map((x) => x.id).sort((a, b) => a - b);
  return `${sensorName(f.sensor)} zone ${same.indexOf(f.id) + 1}`;
}

/** The fan's speed as a share of the maximum it reports (AWCC's "zone speed"), 0–100, or null. */
export function rotorPercent(rpm: number | null, max: number | null): number | null {
  return rpm != null && max ? Math.min(100, Math.round((rpm / max) * 100)) : null;
}

/** Blades on the drawn rotor, and the drawn speed cap: with 11 blades a gap is 32.7°, and a rotor that turns
 * close to a whole gap per frame looks frozen or backwards (the wagon-wheel strobe). */
export const BLADES = 11;
export const MAX_DRAWN_REV_PER_S = 2;
/** The longest frame the animation advances by; a slower frame just shows less motion. */
export const MAX_FRAME_S = 1 / 48;

/** Drawn revolutions per second for a real speed: rises with RPM, capped, and never strobes. */
export function drawnRevPerSec(rpm: number | null): number {
  return MAX_DRAWN_REV_PER_S * Math.pow(Math.min(1, Math.max(0, rpm ?? 0) / 5000), 0.7);
}

/** Fans whose speed reading hasn't moved over the recent history while a partner fan on the same sensor
 * moved at least 300 RPM: the reader's "stuck reading" rule, from the chart history, so the flag holds while
 * it's true. `history[i].fans` is in the snapshot's fan order. Needs 30 readings or more. */
export function stuckFans(history: { fans: (number | null)[] }[], fans: Fan[], window = 60): number[] {
  const recent = history.slice(-window);
  if (recent.length < 30) return [];
  const series = fans.map((_, i) => recent.map((p) => p.fans[i] ?? null));
  return fans.filter((f, i) => {
    const mine = series[i];
    if (mine.some((v) => v == null) || mine[0] === 0 || mine.some((v) => v !== mine[0])) return false;
    return fans.some((g, j) => {
      if (j === i || g.sensor !== f.sensor) return false;
      const v = series[j].filter((x): x is number => x != null);
      return v.length > 0 && Math.max(...v) - Math.min(...v) >= 300;
    });
  }).map((f) => f.id);
}

// ---------------------------------------------------------------- keyboard lighting (docs/lighting-control.md)

const HEX_RE = /^#[0-9A-Fa-f]{6}$/;

/** A colour as the backend takes it: "#RRGGBB", nothing else. */
export function isHex(s: unknown): s is Rgb {
  return typeof s === 'string' && HEX_RE.test(s);
}

/** What someone typed, as "#RRGGBB" in capitals, or null: takes "ff00aa" and "#ff00aa", refuses "#f0a" and words. */
export function normHex(s: string): Rgb | null {
  const t = s.trim();
  const h = t.startsWith('#') ? t : `#${t}`;
  return isHex(h) ? h.toUpperCase() : null;
}

/** AWCC's four groups by LED id. A key can be in two (W is in QWER and in WASD), which `KeyInfo.group` can't say,
 * so the group buttons also take these members when the layout has those keys. */
export const AWCC_GROUP_KEYS: Record<string, number[]> = {
  'Function Keys': [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
  Numbers: [21, 22, 23, 24, 25, 26, 27, 28, 29, 30],
  QWER: [42, 43, 44, 45],
  WASD: [43, 62, 63, 64],
};

/** The keys a group button selects, in layout order. */
export function groupMembers(layout: KeyInfo[], name: string): number[] {
  const extra = AWCC_GROUP_KEYS[name] ?? [];
  return layout.filter((k) => k.group === name || extra.includes(k.id)).map((k) => k.id);
}

/** A key's colour: its own, else its group's, else the keyboard's base colour. */
export function composeKey(k: KeyInfo, kb: Pick<KeyboardLight, 'color' | 'groups' | 'keys'>): Rgb {
  return kb.keys[k.id] ?? (k.group != null ? kb.groups[k.group] : undefined) ?? kb.color;
}

/** A colour at a share of full brightness (0–100), as the eye sees a dimmed LED: each channel scaled. */
export function dimHex(c: Rgb, pct: number): Rgb {
  const f = Math.min(100, Math.max(0, pct)) / 100;
  const ch = (i: number) => Math.round(parseInt(c.slice(i, i + 2), 16) * f).toString(16).padStart(2, '0');
  return `#${ch(1)}${ch(3)}${ch(5)}`.toUpperCase();
}

/** What each key shows on the map: composed and dimmed by brightness; dark when off; with a hardware effect
 * every key shows the effect's colour, since the effect overrides per-key colours. */
export function shownColors(layout: KeyInfo[], kb: KeyboardLight): Record<number, Rgb> {
  const level = kb.on ? kb.brightness : 0;
  const out: Record<number, Rgb> = {};
  for (const k of layout) out[k.id] = dimHex(kb.effect === 'static' ? composeKey(k, kb) : kb.color, level);
  return out;
}

/** Whether a label on this fill reads better dark or light (WCAG relative luminance). */
export function inkOn(c: Rgb): 'dark' | 'light' {
  const lin = (i: number) => { const v = parseInt(c.slice(i, i + 2), 16) / 255; return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
  return 0.2126 * lin(1) + 0.7152 * lin(3) + 0.0722 * lin(5) > 0.33 ? 'dark' : 'light';
}

/** The key under a point given in key units (x = columns, y = rows). */
export function keyAt(layout: KeyInfo[], x: number, y: number): number | null {
  return layout.find((k) => y >= k.row && y < k.row + 1 && x >= k.col && x < k.col + k.width)?.id ?? null;
}

/** The keys a drag rectangle touches, corners in key units, either order. */
export function keysInRect(layout: KeyInfo[], a: { x: number; y: number }, b: { x: number; y: number }): number[] {
  const [x0, x1] = [Math.min(a.x, b.x), Math.max(a.x, b.x)];
  const [y0, y1] = [Math.min(a.y, b.y), Math.max(a.y, b.y)];
  return layout.filter((k) => k.col < x1 && k.col + k.width > x0 && k.row < y1 && k.row + 1 > y0).map((k) => k.id);
}

/** The keys a brush stroke crosses from `a` to `b` (key units), in order: pointer events come far apart on a
 * fast drag, so the segment is walked in small steps instead of taking only its ends. */
export function keysAlong(layout: KeyInfo[], a: { x: number; y: number }, b: { x: number; y: number }, step = 0.2): number[] {
  const n = Math.max(1, Math.ceil(Math.hypot(b.x - a.x, b.y - a.y) / step));
  const out: number[] = [];
  for (let i = 0; i <= n; i++) {
    const id = keyAt(layout, a.x + ((b.x - a.x) * i) / n, a.y + ((b.y - a.y) * i) / n);
    if (id != null && !out.includes(id)) out.push(id);
  }
  return out;
}

/** Shift/Ctrl-click: a key in the selection leaves it, any other joins it. */
export function toggleKey(sel: number[], id: number): number[] {
  return sel.includes(id) ? sel.filter((x) => x !== id) : [...sel, id];
}

/** Both lists, without repeats, in order. */
export function unionKeys<T>(a: T[], b: T[]): T[] {
  return [...new Set([...a, ...b])];
}

/** The colour the selection's editor starts from: the first selected key's; null for none. */
export function selectionColor(layout: KeyInfo[], kb: KeyboardLight, sel: number[]): Rgb | null {
  const cs = sel.map((id) => layout.find((k) => k.id === id)).filter((k): k is KeyInfo => !!k).map((k) => composeKey(k, kb));
  return cs[0] ?? null;
}

/** The keyboard's effects, worded plainly; which settings each one uses. */
export const EFFECTS: { id: KeyEffect; name: string; what: string; tempo: boolean; color2: boolean }[] = [
  { id: 'static', name: 'Static', what: 'Each key keeps its own colour.', tempo: false, color2: false },
  { id: 'breathing', name: 'Breathing', what: 'The colour slowly fades out and back in.', tempo: true, color2: false },
  { id: 'wave', name: 'Wave', what: 'The colour sweeps across the keyboard.', tempo: true, color2: false },
  { id: 'dualWave', name: 'Two-colour wave', what: 'Two colours sweep across the keyboard in turn.', tempo: true, color2: true },
  { id: 'pulse', name: 'Pulse', what: 'The colour flashes on and off.', tempo: true, color2: false },
  { id: 'mixPulse', name: 'Two-colour pulse', what: 'The keys flash between two colours.', tempo: true, color2: true },
  { id: 'nightRider', name: 'Back and forth', what: 'A band of light runs from side to side.', tempo: true, color2: false },
  { id: 'laser', name: 'Laser', what: 'A line of light runs across the keys.', tempo: true, color2: false },
];
export const effectInfo = (e: KeyEffect) => EFFECTS.find((x) => x.id === e) ?? EFFECTS[0];

/** The reactive glow's defaults, as the backend has them: off, 18 fps, a 0.7 s fade, the owner's peach and
 * coral. */
export const REACTIVE_DEFAULTS: ReactiveSettings = { on: false, fps: 18, decayMs: 700, spread: 0.35, drift: 0.12, warm: '#FF9480', peak: '#FF6F61' };
export const reactiveOf = (s: LightingSettings): ReactiveSettings => ({ ...REACTIVE_DEFAULTS, ...s.reactive });

/** The live animation's defaults, as the backend has them: off; the owner's picks (glow, 2 s trail, boost at
 * 70 %; the mirror at spread 1, colour depth 2x, a 0.3 s ease, read on the graphics card). */
export const LIVE_DEFAULTS: LiveSettings = {
  kind: 'off', look: 'glow', fadeMs: 2000, dim: 70, mouseFps: 10, screenFps: 8, smoothMs: 300, spread: 1, saturation: 2, gpu: true,
  tap: [], tapColor: '#FFFFFF', tapSpeed: 30, tapFps: 12, mouse: 'off',
};
/** The live settings with defaults; settings saved before the three layers (kind 'mouse' with its look) read
 * as no effect with that look as the mouse interaction. */
export const liveOf = (s: LightingSettings): LiveSettings => {
  const l = { ...LIVE_DEFAULTS, ...s.live };
  return l.kind === 'mouse' ? { ...l, kind: 'off', mouse: l.look } : l;
};

/** The effects the app draws (layer 2, with the keyboard's own effects), worded plainly. */
export const LIVE_KINDS: { id: LiveKind; name: string; what: string }[] = [
  { id: 'off', name: 'None', what: 'Your colours as they are.' },
  { id: 'clock', name: 'Clock', what: 'The time across the keyboard in big digits: the hour peach, the minutes cyan; for 5 seconds of every 15 the seconds in pink.' },
  { id: 'screen', name: 'Screen mirror', what: 'The keyboard glows with your screen\'s colours, as one soft gradient: an ambient light for what\'s on screen.' },
];
/** The mouse interactions (layer 3), worded plainly. The screen is mapped onto the keys: top-left is Esc,
 * bottom-right the arrows. */
export const MOUSE_MODES: { id: MouseMode; name: string; what: string }[] = [
  { id: 'off', name: 'Off', what: 'The mouse doesn\'t touch the keyboard.' },
  { id: 'glow', name: 'White trail', what: 'The key under the pointer lights white and fades back.' },
  { id: 'screen', name: 'Screen-colour trail', what: 'The key under the pointer takes the screen\'s colour there and fades back.' },
  { id: 'uncover', name: 'Uncover', what: 'The keyboard turns white; the pointer shows your colours where it goes. Over the clock or mirror it brings your colours through.' },
  { id: 'reveal', name: 'Reveal', what: 'The keyboard goes dark; the pointer brings your colours through. Over the clock or mirror it brings your colours through.' },
  { id: 'boost', name: 'Boost', what: 'Your colours dimmed; the pointer brings keys to full brightness. Over the clock or mirror it brings your colours through.' },
];

/** A bar's fill for `pct` used (the owner, 2026-11-25: "more red, less white; more utilization = better =
 * pinkish flame red"): a pale rose at nothing (40 % of the theme's red over `--bar-low`), the theme's red at
 * 60 %, then on to a pinkish flame red (`--flame`) at 100 %. */
export function usedColour(pct: number): string {
  const p = Math.max(0, Math.min(100, Number.isFinite(pct) ? pct : 0));
  if (p <= 60) return `color-mix(in oklab, var(--hue-red-mark) ${Math.round(40 + p)}%, var(--bar-low))`;
  return `color-mix(in oklab, var(--flame) ${Math.round(((p - 60) / 40) * 100)}%, var(--hue-red-mark))`;
}
/** High use glows a little (75 % and up). */
export const usedGlow = (pct: number) => (pct >= 75 ? `0 0 8px color-mix(in oklab, var(--flame) 55%, transparent)` : 'none');

/** The key-press effects, worded plainly. */
export const TAP_KINDS: { id: TapKind; name: string; what: string }[] = [
  { id: 'laser', name: 'Laser', what: 'A beam runs out both ways along the row from the key you press.' },
  { id: 'lightning', name: 'Lightning', what: 'Bolts come in from both ends of the row and strike the key you press.' },
  { id: 'seismic', name: 'Seismic', what: 'A ring erupts from the key you press across the whole keyboard, in that key\'s colour from your static colours.' },
];
/** The key-press effects with `t` switched on or off, in the standard order. */
export const toggleTap = (on: TapKind[], t: TapKind): TapKind[] => TAP_KINDS.map((k) => k.id).filter((id) => (id === t ? !on.includes(t) : on.includes(id)));
/** What live lighting will do with these settings, in a few words (null: nothing runs). */
export function liveSummary(l: LiveSettings): string | null {
  const effect = l.kind === 'off' || l.kind === 'mouse' ? null : LIVE_KINDS.find((k) => k.id === l.kind)?.name ?? null;
  const mouse = l.mouse === 'off' ? null : MOUSE_MODES.find((k) => k.id === l.mouse)?.name ?? null;
  const parts = [effect, ...l.tap.map((t) => TAP_KINDS.find((k) => k.id === t)?.name ?? t), mouse].filter((x): x is string => !!x);
  return parts.length ? parts.join(' + ') : null;
}

/** A scheduler for writes: `schedule` keeps only the newest value and runs `fn` once things have been quiet for
 * `ms` (or, with `maxWait`, at least that often while they keep changing); `flush` runs it now (on release);
 * `cancel` drops it. */
export function debounce<T>(fn: (v: T) => void, ms: number, maxWait?: number) {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let first: number | null = null;
  let value: { v: T } | null = null;
  const run = () => {
    if (timer) clearTimeout(timer);
    timer = null;
    first = null;
    const v = value;
    value = null;
    if (v) fn(v.v);
  };
  return {
    schedule(v: T) {
      value = { v };
      const now = Date.now();
      if (first == null) first = now;
      if (timer) clearTimeout(timer);
      const wait = maxWait != null ? Math.max(0, Math.min(ms, first + maxWait - now)) : ms;
      timer = setTimeout(run, wait);
    },
    flush: run,
    cancel() {
      if (timer) clearTimeout(timer);
      timer = null;
      first = null;
      value = null;
    },
    get pending() {
      return value != null;
    },
  };
}

/** The colour most keys have (the first one wins a tie), or null for none. */
export function mostCommon(colors: Rgb[]): Rgb | null {
  const n = new Map<Rgb, number>();
  let best: Rgb | null = null;
  for (const c of colors) {
    n.set(c, (n.get(c) ?? 0) + 1);
    if (best == null || n.get(c)! > n.get(best)!) best = c;
  }
  return best;
}

/** Custom settings seeded from AWCC's look, per key: the commonest colour becomes the base and only the keys that
 * differ keep their own, so the map looks exactly like AWCC's. Groups are cleared (they would sit between) and
 * the effect goes to Static; on/off, brightness, zones and the power rules stay as they were. */
export function importAwcc(s: LightingSettings, awcc: { keys: Record<number, Rgb> }, layout: KeyInfo[]): LightingSettings {
  const of = (id: number) => { const c = awcc.keys[id]; return typeof c === 'string' ? normHex(c) : null; };
  const base = mostCommon(layout.map((k) => of(k.id)).filter((c): c is Rgb => c != null)) ?? s.keyboard.color;
  const keys: Record<number, Rgb> = {};
  for (const k of layout) {
    const c = of(k.id);
    if (c && c !== base) keys[k.id] = c;
  }
  return { ...s, keyboard: { ...s.keyboard, effect: 'static', color: base, groups: {}, keys } };
}

/** The colour palette next to the picker: the colours of AWCC's look, most used first, then common ones; 12. */
export function palette(awcc: Record<number, Rgb> | null | undefined, size = 12): Rgb[] {
  const fill = ['#FFFFFF', '#FF0000', '#FF8000', '#FFFF00', '#00FF00', '#00FFFF', '#0000FF', '#8000FF', '#FF00FF', '#FF0080', '#80FF00', '#0080FF'];
  const n = new Map<Rgb, number>();
  for (const c of Object.values(awcc ?? {})) { const h = normHex(c); if (h) n.set(h, (n.get(h) ?? 0) + 1); }
  const fromAwcc = [...n.entries()].sort((a, b) => b[1] - a[1]).map(([c]) => c);
  return unionKeys(fromAwcc, fill).slice(0, size);
}

/** The same checks the backend makes, in words, or null when the settings are fine. */
export function lightingProblem(s: LightingSettings, layout: KeyInfo[], groups: string[]): string | null {
  const kb = s.keyboard;
  if (s.mode !== 'awcc' && s.mode !== 'custom') return `Unknown mode "${s.mode}".`;
  if (!Number.isFinite(kb.brightness) || kb.brightness < 0 || kb.brightness > 100) return `Brightness must be 0 to 100, not ${kb.brightness}.`;
  if (!Number.isInteger(kb.tempo) || kb.tempo < 0 || kb.tempo > 255) return `Tempo must be 0 to 255, not ${kb.tempo}.`;
  if (!EFFECTS.some((e) => e.id === kb.effect)) return `Unknown effect "${kb.effect}".`;
  for (const [what, c] of [['base colour', kb.color], ['second colour', kb.color2]] as const) if (!isHex(c)) return `The ${what} "${c}" is not a #RRGGBB colour.`;
  for (const [g, c] of Object.entries(kb.groups)) {
    if (!groups.includes(g)) return `Unknown key group "${g}".`;
    if (!isHex(c)) return `The colour for ${g} "${c}" is not a #RRGGBB colour.`;
  }
  const ids = new Set(layout.map((k) => k.id));
  for (const [id, c] of Object.entries(kb.keys)) {
    if (!ids.has(Number(id))) return `No key with id ${id} on this keyboard.`;
    if (!isHex(c)) return `The colour for key ${id} "${c}" is not a #RRGGBB colour.`;
  }
  for (const [id, c] of Object.entries(s.zones)) {
    const n = Number(id);
    if (!Number.isInteger(n) || n < 0 || n > 19) return `Zone light ids are 0 to 19, not ${id}.`;
    if (!isHex(c)) return `The colour for light ${id} "${c}" is not a #RRGGBB colour.`;
  }
  if (s.reactive) {
    const r = reactiveOf(s);
    if (!Number.isInteger(r.fps) || r.fps < 5 || r.fps > 30) return `The reactive glow runs at 5 to 30 frames a second, not ${r.fps}.`;
    if (!Number.isInteger(r.decayMs) || r.decayMs < 200 || r.decayMs > 3000) return `The glow's fade must be 200 to 3000 ms, not ${r.decayMs}.`;
    if (!(r.spread >= 0 && r.spread <= 1)) return `The glow's spread must be 0 to 1, not ${r.spread}.`;
    if (!(r.drift >= 0 && r.drift <= 0.5)) return `The glow's drift must be 0 to 0.5, not ${r.drift}.`;
    for (const [what, c] of [['warm colour', r.warm], ['peak colour', r.peak]] as const) if (!isHex(c)) return `The glow's ${what} "${c}" is not a #RRGGBB colour.`;
  }
  if (s.live) {
    const l = liveOf(s);
    if (!LIVE_KINDS.some((k) => k.id === l.kind)) return `Unknown effect "${l.kind}".`;
    if (!MOUSE_MODES.some((k) => k.id === l.mouse)) return `Unknown mouse interaction "${l.mouse}".`;
    if (!Number.isInteger(l.fadeMs) || l.fadeMs < 0 || l.fadeMs > 10000) return `The trail's fade must be 0 to 10000 ms, not ${l.fadeMs}.`;
    if (!Number.isInteger(l.dim) || l.dim < 1 || l.dim > 100) return `Boost's dimmed keys must be 1 to 100 %, not ${l.dim}.`;
    for (const [what, v] of [['Mouse paint', l.mouseFps], ['The screen mirror', l.screenFps]] as const) if (!Number.isInteger(v) || v < 1 || v > 30) return `${what} writes 1 to 30 times a second, not ${v}.`;
    if (!Number.isInteger(l.smoothMs) || l.smoothMs < 0 || l.smoothMs > 10000) return `The mirror's ease must be 0 to 10000 ms, not ${l.smoothMs}.`;
    if (!(l.spread >= 0 && l.spread <= 1)) return `The mirror's variety must be 0 to 1, not ${l.spread}.`;
    if (!(l.saturation >= 1 && l.saturation <= 4)) return `The mirror's colour depth must be 1 to 4, not ${l.saturation}.`;
    for (const t of l.tap) if (!TAP_KINDS.some((k) => k.id === t)) return `Unknown key-press effect "${t}".`;
    if (l.tapColor !== 'keys' && !isHex(l.tapColor)) return `The key-press colour "${l.tapColor}" is not a #RRGGBB colour.`;
    if (!Number.isInteger(l.tapSpeed) || l.tapSpeed < 5 || l.tapSpeed > 80) return `Key-press effects run at 5 to 80 keys a second, not ${l.tapSpeed}.`;
    if (!Number.isInteger(l.tapFps) || l.tapFps < 1 || l.tapFps > 30) return `Key-press effects write 1 to 30 times a second, not ${l.tapFps}.`;

  }
  return null;
}

/** Names for the zone controller's lights (docs/lighting-and-macros-parity.md). */
export function zoneLabel(id: number): string {
  if (id === 0) return 'Power button';
  if (id === 1) return 'Alien head';
  if (id >= 10 && id <= 14) return `Left ${id - 9}`;
  if (id >= 15 && id <= 19) return `Right ${id - 14}`;
  return `Light ${id}`;
}
