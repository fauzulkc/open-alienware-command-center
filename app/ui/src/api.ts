// Calls into the app's Rust commands. In a plain browser (vite dev without Tauri) it answers from demo data,
// clearly marked, so the screens can be worked on without the laptop.

import { importAwcc, isHex, lightingProblem } from './logic';

export type Source = 'service' | 'direct' | 'demo' | 'unavailable';

export interface Sensor { id: number; name: string; celsius: number | null; stale: boolean }
export interface Fan { id: number; sensor: number | null; rpm: number | null; max_rpm: number | null }
export interface Alert { kind: string; fan?: number; rpm?: number; name?: string; sensor?: number; peak_c?: number }
export interface Snapshot { t_ms: number; sensors: Sensor[]; fans: Fan[]; profile: number | null; profile_name: string | null; alerts: Alert[] }
export interface Power { cpuPackageW: number | null; cpuCoresW: number | null; cpuPerformancePct: number | null }
export interface Status { source: Source | null; snapshot: Snapshot | null; lists: { profiles: string[]; fans: string[]; sensors: string[] } | null; power: Power | null; error: string | null; ageMs: number | null; energySaver?: boolean | null; elevated?: boolean }
export interface Proc { name: string; value: number; count: number }
export interface Top { cpu: Proc[]; ram: Proc[]; gpu: Proc[]; disk: Proc[] }
export interface Point { t: number; at: number; cpu_c: number | null; gpu_c: number | null; cpu_w: number | null; fans: (number | null)[]; profile: string | null; ram_gb: number; cpu_pct: number; gpu_pct: number | null; net_kbps: number | null; top: Top }
export interface Battery { design_mwh: number; full_mwh: number; wear_pct: number; cycles: number | null }
export interface Disk { name: string; wear_pct: number | null; temp_c: number | null; read_errors: number | null; power_on_hours: number | null }
export interface Health { battery: Battery | null; disks: Disk[]; events: { days: number; hardware_errors: number; crashes: number; unexpected_shutdowns: number } | null; unavailable: string[]; findings: string[] }
export interface Drift { service: string; expected: string[]; actual: string; running: boolean; severity: string; why: string }
export interface LightGroup { leds: number[]; effect: number; brightness: number; colors: string[] }
export interface LightDevice { device: string; preset: string; groups: LightGroup[]; power_states: string[] }
export interface LogSettings { enabled: boolean; everyS: number; keepDays: number; maxMb: number }
export interface Settings { demo: boolean; emergencyC: number; defaultProfile: string; boostKey: string; fanCurves: Record<string, [number, number][]>; gameModes: Record<string, string>; notify: boolean; theme: string; log: LogSettings; effects: Effects; liveAdmin: boolean; autostart?: boolean }
export interface Effects { motion: boolean; hover: boolean; press: boolean; largeText: boolean }
export interface ControlState { enabled: boolean; reasons: string[]; route?: string | null; last?: ModeChange | null; notes?: string[]; boosted?: Record<string, number> }
export interface BoostChange { fan: string; level: number; before: number | null; after: number | null; rpm_before: number | null; rpm_after: number | null; confirmed: boolean }
export interface LogDay { day: string; bytes: number }
export interface Controls { modes: ControlState; fans: ControlState; lighting: ControlState; emergency?: { active: boolean; triggerC: number | null; releaseC: number | null } }
export interface ModeChange { requested: string; before: string | null; after: string | null; confirmed: boolean; unchanged: boolean }

