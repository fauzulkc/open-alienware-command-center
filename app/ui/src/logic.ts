// The screens' decisions, kept out of the components so they can be tested on their own (logic.test.ts).

import { hex, sensorName, type Alert, type Fan, type Snapshot, type Status } from './api';

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
