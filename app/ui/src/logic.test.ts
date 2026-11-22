import { describe, expect, it } from 'vitest';
import type { Fan, Snapshot, Status } from './api';
import { alertText, BLADES, drawnRevPerSec, fanFlag, MAX_DRAWN_REV_PER_S, MAX_FRAME_S, rotorPercent, stoppedZones, stuckFans, verdict, zoneName } from './logic';

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
