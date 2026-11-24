import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DEMO_AWCC_KEYS, DEMO_LAYOUT, type Fan, type KeyboardLight, type KeyInfo, type LightingSettings, type ReactiveSettings, type Snapshot, type Status } from './api';
import {
  alertText, BLADES, composeKey, REACTIVE_DEFAULTS, reactiveOf, debounce, dimHex, drawnRevPerSec, EFFECTS, fanFlag, groupMembers, importAwcc, inkOn, isHex, keyAt, keysAlong, keysInRect, lightingProblem,
  MAX_DRAWN_REV_PER_S, MAX_FRAME_S, mergeHistory, mostCommon, normHex, palette, rotorPercent, shownColors, stoppedZones, stuckFans, toggleKey, verdict, zoneName,
} from './logic';

// The x17 R2 as the reader reports it: sensor 0x01 (CPU) with fans 0x32 and 0x3B, sensor 0x06 (GPU) with 0x33 and 0x3C.
const fan = (id: number, sensor: number, rpm: number | null, max = 4200): Fan => ({ id, sensor, rpm, max_rpm: max });
function snap(cpu: number, gpu: number, rpm: Record<number, number | null>): Snapshot {
  return {
    t_ms: 0,
    sensors: [{ id: 1, name: 'CPU', celsius: cpu, stale: false }, { id: 6, name: 'GPU', celsius: gpu, stale: false }],
    fans: [fan(0x32, 1, rpm[0x32] ?? 0), fan(0x33, 6, rpm[0x33] ?? 0), fan(0x3b, 1, rpm[0x3b] ?? 0), fan(0x3c, 6, rpm[0x3c] ?? 0)],
    profile: 0xab,
    profile_name: 'G-Mode',
    alerts: [],
  };
}
const status = (s: Snapshot | null, w: number | null = 60): Status => ({ source: 'direct', snapshot: s, lists: null, power: { cpuPackageW: w, cpuCoresW: null, cpuPerformancePct: null }, error: null, ageMs: 0 });

describe('the dead-fan checks', () => {
  it('finds a hot sensor whose fans all read 0 RPM, and only that', () => {
    expect(stoppedZones(snap(100, 63, { 0x33: 1300, 0x3c: 3000 }))).toEqual([{ sensor: 1, celsius: 100, fans: [0x32, 0x3b] }]);
    expect(stoppedZones(snap(100, 63, { 0x32: 4600, 0x33: 1300, 0x3c: 3000 })), 'one CPU fan spins').toEqual([]);
    expect(stoppedZones(snap(80, 63, {})), '80 °C is not hot yet').toEqual([]);
    expect(stoppedZones(snap(45, 45, {})), 'an idle laptop with fans off').toEqual([]);
    expect(stoppedZones(null)).toEqual([]);
  });

  it('makes both CPU fans stopped the first thing the cockpit says', () => {
    const v = verdict(status(snap(100, 63, { 0x33: 1300, 0x3c: 3000 })));
    expect(v.tone).toBe('hot');
    expect(v.text).toContain('Every CPU fan reads 0 RPM at 100 °C');
  });

  it('flags a fan by its own sensor, not the CPU', () => {
    const s = snap(100, 50, { 0x32: 4600, 0x33: 0, 0x3c: 0 });
    expect(fanFlag(s.fans[2], s, [])).toMatchObject({ flag: 'stopped' });
    expect(fanFlag(s.fans[2], s, []).note).toContain('CPU is at 100 °C');
    expect(fanFlag(s.fans[1], s, []), 'a GPU fan off on a cool GPU is normal, however hot the CPU').toEqual({});
    expect(fanFlag(s.fans[0], s, []), 'a spinning fan').toEqual({});
    const hotGpu = snap(60, 85, { 0x32: 2000, 0x3b: 2000 });
    expect(fanFlag(hotGpu.fans[1], hotGpu, []).flag).toBe('stopped');
  });

  it('marks a fan whose reading never moves, and nothing else', () => {
    const s = snap(70, 60, { 0x32: 3000, 0x3b: 3000, 0x33: 1200, 0x3c: 3000 });
    expect(fanFlag(s.fans[3], s, [0x3c]).flag).toBe('fixed');
    expect(fanFlag(s.fans[1], s, [0x3c])).toEqual({});
  });
});

