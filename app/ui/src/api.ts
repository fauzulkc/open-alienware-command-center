// Calls into the app's Rust commands. In a plain browser (vite dev without Tauri) it answers from demo data,
// clearly marked, so the screens can be worked on without the laptop.

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
  controls: (): Controls => ({ modes: { enabled: true, reasons: ['Browser preview: switching changes the demo only.'], route: 'demo', last: demoLast }, fans: { enabled: true, reasons: [], notes: ['Browser preview: boost changes the demo only.'], boosted: { ...demoBoost } }, lighting: { enabled: false, reasons: ['Writing lighting needs the lighting protocol port and a test on this keyboard.'] } }),
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

export const api = {
  inApp,
  status: () => (inApp ? call<Status>('status') : Promise.resolve(demo.status())),
  /** Chart points: all of them, or only those after `after` (the newest `t` the window already has). */
  history: (after?: number) => (inApp ? call<Point[]>('history', { after: after ?? null }) : Promise.resolve(demo.history(after))),
  health: () => (inApp ? call<Health>('health') : Promise.resolve(demo.health())),
  drift: () => (inApp ? call<{ ok: boolean; drift: Drift[] }>('drift') : Promise.resolve(demo.drift())),
  lighting: () => (inApp ? call<{ devices: LightDevice[]; error?: string }>('lighting') : Promise.resolve(demo.lighting())),
  settings: () => (inApp ? call<Settings>('settings_get') : Promise.resolve(demoSettings)),
  saveSettings: (s: Settings) => (inApp ? call<Settings>('settings_set', { settings: s }) : Promise.resolve((demoSettings = s))),
  controls: () => (inApp ? call<Controls>('controls') : Promise.resolve(demo.controls())),
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