// ---------------------------------------------------------------- keyboard lighting (docs/lighting-control.md)
/** "#RRGGBB" */
export type Rgb = string;
export type KeyEffect = 'static' | 'breathing' | 'wave' | 'dualWave' | 'pulse' | 'mixPulse' | 'nightRider' | 'laser';
export interface KeyboardLight {
  /** false = backlight off (brightness 0), colours kept */
  on: boolean;
  /** 0..100 % */
  brightness: number;
  /** 'static' = per-key colours; the others are the keyboard's own (hardware) effects */
  effect: KeyEffect;
  /** 0..255, effects only */
  tempo: number;
  /** base colour for every key */
  color: Rgb;
  /** second colour for dualWave / mixPulse */
  color2: Rgb;
  /** group name (from `layout`) -> colour, over the base */
  groups: Record<string, Rgb>;
  /** key id -> colour, over groups */
  keys: Record<number, Rgb>;
}
/** The reactive glow: keys warm up as they are pressed (custom, static only). */
export interface ReactiveSettings {
  on: boolean;
  /** frames a second at most, 10..30 */
  fps: number;
  /** the fade's time constant, 200..3000 ms */
  decayMs: number;
  /** warmth a press adds to its neighbours, 0..1 */
  spread: number;
  /** how far keys near where you type lean warm, 0..0.5 */
  drift: number;
  /** the warm colour (intended; sent through ledPalette) */
  warm: Rgb;
  /** the hint at a press's peak (intended) */
  peak: Rgb;
}
/** A PC-driven animation the app keeps running (`lighting.live`). */
export type LiveKind = 'off' | 'clock' | 'mouse' | 'screen';
export type MouseLook = 'glow' | 'uncover' | 'reveal' | 'boost' | 'screen';
/** The mouse interaction: off, a white trail (glow), a screen-colour trail (screen), or uncover / reveal / boost. */
export type MouseMode = 'off' | MouseLook;
/** Key-press effects drawn over the base; any can be on together. */
export type TapKind = 'laser' | 'lightning' | 'seismic';
export interface LiveSettings {
  kind: LiveKind;
  /** mouse paint's look */
  look: MouseLook;
  /** mouse paint: a key fades back over this, 0..10000 ms (0 keeps the paint) */
  fadeMs: number;
  /** mouse paint, boost: the dimmed keys' brightness, 1..100 % */
  dim: number;
  /** mouse paint: writes a second, 1..30 */
  mouseFps: number;
  /** screen mirror: writes a second, 1..30 */
  screenFps: number;
  /** screen mirror: the ease's time constant, 0..10000 ms */
  smoothMs: number;
  /** screen mirror: how much the screen's areas keep their own colours, 0..1 */
  spread: number;
  /** screen mirror: colour depth, 1..4 */
  saturation: number;
  /** screen mirror: read the screen on the graphics card (else through GDI) */
  gpu: boolean;
  /** key-press effects over the base */
  tap: TapKind[];
  /** laser and lightning colour, #RRGGBB, or 'keys' for each key's own colour (always the keys' over the screen mirror) */
  tapColor: string;
  /** how fast they run, 5..80 keys a second */
  tapSpeed: number;
  /** writes a second while one runs, 1..30 */
  tapFps: number;
  /** the mouse interaction */
  mouse: MouseMode;
}
export interface LightingSettings {
  /** 'awcc': the app never writes (AWCC's look stays) */
  mode: 'awcc' | 'custom';
  keyboard: KeyboardLight;
  /** v4 light id -> colour (sent, not visible on this unit) */
  zones: Record<number, Rgb>;
  offOnSleep: boolean;
  offOnLidClose: boolean;
  offOnDisplayOff: boolean;
  /** the keyboard clock while the display is off (instead of dark); missing in older settings: off */
  clockOnDisplayOff?: boolean;
  dimOnBatterySaver: boolean;
  pauseAwcc: boolean;
  /** The keyboard LEDs' colour balance, % per channel: colours are corrected on the way to the keyboard. */
  ledBalance?: { r: number; g: number; b: number };
  /** Colours matched by eye on this keyboard: intended #RRGGBB -> the value sent so the key looks like it. */
  ledPalette?: Record<string, string>;
  /** The reactive glow (missing in older settings: off). */
  reactive?: ReactiveSettings;
  /** The live animation (missing in older settings: off). */
  live?: LiveSettings;
}
export interface KeyInfo { id: number; name: string; row: number; col: number; width: number; group?: string; present?: boolean }
export interface LightingStatus {
  settings: LightingSettings;
  layout: KeyInfo[];
  groups: string[];
  awcc: { preset: string; keys: Record<number, Rgb> } | null;
  device: { keyboard: boolean; zones: boolean; zonesVisible: false; error?: string };
  lastApply?: { ok: boolean; at: number; text: string };
}

const inApp = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<T>(cmd, args);
}