describe('the stuck-reading check from history', () => {
  const fans = snap(70, 60, {}).fans;
  const hist = (n: number, f: (i: number) => (number | null)[]) => Array.from({ length: n }, (_, i) => ({ fans: f(i) }));
  it('finds 0x3C fixed at 3000 while its partner 0x33 moves', () => {
    expect(stuckFans(hist(40, (i) => [4600, 700 + (i % 9) * 100, 0, 3000]), fans)).toEqual([0x3c]);
  });
  it('stays quiet when the partner is steady too, for stopped fans, for short history and gaps', () => {
    expect(stuckFans(hist(40, () => [4600, 1000, 0, 3000]), fans), 'both steady proves nothing').toEqual([]);
    expect(stuckFans(hist(40, (i) => [0, 700 + (i % 9) * 100, 0, 0]), fans), '0 RPM is the dead-fan check’s case').toEqual([]);
    expect(stuckFans(hist(20, (i) => [4600, 700 + (i % 9) * 100, 0, 3000]), fans), 'not enough history').toEqual([]);
    expect(stuckFans(hist(40, (i) => [4600, 700 + (i % 9) * 100, 0, i === 5 ? null : 3000]), fans), 'a missing reading').toEqual([]);
  });
});

describe('the fan rotor', () => {
  it('names fans like AWCC: per sensor, in id order', () => {
    const fans = snap(50, 50, {}).fans;
    expect(fans.map((f) => zoneName(fans, f))).toEqual(['CPU zone 1', 'GPU zone 1', 'CPU zone 2', 'GPU zone 2']);
  });

  it('shows speed as a share of the reported maximum, capped at 100', () => {
    expect(rotorPercent(2100, 4200)).toBe(50);
    expect(rotorPercent(4600, 4200), 'fans run past their reported max in G-Mode').toBe(100);
    expect(rotorPercent(0, 4200)).toBe(0);
    expect(rotorPercent(null, 4200)).toBeNull();
    expect(rotorPercent(1000, null)).toBeNull();
  });

  it('draws faster for faster fans, never past the cap', () => {
    const speeds = [0, 500, 1000, 2000, 3000, 4600, 5000, 9000].map(drawnRevPerSec);
    expect(speeds[0]).toBe(0);
    for (let i = 1; i < speeds.length; i++) expect(speeds[i]).toBeGreaterThanOrEqual(speeds[i - 1]);
    expect(Math.max(...speeds)).toBeLessThanOrEqual(MAX_DRAWN_REV_PER_S);
    expect(drawnRevPerSec(null)).toBe(0);
    expect(drawnRevPerSec(-5)).toBe(0);
  });

  it('never strobes: one frame moves less than half a blade gap, even on a slow frame', () => {
    const halfGap = 360 / BLADES / 2;
    const worstFrame = MAX_DRAWN_REV_PER_S * 360 * MAX_FRAME_S;
    expect(worstFrame).toBeLessThan(halfGap);
  });
});

describe('alert wording', () => {
  it('has words for every alert the reader raises', () => {
    expect(alertText({ kind: 'fan-fault', fan: 0x3b })).toContain('0x3B');
    expect(alertText({ kind: 'stuck-reading', fan: 0x3c, rpm: 3000 })).toContain('3000 RPM');
    expect(alertText({ kind: 'zone-stopped', sensor: 1 })).toMatch(/^Every CPU fan reads 0 RPM/);
    expect(alertText({ kind: 'sensor-stale', name: 'GPU' })).toContain('GPU');
  });
});

describe('the chart history the window keeps', () => {
  const pts = (...ts: number[]) => ts.map((t) => ({ t }));
  it('appends only what is new and keeps the newest 600', () => {
    expect(mergeHistory(pts(1, 2), pts(3)).map((p) => p.t)).toEqual([1, 2, 3]);
    const full = pts(...Array.from({ length: 600 }, (_, i) => i + 1));
    const next = mergeHistory(full, pts(601, 602));
    expect(next.length).toBe(600);
    expect([next[0].t, next[599].t]).toEqual([3, 602]);
  });
  it('returns the same array when nothing came, so the charts do not redraw', () => {
    const prev = pts(1, 2);
    expect(mergeHistory(prev, [])).toBe(prev);
  });
  it('starts over when the app restarted its clock', () => {
    expect(mergeHistory(pts(5000, 6000), pts(100, 200)).map((p) => p.t)).toEqual([100, 200]);
  });
});

// ---------------------------------------------------------------- keyboard lighting
const kb = (over: Partial<KeyboardLight> = {}): KeyboardLight => ({ on: true, brightness: 100, effect: 'static', tempo: 128, color: '#112233', color2: '#000000', groups: {}, keys: {}, ...over });
const lightSettings = (over: Partial<LightingSettings> = {}): LightingSettings => ({ mode: 'custom', keyboard: kb(), zones: {}, offOnSleep: true, offOnLidClose: true, offOnDisplayOff: true, dimOnBatterySaver: false, pauseAwcc: true, ...over });
// Two rows: Q W E (QWER), A S (A in WASD), and a wide Shift.
const tiny: KeyInfo[] = [
  { id: 42, name: 'Q', row: 0, col: 0, width: 1, group: 'QWER' },
  { id: 43, name: 'W', row: 0, col: 1, width: 1, group: 'WASD' },
  { id: 44, name: 'E', row: 0, col: 2, width: 1, group: 'QWER' },
  { id: 62, name: 'A', row: 1, col: 0.25, width: 1, group: 'WASD' },
  { id: 63, name: 'S', row: 1, col: 1.25, width: 1, group: 'WASD' },
  { id: 81, name: 'Left Shift', row: 2, col: 0, width: 2.25 },
];
const GROUPS = ['Function Keys', 'Numbers', 'QWER', 'WASD'];

describe('the demo keyboard', () => {
  it('has the 91 keys once each, every one coloured by AWCC, in 6 rows of the same width', () => {
    expect(DEMO_LAYOUT.length).toBe(91);
    expect(new Set(DEMO_LAYOUT.map((k) => k.id)).size).toBe(91);
    expect(Object.keys(DEMO_AWCC_KEYS).length).toBe(91);
    for (const k of DEMO_LAYOUT) expect(DEMO_AWCC_KEYS[k.id], k.name).toBeDefined();
    const ends = [0, 1, 2, 3, 4, 5].map((r) => Math.max(...DEMO_LAYOUT.filter((k) => k.row === r).map((k) => k.col + k.width)));
    for (const e of ends) expect(e).toBeCloseTo(16);
  });
  it("gives AWCC's four groups their sizes, W in both QWER and WASD", () => {
    expect(['Function Keys', 'Numbers', 'QWER', 'WASD'].map((g) => groupMembers(DEMO_LAYOUT, g).length)).toEqual([12, 10, 4, 4]);
    expect(groupMembers(DEMO_LAYOUT, 'QWER')).toContain(43);
    expect(groupMembers(DEMO_LAYOUT, 'WASD')).toContain(43);
  });
});

describe('composition and dimming', () => {
  it('takes the key colour over the group colour over the base', () => {
    const k = kb({ groups: { QWER: '#00FF00' }, keys: { 44: '#FF0000' } });
    expect(composeKey(tiny[0], k), 'Q: the group').toBe('#00FF00');
    expect(composeKey(tiny[2], k), 'E: its own over the group').toBe('#FF0000');
    expect(composeKey(tiny[1], k), "W: WASD has no colour, so the base").toBe('#112233');
    expect(composeKey(tiny[5], k), 'Shift: no group').toBe('#112233');
  });
  it('dims each channel by brightness', () => {
    expect(dimHex('#FF8040', 100)).toBe('#FF8040');
    expect(dimHex('#FF8040', 50)).toBe('#804020');
    expect(dimHex('#FF8040', 0)).toBe('#000000');
    expect(dimHex('#ff8040', 150), 'clamped').toBe('#FF8040');
  });
  it('shows keys dimmed, dark when off, and the effect colour when an effect runs', () => {
    const k = kb({ keys: { 42: '#FF0000' }, brightness: 50 });
    expect(shownColors(tiny, k)[42]).toBe('#800000');
    expect(Object.values(shownColors(tiny, { ...k, on: false })).every((c) => c === '#000000')).toBe(true);
    expect(shownColors(tiny, { ...k, effect: 'wave', brightness: 100 })[42], 'effects override per-key colours').toBe('#112233');
  });
  it('picks dark labels on light keys and light labels on dark ones', () => {
    expect(inkOn('#FFFFFF')).toBe('dark');
    expect(inkOn('#FFCDE9')).toBe('dark');
    expect(inkOn('#000000')).toBe('light');
    expect(inkOn('#0000FF')).toBe('light');
  });
});