// ---------------------------------------------------------------- demo (browser only)
const demoProfile = { id: 0xa0 };
const NAMES: Record<number, string> = { 0xa3: 'Quiet', 0xa0: 'Balanced', 0xa1: 'Performance', 0xa4: 'Full Speed', 0xa5: 'Battery Saver', 0xab: 'G-Mode' };
let demoT = 0;
const demoHistory: Point[] = [];
function demoPoint(i: number): Point {
  const g = demoProfile.id === 0xab;
  const wave = Math.sin(i / 7) * 3 + Math.sin(i / 2.3);
  return {
    t: i * 1600, at: Date.now() - (120 - i) * 1600,
    cpu_c: Math.round(93 + wave), gpu_c: Math.round(66 + Math.sin(i / 9) * 4), cpu_w: 22 + Math.abs(Math.sin(i / 5)) * 9,
    fans: [g ? 4600 : 3700 + Math.round(Math.sin(i / 4) * 200), 800 + Math.round(Math.abs(Math.sin(i / 6)) * 600), 0, g ? 3000 : 2000],
    profile: NAMES[demoProfile.id], ram_gb: 21.4 + Math.sin(i / 11) * 0.6, cpu_pct: Math.round(14 + Math.abs(Math.sin(i / 5)) * 22), gpu_pct: Math.round(18 + Math.abs(Math.sin(i / 8)) * 30), net_kbps: Math.round(40 + Math.abs(Math.sin(i / 3)) * 400),
    top: {
      cpu: [{ name: 'claude', value: 9 + Math.abs(wave), count: 13 }, { name: 'msedgewebview2', value: 4.2, count: 12 }, { name: 'MsMpEng', value: 2.1, count: 1 }, { name: 'node', value: 1.4, count: 13 }],
      ram: [{ name: 'claude', value: 3091, count: 13 }, { name: 'vmmem', value: 2566, count: 1 }, { name: 'msedgewebview2', value: 631, count: 12 }],
      gpu: [{ name: 'dwm', value: 6, count: 1 }, { name: 'claude', value: 3, count: 2 }],
      disk: [{ name: 'MsMpEng', value: 820, count: 1 }, { name: 'SearchIndexer', value: 140, count: 1 }],
    },
  };
}
for (; demoT < 120; demoT++) demoHistory.push(demoPoint(demoT));
function demoSnap(): Snapshot {
  const p = demoHistory[demoHistory.length - 1];
  return { t_ms: p.t, sensors: [{ id: 1, name: 'CPU', celsius: p.cpu_c, stale: false }, { id: 6, name: 'GPU', celsius: p.gpu_c, stale: false }], fans: [{ id: 0x32, sensor: 1, rpm: p.fans[0], max_rpm: 4200 }, { id: 0x33, sensor: 6, rpm: p.fans[1], max_rpm: 4100 }, { id: 0x3b, sensor: 1, rpm: 0, max_rpm: 3000 }, { id: 0x3c, sensor: 6, rpm: p.fans[3], max_rpm: 3000 }], profile: demoProfile.id, profile_name: NAMES[demoProfile.id], alerts: [] };
}
let demoSettings: Settings = { demo: true, emergencyC: 98, defaultProfile: '0xA0', boostKey: 'awcc', fanCurves: {}, gameModes: {}, notify: true, theme: 'dark', log: { enabled: true, everyS: 30, keepDays: 14, maxMb: 200 }, liveAdmin: true, autostart: false, effects: { motion: false, hover: true, press: true, largeText: true } };
const demoBoost: Record<string, number> = {};
let demoLast: ModeChange | null = null;
/** CPU and GPU clocks now (the `clocks` command; like CPU-Z and GPU-Z). */
export interface CoreClock { label: string; efficient: boolean; threads: number[]; baseMhz: number; mhz: number }
export interface CpuClocks { cores: CoreClock[]; avgMhz: number; maxMhz: number }
export interface GpuClocks { name: string; coreMhz: number | null; memMhz: number | null; maxCoreMhz: number | null; maxMemMhz: number | null; pstate: number | null; powerW: number | null; loadPct: number | null; tempC: number | null; powerLimitW: number | null }
export interface BatteryNow { onAc: boolean; chargingW: number; dischargingW: number }
/** Where the power goes, W; null where nothing reports it. */
export interface PowerDraw {
  cpuPackageW: number | null; cpuCoresW: number | null; igpuW: number | null; dramW: number | null; cpuRestW: number | null;
  gpuW: number | null; gpuLimitW: number | null; measuredW: number; battery: BatteryNow | null;
  /** on battery: its discharge (the whole laptop); on AC: not reported */
  inputW: number | null; outputW: number | null;
  /** on battery: input less the measured parts (screen, SSD, fans, Wi-Fi...) */
  restW: number | null;
}
export interface Clocks { cpu: CpuClocks | null; gpu: GpuClocks | null; power: PowerDraw | null; cpuError: string | null; gpuError: string | null }