describe('colours as typed', () => {
  it('accepts only #RRGGBB', () => {
    expect(isHex('#A1b2C3')).toBe(true);
    for (const bad of ['A1B2C3', '#A1B2C', '#A1B2C3D', '#GGGGGG', 'red', '', null, 12]) expect(isHex(bad), String(bad)).toBe(false);
  });
  it('normalises what someone typed, or refuses it', () => {
    expect(normHex(' ff00aa ')).toBe('#FF00AA');
    expect(normHex('#ff00aa')).toBe('#FF00AA');
    expect(normHex('#f0a')).toBeNull();
    expect(normHex('pink')).toBeNull();
  });
});

describe('selecting keys', () => {
  it('finds the key under a point, including wide and offset keys, and nothing in a gap', () => {
    expect(keyAt(tiny, 1.5, 0.5)).toBe(43);
    expect(keyAt(tiny, 0.1, 1.5), 'A starts at 0.25').toBeNull();
    expect(keyAt(tiny, 2.2, 2.5), 'Shift is 2.25 wide').toBe(81);
    expect(keyAt(tiny, 9, 9)).toBeNull();
  });
  it('selects what a drag rectangle touches, whichever way it was drawn', () => {
    expect(keysInRect(tiny, { x: 0.5, y: 0.5 }, { x: 1.5, y: 1.5 }).sort()).toEqual([42, 43, 62, 63]);
    expect(keysInRect(tiny, { x: 1.5, y: 1.5 }, { x: 0.5, y: 0.5 }).sort(), 'reversed').toEqual([42, 43, 62, 63]);
    expect(keysInRect(tiny, { x: 2.5, y: 0.2 }, { x: 2.6, y: 0.3 }), 'a tiny box inside one key').toEqual([44]);
    expect(keysInRect(tiny, { x: 1, y: 0 }, { x: 1, y: 1 }), 'a zero-width line on a border touches nothing').toEqual([]);
  });
  it('paints every key a fast stroke crosses, once, in order', () => {
    expect(keysAlong(tiny, { x: 0.5, y: 0.5 }, { x: 2.5, y: 0.5 }), 'two pointer events, three keys').toEqual([42, 43, 44]);
    expect(keysAlong(tiny, { x: 2.5, y: 0.5 }, { x: 0.5, y: 0.5 })).toEqual([44, 43, 42]);
    expect(keysAlong(tiny, { x: 0.5, y: 0.5 }, { x: 0.5, y: 2.5 }), 'down a column: Q, A, Shift').toEqual([42, 62, 81]);
    expect(keysAlong(tiny, { x: 0.1, y: 1.2 }, { x: 0.1, y: 1.8 }), 'inside the gap left of A: nothing').toEqual([]);
    expect(keysAlong(tiny, { x: 1.5, y: 0.5 }, { x: 1.5, y: 0.5 }), 'a click').toEqual([43]);
  });
  it('toggles keys in and out with shift/ctrl-click', () => {
    expect(toggleKey([42], 43)).toEqual([42, 43]);
    expect(toggleKey([42, 43], 42)).toEqual([43]);
  });
  it('selects groups from the layout plus AWCC members the single group field cannot hold', () => {
    expect(groupMembers(tiny, 'QWER'), 'W is QWER too').toEqual([42, 43, 44]);
    expect(groupMembers(tiny, 'WASD')).toEqual([43, 62, 63]);
    expect(groupMembers(tiny, 'Numbers')).toEqual([]);
    expect(groupMembers([{ id: 7, name: 'X', row: 0, col: 0, width: 1, group: 'Custom' }], 'Custom')).toEqual([7]);
  });
});

describe('the write scheduler', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('sends only the newest value, once, after 250 ms of quiet', () => {
    const fn = vi.fn();
    const d = debounce(fn, 250);
    d.schedule(1);
    vi.advanceTimersByTime(200);
    d.schedule(2);
    vi.advanceTimersByTime(200);
    d.schedule(3);
    expect(fn).not.toHaveBeenCalled();
    expect(d.pending).toBe(true);
    vi.advanceTimersByTime(250);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith(3);
    expect(d.pending).toBe(false);
  });
  it('sends at once on release, and not again later', () => {
    const fn = vi.fn();
    const d = debounce(fn, 250);
    d.schedule('a');
    d.flush();
    expect(fn).toHaveBeenCalledWith('a');
    vi.advanceTimersByTime(1000);
    expect(fn).toHaveBeenCalledTimes(1);
    d.flush();
    expect(fn, 'nothing pending: nothing sent').toHaveBeenCalledTimes(1);
  });
  it('drops a cancelled value', () => {
    const fn = vi.fn();
    const d = debounce(fn, 250);
    d.schedule(1);
    d.cancel();
    vi.advanceTimersByTime(1000);
    expect(fn).not.toHaveBeenCalled();
  });
  it('with maxWait, still sends while changes keep coming (the live key preview)', () => {
    const fn = vi.fn();
    const d = debounce(fn, 40, 80);
    for (let t = 0; t < 200; t += 10) { d.schedule(t); vi.advanceTimersByTime(10); }
    expect(fn.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(fn.mock.calls.length).toBeLessThanOrEqual(3);
  });
});

describe('importing the AWCC look', () => {
  it('reproduces AWCC on every key with the commonest colour as the base', () => {
    const s = importAwcc(lightSettings({ keyboard: kb({ effect: 'wave', groups: { QWER: '#123456' }, keys: { 0: '#ABCDEF' }, brightness: 40 }) }), { keys: DEMO_AWCC_KEYS }, DEMO_LAYOUT);
    expect(s.keyboard.color).toBe('#FFCDE9');
    expect(s.keyboard.effect).toBe('static');
    expect(s.keyboard.groups).toEqual({});
    expect(s.keyboard.brightness, 'brightness stays').toBe(40);
    for (const k of DEMO_LAYOUT) expect(composeKey(k, s.keyboard), k.name).toBe(DEMO_AWCC_KEYS[k.id]);
    expect(Object.keys(s.keyboard.keys).length, 'only the keys that differ').toBe(91 - 25);
    expect(lightingProblem(s, DEMO_LAYOUT, GROUPS)).toBeNull();
  });
  it('skips keys AWCC has no valid colour for, and ids not on the keyboard', () => {
    const s = importAwcc(lightSettings(), { keys: { 42: '#ff0000', 43: '#FF0000', 44: 'oops', 999: '#00FF00' } }, tiny);
    expect(s.keyboard.color).toBe('#FF0000');
    expect(s.keyboard.keys).toEqual({});
  });
  it('builds a 12-colour palette starting with AWCC’s colours', () => {
    const p = palette(DEMO_AWCC_KEYS);
    expect(p.length).toBe(12);
    expect(new Set(p).size).toBe(12);
    expect(p.slice(0, 3), 'most used first').toEqual(['#FFCDE9', '#FF00FF', '#2AAAFF']);
    expect(palette(null).length).toBe(12);
    expect(mostCommon([])).toBeNull();
  });
});