/** Demo clocks: the owner's i7-12700H (6 P-cores at 2.3 GHz rated, 8 E-cores at 1.7) and RTX 3070 Ti Laptop. */
function demoClocks(): Clocks {
  const t = Date.now() / 1000;
  const wob = (i: number, amp: number) => Math.sin(t * 0.7 + i * 1.3) * amp;
  const cores: CoreClock[] = [
    ...Array.from({ length: 6 }, (_, i) => ({ label: `P${i}`, efficient: false, threads: [i * 2, i * 2 + 1], baseMhz: 2300, mhz: 2650 + wob(i, 900) })),
    ...Array.from({ length: 8 }, (_, i) => ({ label: `E${i}`, efficient: true, threads: [12 + i], baseMhz: 1700, mhz: 1900 + wob(i + 6, 700) })),
  ];
  const avg = cores.reduce((a, c) => a + c.mhz, 0) / cores.length;
  const gpuW = 48 + wob(3, 12);
  const pkg = 38 + wob(5, 8);
  return {
    cpu: { cores, avgMhz: avg, maxMhz: Math.max(...cores.map((c) => c.mhz)) },
    gpu: { name: 'NVIDIA GeForce RTX 3070 Ti Laptop GPU', coreMhz: Math.round(1300 + wob(9, 400)), memMhz: 6001, maxCoreMhz: 1785, maxMemMhz: 7001, pstate: 3, powerW: gpuW, loadPct: Math.round(38 + wob(4, 25)), tempC: 57, powerLimitW: 132 },
    power: { cpuPackageW: pkg, cpuCoresW: pkg * 0.77, igpuW: null, dramW: null, cpuRestW: pkg * 0.23, gpuW, gpuLimitW: 132, measuredW: pkg + gpuW, battery: { onAc: true, chargingW: 0, dischargingW: 0 }, inputW: null, outputW: null, restW: null },
    cpuError: null,
    gpuError: null,
  };
}

const demo = {
  status: (): Status => {
    demoHistory.push(demoPoint(demoT++));
    demoHistory.shift();
    return { source: 'demo', snapshot: demoSnap(), lists: { profiles: ['0xA3', '0xA0', '0xA1', '0xA4', '0xA5', '0xAB'], fans: ['0x32', '0x33', '0x3B', '0x3C'], sensors: ['0x01', '0x06'] }, power: { cpuPackageW: demoHistory[demoHistory.length - 1].cpu_w, cpuCoresW: 16, cpuPerformancePct: 88 }, error: null, ageMs: 400, energySaver: false };
  },
  history: (after?: number): Point[] => demoHistory.filter((p) => after == null || p.t > after),
  health: (): Health => ({ battery: { design_mwh: 87005, full_mwh: 52201, wear_pct: 40, cycles: 98 }, disks: [], events: { days: 30, hardware_errors: 6, crashes: 0, unexpected_shutdowns: 0 }, unavailable: ['disk wear: access denied: Windows needs admin to read this'], findings: ['6 hardware error report(s) (WHEA) in the last 30 days.', 'The battery holds 40% less than when new (52201 of 87005 mWh).'] }),
  drift: () => ({ ok: false, drift: [{ service: 'DellTechHub', expected: ['Delayed'], actual: 'Auto', running: true, severity: 'high', why: 'Your rule: Automatic (Delayed Start).' }] as Drift[] }),
  lighting: () => ({ devices: [{ device: '0x0406_AdvCherryMechanicalKB', preset: 'System Default', groups: [{ leds: Array(19).fill(0), effect: 4, brightness: 100, colors: ['#FF00FF'] }, { leds: Array(12).fill(0), effect: 4, brightness: 100, colors: ['#2AAAFF'] }, { leds: Array(25).fill(0), effect: 4, brightness: 100, colors: ['#FFCDE9'] }, { leds: Array(10).fill(0), effect: 4, brightness: 100, colors: ['#6BFFB9'] }, { leds: Array(11).fill(0), effect: 4, brightness: 100, colors: ['#FFED87'] }], power_states: [] }, { device: '0x0406', preset: 'System Default', groups: [], power_states: ['DC - Sleep', 'DC - Working', 'DC - Low Battery', 'AC - Sleep', 'AC - Fully Charge', 'AC - Charging', 'Spectrum'] }] as LightDevice[], error: undefined as string | undefined }),
  controls: (): Controls => ({ modes: { enabled: true, reasons: ['Browser preview: switching changes the demo only.'], route: 'demo', last: demoLast }, fans: { enabled: true, reasons: [], notes: ['Browser preview: boost changes the demo only.'], boosted: { ...demoBoost } }, lighting: { enabled: true, reasons: [], notes: ['Browser preview: lighting changes the demo only.'] } }),
  setFanBoost: async (fans: string[], level: number): Promise<BoostChange[]> => {
    await new Promise((r) => setTimeout(r, 600));
    return fans.map((f) => { const before = demoBoost[f] ?? 0; if (level) demoBoost[f] = level; else delete demoBoost[f]; return { fan: f, level, before, after: level, rpm_before: 3700, rpm_after: Math.min(4600, 3700 + level * 9), confirmed: true }; });
  },
  logDays: () => ({ days: ['2026-11-09', '2026-11-08', '2026-11-07'].map((day, i) => ({ day, bytes: 2_400_000 - i * 300_000 })), totalBytes: 6_300_000, dir: '%APPDATA%\Alien Cockpit OSS\logs' }),
  setMode: async (p: string): Promise<ModeChange> => {
    await new Promise((r) => setTimeout(r, 700));
    const before = NAMES[demoProfile.id];
    demoProfile.id = parseInt(p, 16);
    demoLast = { requested: NAMES[demoProfile.id], before, after: NAMES[demoProfile.id], confirmed: true, unchanged: false };
    return demoLast;
  },
};