describe('the backend’s checks, mirrored', () => {
  it('passes good settings and names the first problem in plain words', () => {
    expect(lightingProblem(lightSettings({ zones: { 1: '#FF0000', 19: '#00FF00' } }), tiny, GROUPS)).toBeNull();
    expect(lightingProblem(lightSettings({ keyboard: kb({ brightness: 101 }) }), tiny, GROUPS)).toMatch(/Brightness must be 0 to 100/);
    expect(lightingProblem(lightSettings({ keyboard: kb({ tempo: 256 }) }), tiny, GROUPS)).toMatch(/Tempo/);
    expect(lightingProblem(lightSettings({ keyboard: kb({ color: 'red' }) }), tiny, GROUPS)).toMatch(/base colour/);
    expect(lightingProblem(lightSettings({ keyboard: kb({ keys: { 34: '#FF0000' } }) }), tiny, GROUPS)).toMatch(/No key with id 34/);
    expect(lightingProblem(lightSettings({ keyboard: kb({ groups: { Arrows: '#FF0000' } }) }), tiny, GROUPS)).toMatch(/Unknown key group/);
    expect(lightingProblem(lightSettings({ zones: { 20: '#FF0000' } }), tiny, GROUPS)).toMatch(/0 to 19/);
  });
  it('checks the reactive glow like the backend, and older settings without it pass', () => {
    const rx = (over: Partial<ReactiveSettings>) => lightSettings({ reactive: { ...REACTIVE_DEFAULTS, ...over } });
    expect(lightingProblem(lightSettings(), tiny, GROUPS)).toBeNull();
    expect(lightingProblem(rx({ on: true }), tiny, GROUPS)).toBeNull();
    expect(lightingProblem(rx({ fps: 10 }), tiny, GROUPS)).toBeNull();
    expect(lightingProblem(rx({ fps: 31 }), tiny, GROUPS)).toMatch(/5 to 30 frames/);
    expect(lightingProblem(rx({ fps: 4 }), tiny, GROUPS)).toMatch(/5 to 30 frames/);
    expect(lightingProblem(rx({ decayMs: 100 }), tiny, GROUPS)).toMatch(/200 to 3000 ms/);
    expect(lightingProblem(rx({ spread: 2 }), tiny, GROUPS)).toMatch(/spread/);
    expect(lightingProblem(rx({ drift: 0.6 }), tiny, GROUPS)).toMatch(/drift/);
    expect(lightingProblem(rx({ warm: 'peach' }), tiny, GROUPS)).toMatch(/warm colour/);
    expect(reactiveOf(lightSettings())).toEqual(REACTIVE_DEFAULTS);
    expect(REACTIVE_DEFAULTS).toMatchObject({ on: false, fps: 18, decayMs: 700, warm: '#FF9480' });
  });
  it('words every effect, and only the two-colour ones use the second colour', () => {
    expect(EFFECTS.map((e) => e.id)).toEqual(['static', 'breathing', 'wave', 'dualWave', 'pulse', 'mixPulse', 'nightRider', 'laser']);
    expect(EFFECTS.filter((e) => e.color2).map((e) => e.id)).toEqual(['dualWave', 'mixPulse']);
    expect(EFFECTS.filter((e) => !e.tempo).map((e) => e.id)).toEqual(['static']);
  });
});

describe('the x17 R2 keyboard layout (owner photo, 2026-11-24)', () => {
  it('has 84 lit keys in 6 rows of 16 units, the space bar gap and 7 AWCC ids with no key', async () => {
    const { DEMO_LAYOUT } = await import('./api');
    const shown = DEMO_LAYOUT.filter((k) => k.present !== false);
    expect(shown.length).toBe(84);
    expect(DEMO_LAYOUT.filter((k) => k.present === false).map((k) => k.id).sort((a, b) => a - b)).toEqual([33, 59, 73, 82, 105, 113, 115]);
    for (let r = 0; r < 6; r++) {
      const row = shown.filter((k) => k.row === r);
      const width = row.reduce((s, k) => s + k.width, 0) + (r === 5 ? 5 : 0);
      expect(width, `row ${r}`).toBe(16);
      const sorted = [...row].sort((a, b) => a.col - b.col);
      for (let i = 1; i < sorted.length; i++) expect(sorted[i].col, `row ${r} overlap`).toBeGreaterThanOrEqual(sorted[i - 1].col + sorted[i - 1].width);
    }
    const at = (id: number) => shown.find((k) => k.id === id)!;
    expect([at(114).row, at(114).col]).toEqual([4, 14]);
    expect([at(134).row, at(134).col]).toEqual([5, 14]);
    expect([15, 19, 16, 18, 17, 135].map((id) => [at(id).row, at(id).col])).toEqual([[0, 15], [1, 15], [2, 15], [3, 15], [4, 15], [5, 15]]);
  });
});