// ---------------------------------------------------------------- demo keyboard lighting (browser only)
// The x17 R2's 91 keys by AWCC's LED ids, laid out like the laptop. The real layout comes from `lighting_status`.
// From the owner's photo of the keyboard (2026-11-24): 84 lit keys, 16 key units per row; the arrows double as
// Page Up/Down; the right column is Del, Mic, Mute, Vol +, Vol −, →; the space bar (null) has no LED.
type Row = [id: number | null, name: string, width?: number][];
const DEMO_ROWS: Row[] = [
  [[0, 'ESC'], ...Array.from({ length: 12 }, (_, i) => [i + 1, `F${i + 1}`] as [number, string]), [13, 'Home'], [14, 'End'], [15, 'Delete']],
  [[20, '`'], ...'1234567890'.split('').map((c, i) => [21 + i, c] as [number, string]), [31, '-'], [32, '='], [35, 'Backspace', 2], [19, 'Microphone Mute']],
  [[40, 'Tab', 1.5], ...'QWERTYUIOP[]'.split('').map((c, i) => [42 + i, c] as [number, string]), [55, '|', 1.5], [16, 'Speaker Mute']],
  [[61, 'Caps Lock', 1.75], ...'ASDFGHJKL;\''.split('').map((c, i) => [62 + i, c] as [number, string]), [74, 'Enter', 2.25], [18, 'Volume Up']],
  [[81, 'Left Shift', 2.25], ...'ZXCVBNM,./'.split('').map((c, i) => [83 + i, c] as [number, string]), [94, 'Right Shift', 1.75], [114, 'Up / Page Up'], [17, 'Volume Down']],
  [[100, 'Left Ctrl', 1.25], [101, 'Fn'], [103, 'Left Windows'], [104, 'Left Alt'], [null, '', 5], [111, 'Right Alt'], [109, 'Windows lock'], [112, 'Right Ctrl', 1.75], [133, 'Left Arrow'], [134, 'Down / Page Down'], [135, 'Right Arrow']],
];
/** AWCC ids with no physical key on this keyboard: written with the rest, never drawn. */
const DEMO_ABSENT: [number, string][] = [[33, 'JP1'], [59, 'JP3'], [73, 'UK1'], [82, 'JP6'], [105, 'JP4'], [113, 'Page Up'], [115, 'Page Down']];
const DEMO_GROUP_OF: Record<number, string> = {
  ...Object.fromEntries(Array.from({ length: 12 }, (_, i) => [i + 1, 'Function Keys'])),
  ...Object.fromEntries(Array.from({ length: 10 }, (_, i) => [21 + i, 'Numbers'])),
  42: 'QWER', 44: 'QWER', 45: 'QWER', 43: 'WASD', 62: 'WASD', 63: 'WASD', 64: 'WASD',
};
export const DEMO_LAYOUT: KeyInfo[] = [
  ...DEMO_ROWS.flatMap((keys, row) => {
    let col = 0;
    const out: KeyInfo[] = [];
    for (const [id, name, width = 1] of keys) {
      if (id != null) out.push({ id, name, row, col, width, present: true, ...(DEMO_GROUP_OF[id] ? { group: DEMO_GROUP_OF[id] } : {}) });
      col += width;
    }
    return out;
  }),
  ...DEMO_ABSENT.map(([id, name]) => ({ id, name, row: 255, col: 0, width: 1, present: false })),
];
const range = (a: number, b: number) => Array.from({ length: b - a + 1 }, (_, i) => a + i);
/** AWCC's "System Default" preset on the owner's laptop, as colour groups by LED id. */
const DEMO_AWCC_GROUPS: [Rgb, number[]][] = [
  ['#FF00FF', [0, 13, 14, 15, 31, 32, 33, 35, 19, 46, 47, 59, 65, 66, 73, 82, 105, 113, 115]],
  ['#2AAAFF', [...range(1, 12), 42, 44, 45]],
  ['#3500FF', [43, 62, 63, 64]],
  ['#FFCDE9', [133, 134, 112, 92, 94, 114, 91, 70, 71, 72, 74, 90, 111, 69, 89, 68, 88, 67, 48, 49, 50, 51, 52, 53, 55]],
  ['#6BFFB9', [101, 103, 104, 100, 81, 83, 84, 85, 86, 87]],
  ['#6022FF', [135, 17, 18, 16]],
  ['#FF00FF', [61]],
  ['#FF0000', [40]],
  ['#FFED87', range(20, 30)],
  ['#0000FF', [109]],
];
export const DEMO_AWCC_KEYS: Record<number, Rgb> = Object.fromEntries(DEMO_AWCC_GROUPS.flatMap(([c, ids]) => ids.map((id) => [id, c])));
const DEMO_LIGHT_GROUPS = ['Function Keys', 'Numbers', 'QWER', 'WASD'];
const demoLightDefaults = (): LightingSettings => ({
  mode: 'awcc',
  keyboard: { on: true, brightness: 100, effect: 'static', tempo: 128, color: '#FFCDE9', color2: '#2AAAFF', groups: {}, keys: {} },
  zones: { 1: '#FF00FF', ...Object.fromEntries(range(10, 19).map((id) => [id, '#2AAAFF'])) },
  offOnSleep: true, offOnLidClose: true, offOnDisplayOff: true, dimOnBatterySaver: false, pauseAwcc: true,
});
let demoLight: LightingStatus = {
  settings: demoLightDefaults(),
  layout: DEMO_LAYOUT,
  groups: DEMO_LIGHT_GROUPS,
  awcc: { preset: 'System Default', keys: DEMO_AWCC_KEYS },
  device: { keyboard: true, zones: true, zonesVisible: false },
};
const copy = <T,>(x: T): T => JSON.parse(JSON.stringify(x));
const later = <T,>(f: () => T, ms = 140) => new Promise<T>((res, rej) => setTimeout(() => { try { res(f()); } catch (e) { rej(e); } }, ms));
const applied = (text: string) => ({ ok: true, at: Date.now(), text });
const demoLighting = {
  status: () => later(() => copy(demoLight), 60),
  set: (s: LightingSettings) => later(() => {
    const problem = lightingProblem(s, demoLight.layout, demoLight.groups);
    if (problem) throw problem; // the backend refuses with a plain error string
    demoLight = { ...demoLight, settings: copy(s), lastApply: s.mode === 'custom' ? applied('Demo: written to the keyboard.') : applied('Demo: saved; AWCC keeps the keyboard.') };
    return copy(demoLight);
  }),
  previewKey: (id: number, color: Rgb) => later(() => {
    if (!demoLight.layout.some((k) => k.id === id)) throw `No key with id ${id} on this keyboard.`;
    if (!isHex(color)) throw `"${color}" is not a #RRGGBB colour.`;
    return copy(demoLight);
  }, 20),
  restoreAwcc: () => later(() => {
    demoLight = { ...demoLight, settings: { ...demoLight.settings, mode: 'awcc' }, lastApply: applied(`Demo: AWCC's "${demoLight.awcc?.preset}" written back to all ${demoLight.layout.length} keys.`) };
    return copy(demoLight);
  }, 400),
  importAwcc: () => later(() => {
    if (!demoLight.awcc) throw "AWCC's lighting preset couldn't be read.";
    demoLight = { ...demoLight, settings: importAwcc(demoLight.settings, demoLight.awcc, demoLight.layout) };
    return copy(demoLight);
  }, 300),
};

export const api = {
  inApp,
  lightingStatus: () => (inApp ? call<LightingStatus>('lighting_status') : demoLighting.status()),
  lightingSet: (settings: LightingSettings) => (inApp ? call<LightingStatus>('lighting_set', { settings }) : demoLighting.set(settings)),
  lightingPreviewKey: (id: number, color: Rgb) => (inApp ? call<LightingStatus>('lighting_preview_key', { id, color }) : demoLighting.previewKey(id, color)),
  lightingRestoreAwcc: () => (inApp ? call<LightingStatus>('lighting_restore_awcc') : demoLighting.restoreAwcc()),
  lightingImportAwcc: () => (inApp ? call<LightingStatus>('lighting_import_awcc') : demoLighting.importAwcc()),
  status: () => (inApp ? call<Status>('status') : Promise.resolve(demo.status())),
  /** Chart points: all of them, or only those after `after` (the newest `t` the window already has). */
  history: (after?: number) => (inApp ? call<Point[]>('history', { after: after ?? null }) : Promise.resolve(demo.history(after))),
  health: () => (inApp ? call<Health>('health') : Promise.resolve(demo.health())),
  drift: () => (inApp ? call<{ ok: boolean; drift: Drift[] }>('drift') : Promise.resolve(demo.drift())),
  lighting: () => (inApp ? call<{ devices: LightDevice[]; error?: string }>('lighting') : Promise.resolve(demo.lighting())),
  settings: () => (inApp ? call<Settings>('settings_get') : Promise.resolve(demoSettings)),
  saveSettings: (s: Settings) => (inApp ? call<Settings>('settings_set', { settings: s }) : Promise.resolve((demoSettings = s))),
  controls: () => (inApp ? call<Controls>('controls') : Promise.resolve(demo.controls())),
  /** `gpu`: read the NVIDIA GPU too (it can wake it from sleep: only while a page shows it). */
  clocks: (gpu: boolean) => (inApp ? call<Clocks>('clocks', { gpu }) : Promise.resolve(demoClocks())),
  setMode: (profile: string) => (inApp ? call<ModeChange>('set_mode', { profile }) : demo.setMode(profile)),
  setFanBoost: (fans: string[], level: number) => (inApp ? call<BoostChange[]>('set_fan_boost', { fans, level }) : demo.setFanBoost(fans, level)),
  logDays: () => (inApp ? call<{ days: LogDay[]; totalBytes: number; dir: string }>('log_days') : Promise.resolve(demo.logDays())),
  logRead: (day: string) => (inApp ? call<Point[]>('log_read', { day }) : Promise.resolve(demoHistory.map((p, i) => ({ ...p, at: Date.parse(day + 'T08:00:00') + i * 30_000 })))),
  logCleanup: () => (inApp ? call<string[]>('log_cleanup') : Promise.resolve([] as string[])),
  restartElevated: () => (inApp ? call<void>('restart_elevated') : Promise.resolve()),
  openAlienwareUpdate: () => (inApp ? call<void>('open_alienware_update') : Promise.resolve()),
};

/** AWCC's names for the thermal profiles on this laptop. */
export const PROFILE_NAMES: Record<string, string> = { '0xA3': 'Quiet', '0xA0': 'Balanced', '0xA1': 'Performance', '0xA4': 'Full Speed', '0xA5': 'Battery Saver', '0xAB': 'G-Mode', '0x00': 'Custom' };
export const hex = (n: number) => '0x' + n.toString(16).toUpperCase().padStart(2, '0');
export const sensorName = (id: number | null) => (id === 1 ? 'CPU' : id === 6 ? 'GPU' : id == null ? '—' : `sensor ${hex(id)}`);
