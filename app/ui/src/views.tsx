import { useEffect, useMemo, useState } from 'react';
import type { Page } from './App';
import { api, hex, PROFILE_NAMES, sensorName, type BoostChange, type Clocks, type Controls, type Drift, type Fan, type Health, type LightingStatus, type LogDay, type ModeChange, type Point, type Proc, type Settings, type Status } from './api';
import { Chart, CurveEditor, FanRotor, Gauge, LoadChart, LockedWhy, ModeDock, Section, Tip, type LoadSeries } from './components';
import { Duty, FanResponse, SpeedAndTemp } from './fangraphs';
import { KeyboardMap } from './lighting';
import { alertText, fanFlag, fanLayout, liveOf, liveSummary, modeChanges, shownColors, stuckFans, usedColour, usedGlow, verdict, zoneName } from './logic';

/** Power and what it powers, on one time axis: each on its own scale. */
function loadSeries(h: Point[]): LoadSeries[] {
  return [
    { key: 'w', name: 'CPU power', color: 'var(--hue-amber-mark)', unit: ' W', lo: 0, hi: 120, digits: 1, values: h.map((p) => p.cpu_w) },
    { key: 'cpu', name: 'CPU load', color: 'var(--hue-red-mark)', unit: '%', lo: 0, hi: 100, values: h.map((p) => p.cpu_pct ?? null) },
    { key: 'gpu', name: 'GPU load', color: 'var(--hue-purple-mark)', unit: '%', lo: 0, hi: 100, values: h.map((p) => p.gpu_pct) },
    { key: 'ram', name: 'RAM', color: 'var(--hue-sage-mark)', unit: ' GB', lo: 0, hi: 32, digits: 1, values: h.map((p) => p.ram_gb) },
    { key: 'temp', name: 'CPU temp', color: 'var(--muted)', unit: '°', lo: 30, hi: 105, dashed: true, values: h.map((p) => p.cpu_c) },
  ];
}

const COL = { cpu: 'var(--hue-red-mark)', gpu: 'var(--hue-purple-mark)', power: 'var(--hue-amber-mark)', ram: 'var(--hue-sage-mark)', fans: ['var(--ink)', 'var(--hue-purple-mark)', 'var(--hue-red-mark)', 'var(--hue-sage-mark)'] };

const sensorC = (s: Status | null, name: string) => s?.snapshot?.sensors.find((x) => x.name === name)?.celsius ?? null;

/** The CPU and GPU clocks, read once a second while the page using this is open (and only then: reading the
 * NVIDIA GPU can wake it from sleep). */
function useClocks(): Clocks | null {
  const [c, setC] = useState<Clocks | null>(null);
  useEffect(() => {
    let live = true;
    // Skipped while the window is hidden or minimised (nothing to show, and the NVIDIA GPU can stay asleep).
    const tick = () => { if (!document.hidden) api.clocks(true).then((v) => { if (live) setC(v); }, () => {}); };
    tick();
    const id = setInterval(tick, 1000);
    return () => { live = false; clearInterval(id); };
  }, []);
  return c;
}

const ghz = (mhz: number | null | undefined) => (mhz == null ? '—' : `${(mhz / 1000).toFixed(2)} GHz`);
const mhzText = (mhz: number | null | undefined) => (mhz == null ? '—' : `${Math.round(mhz)} MHz`);

/** One clock as a bar against `of`, coloured by how much of it is used. */
function ClockBar({ label, mhz, of, note, hint }: { label: string; mhz: number | null; of: number; note?: string; hint?: string }) {
  const pct = mhz == null || of <= 0 ? 0 : Math.min(100, (mhz / of) * 100);
  return (
    <div className="clk-row" title={hint}>
      <span className="clk-lbl">{label}</span>
      <span className="clk-bar"><i style={{ transform: barScale(pct), background: usedColour(pct), boxShadow: usedGlow(pct) }} /></span>
      <b className="mono">{note ?? ghz(mhz)}</b>
    </div>
  );
}

/** A bar's length as a transform (animated by the compositor, no repaint): `pct` of the track, clamped. */
const barScale = (pct: number) => `scaleX(${Math.max(0, Math.min(100, Number.isFinite(pct) ? pct : 0)) / 100})`;

const watts = (w: number | null | undefined) => (w == null ? '—' : `${w.toFixed(1)} W`);

/** Where the power goes: each measured part as its share of the total, the input against the output. */
function PowerPanel({ c }: { c: Clocks | null }) {
  const p = c?.power;
  if (!p) return <p className="muted small">{c ? 'No power readings.' : 'Reading…'}</p>;  const total = p.inputW ?? p.measuredW;
  const row = (label: string, w: number | null, hint: string, note?: string, sub = false) => w == null ? null : (
    <div className={`clk-row pw-row ${sub ? 'pw-sub' : ''}`} title={hint}>
      <span className="clk-lbl">{label}</span>
      <span className="clk-bar"><i style={{ transform: barScale(total > 0 ? (w / total) * 100 : 0), background: usedColour(total > 0 ? (w / total) * 100 : 0), boxShadow: usedGlow(total > 0 ? (w / total) * 100 : 0) }} /></span>
      <b className="mono">{watts(w)}{note && <small className="muted"> {note}</small>}</b>
    </div>
  );
  const b = p.battery;
  return (
    <div className="clk-grid">
      <div>
        <div className="clk-head">
          <span className="lbl">In</span>
          {p.inputW != null
            ? <span><b className="mono">{watts(p.inputW)}</b> <small className="muted">from the battery</small></span>
            : <span><b className="mono">{b?.onAc ? 'AC' : '—'}</b> <small className="muted">{b?.onAc ? 'the adapter\'s power isn\'t reported by Windows' : 'no battery reading'}</small></span>}
          <span className="lbl">Out</span>
          <span><b className="mono">{watts(p.outputW ?? p.measuredW)}</b> <small className="muted">{p.outputW != null ? 'the whole laptop' : 'measured parts'}</small></span>
        </div>
        {row('CPU', p.cpuPackageW, 'The whole CPU package, from its own energy meter.', total > 0 && p.cpuPackageW != null ? `${Math.round((p.cpuPackageW / total) * 100)}%` : undefined)}
        {row('cores', p.cpuCoresW, 'The CPU cores.', undefined, true)}
        {row('graphics', p.igpuW, 'Intel\'s built-in graphics.', undefined, true)}
        {row('rest of chip', p.cpuRestW, 'Caches, the memory controller and I/O: the package less the cores and graphics.', undefined, true)}
        {row('Memory', p.dramW, 'Memory, as the CPU measures it.')}
        {row('GPU', p.gpuW, 'The NVIDIA GPU, from its driver.', p.gpuLimitW != null ? `of ${Math.round(p.gpuLimitW)} W limit` : undefined)}
        {row('Not measured', p.restW, 'The rest of the laptop: screen, SSD, fans, Wi-Fi, memory and the board. On battery it is the battery\'s draw less the measured parts.')}
        {b && b.chargingW > 0 && row('Charging', b.chargingW, 'Power going into the battery.')}
      </div>
      <div>
        <p className="small">{p.inputW != null
          ? `On battery: ${watts(p.inputW)} comes in and goes out to the parts below. ${watts(p.measuredW)} of it is measured directly; the remaining ${watts(p.restW)} goes to parts nothing measures.`
          : `On AC: the parts that report their power draw ${watts(p.measuredW)} together. Windows doesn't report what the adapter delivers, so the whole laptop's input shows on battery only: unplug to see it.`}</p>
        <p className="muted small">Bars show each part's share of {p.inputW != null ? 'the input' : 'the measured total'}, a pale rose for a little, red, and a pinkish flame for a lot. The CPU's figures come from its own energy meters (no driver); the GPU's from NVIDIA's driver; the battery's from Windows.</p>
      </div>
    </div>
  );
}

/** The CPU-Z / GPU-Z panel: every core's clock, then the GPU's. */
function ClocksPanel({ c }: { c: Clocks | null }) {
  const cpu = c?.cpu;
  const gpu = c?.gpu;
  // Bars against 5 GHz for the cores (the turbo limit isn't something Windows reports).
  const SCALE = 5000;
  return (
    <div className="clk-grid">
      <div>
        <div className="clk-head">
          <span className="lbl">CPU</span>
          <span><b className="mono">{ghz(cpu?.avgMhz)}</b> <small className="muted">average</small></span>
          <span><b className="mono">{ghz(cpu?.maxMhz)}</b> <small className="muted">fastest core</small></span>
        </div>
        {cpu ? cpu.cores.map((k) => (
          <ClockBar key={k.label} label={k.label} mhz={k.mhz} of={SCALE} hint={`${k.efficient ? 'Efficient' : 'Performance'} core, threads ${k.threads.join(', ')}, rated ${(k.baseMhz / 1000).toFixed(1)} GHz`} />
        )) : <p className="muted small">{c?.cpuError ?? 'Reading…'}</p>}
        {cpu && <p className="muted small">Each core's running clock, measured by the CPU itself (turbo included), averaged over the last second, like Task Manager's Speed. P are performance cores, E efficient ones; bars against 5 GHz.</p>}
      </div>
      <div>
        <div className="clk-head">
          <span className="lbl">GPU</span>
          <span><b className="mono">{mhzText(gpu?.coreMhz)}</b> <small className="muted">core</small></span>
          <span><b className="mono">{gpu?.pstate != null ? `P${gpu.pstate}` : '—'}</b> <small className="muted">state</small></span>
        </div>
        {gpu ? (
          <>
            <p className="small">{gpu.name}</p>
            <ClockBar label="Core" mhz={gpu.coreMhz} of={gpu.maxCoreMhz ?? 2000} note={`${mhzText(gpu.coreMhz)} of ${mhzText(gpu.maxCoreMhz)}`} />
            <ClockBar label="Memory" mhz={gpu.memMhz} of={gpu.maxMemMhz ?? 8000} note={`${mhzText(gpu.memMhz)} of ${mhzText(gpu.maxMemMhz)}`} />
            <div className="m-totals clk-more">
              <span>Power <b className="mono">{gpu.powerW != null ? `${gpu.powerW.toFixed(1)} W` : '—'}</b></span>
              <span>Load <b className="mono">{gpu.loadPct != null ? `${gpu.loadPct}%` : '—'}</b></span>
              <span>Temperature <b className="mono">{gpu.tempC != null ? `${gpu.tempC} °C` : '—'}</b></span>
            </div>
            <p className="muted small">From NVIDIA's driver, as nvidia-smi and GPU-Z read it. P0 is full speed; higher states are slower and save power. Intel's built-in graphics doesn't report its clock.</p>
          </>
        ) : <p className="muted small">{c?.gpuError ?? 'Reading…'}</p>}
      </div>
    </div>
  );
}

/** Mode switching shared by Overview and Performance: one request at a time, result shown in plain words. */
export function useModes(status: Status | null, controls: Controls | null) {
  const [pending, setPending] = useState<string | null>(null);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const current = status?.snapshot?.profile != null ? hex(status.snapshot.profile) : null;
  const pick = async (id: string) => {
    const target = id === '__back' ? (controls?.modes.last?.before && Object.entries(PROFILE_NAMES).find(([, n]) => n === controls.modes.last!.before)?.[0]) || '0xA0' : id;
    setPending(target);
    setResult(null);
    try {
      const c: ModeChange = await api.setMode(target);
      setResult(c.confirmed ? { ok: true, text: c.unchanged ? `Already ${c.after}.` : `Switched to ${c.after}.` } : { ok: false, text: `Asked for ${c.requested}; the firmware still reports ${c.after ?? 'nothing'}. AWCC may have switched it back.` });
    } catch (e) {
      setResult({ ok: false, text: String(e) });
    } finally {
      setPending(null);
    }
  };
  return { current, pending, result, pick, enabled: controls?.modes.enabled ?? false, available: status?.lists?.profiles ?? MODES_DEFAULT };
}
const MODES_DEFAULT = ['0xA3', '0xA0', '0xA1', '0xA4', '0xA5', '0xAB'];

function Moment({ p, live, className = 'moment' }: { p: Point | null; live: boolean; className?: string }) {
  const [tab, setTab] = useState<'cpu' | 'ram' | 'gpu' | 'disk'>('cpu');
  const fmt = (x: Proc) => (tab === 'cpu' || tab === 'gpu' ? `${x.value.toFixed(1)}%` : tab === 'ram' ? `${x.value >= 1024 ? (x.value / 1024).toFixed(1) + ' GB' : Math.round(x.value) + ' MB'}` : `${x.value >= 1024 ? (x.value / 1024).toFixed(1) + ' MB/s' : Math.round(x.value) + ' KB/s'}`);
  const list = p?.top[tab] ?? [];
  const top = list[0]?.value || 1;
  return (
    <Section title={live ? 'What is using the machine now' : `What was using the machine at ${p ? new Date(p.at).toLocaleTimeString() : ''}`} className={className} right={
      <div className="tabs">{(['cpu', 'ram', 'gpu', 'disk'] as const).map((t) => <button key={t} className={t === tab ? 'on' : ''} onClick={() => setTab(t)}>{t.toUpperCase()}</button>)}</div>
    }>
      <div className="m-totals">
        <Tip text="All programs together, from Windows' counters."><span>RAM <b className="mono">{p ? p.ram_gb.toFixed(1) : '—'} GB</b></span></Tip>
        <Tip text="The busiest GPU engine, all programs together."><span>GPU <b className="mono">{p?.gpu_pct ?? '—'}%</b></span></Tip>
        <Tip text="All network adapters together. Windows only gives per-program network figures to admin-level tracing."><span>Network <b className="mono">{p?.net_kbps != null ? (p.net_kbps >= 1024 ? `${(p.net_kbps / 1024).toFixed(1)} MB/s` : `${p.net_kbps} KB/s`) : '—'}</b></span></Tip>
      </div>
      <ol className="procs">
        {list.length === 0 && <li className="muted">Nothing above the noise floor.</li>}
        {list.map((x) => (
          <li key={x.name}>
            <Tip text={`${x.name}${x.count > 1 ? `: ${x.count} processes added up` : ''}`}><span className="p-name">{x.name}</span></Tip>
            <span className="p-bar"><i style={{ transform: barScale((x.value / top) * 100) }} /></span>
            <span className="p-val mono">{fmt(x)}</span>
          </li>
        ))}
      </ol>
    </Section>
  );
}

export { zoneName };

/** Windows Energy Saver caps CPU power whatever the thermal mode says; AWCC greys its modes out then. */
function EnergySaverNote({ status }: { status: Status | null }) {
  if (!status?.energySaver) return null;
  return <div className="note warn span3">Windows Energy Saver is on: Windows caps CPU power, so thermal modes change less than they say (AWCC turns its modes off while it's on). Turn it off in Settings › System › Power &amp; battery for full performance.</div>;
}

// ---------------------------------------------------------------- Overview: the cockpit
// The redesign (2026-11-25): the same gauges, dock, rotors and keyboard, tighter: gauges and mode with the fans
// and boost beside them, then temperature with the keyboard now, then power and load with what's using it.
export function Overview({ status, history, controls, go }: { status: Status | null; history: Point[]; controls: Controls | null; go: (p: Page) => void }) {
  const s = status?.snapshot;
  const cpu = sensorC(status, 'CPU');
  const gpu = sensorC(status, 'GPU');
  const w = status?.power?.cpuPackageW ?? null;
  const v = verdict(status);
  const m = useModes(status, controls);
  const [hover, setHover] = useState<number | null>(null);
  const shown = hover != null ? history[hover] : history[history.length - 1] ?? null;
  const gpuNow = history[history.length - 1]?.gpu_pct;
  const clk = useClocks();
  return (
    <div className="cockpit">
      <div className={`verdict ${v.tone}`}><span className="v-dot" />{v.text}</div>
      <EnergySaverNote status={status} />
      {controls?.emergency?.active && <div className="note warn span3">Emergency fan rule: every fan is at full until the laptop cools to {controls.emergency.releaseC} °C. Fan changes wait until then.</div>}
      {s?.alerts.map((a, i) => <div key={i} className="note warn">{alertText(a)}</div>)}
      <div className="hero">
        <Gauge label="CPU" value={cpu} sub={<Tip text={`Average clock over every core (fastest core ${ghz(clk?.cpu?.maxMhz)}), and CPU package power from Windows' energy meter. Every core is on the Power page.`}><span className="mono">{ghz(clk?.cpu?.avgMhz)} · {w != null ? `${w.toFixed(1)} W` : '—'}</span></Tip>} tip="CPU temperature from the Alienware firmware sensor the CPU fans follow. TjMax is 100 °C: the CPU slows itself down there." />
        <div className="hero-mid">
          <span className="lbl">Thermal mode</span>
          <span className="mode-now">{s?.profile_name ?? '—'}</span>
          <ModeDock current={m.current} enabled={m.enabled} pending={m.pending} onPick={m.pick} available={m.available} />
          {m.result && <span className={m.result.ok ? 'm-res ok' : 'm-res bad'}>{m.result.text}</span>}
          {!m.enabled && <LockedWhy reasons={controls?.modes.reasons ?? []} />}
        </div>
        <Gauge label="GPU" value={gpu} sub={<Tip text={`The NVIDIA GPU's core clock${clk?.gpu?.maxCoreMhz ? ` (up to ${clk.gpu.maxCoreMhz} MHz)` : ''} and power${clk?.gpu?.powerLimitW ? ` (limit ${Math.round(clk.gpu.powerLimitW)} W)` : ''}, from its driver; the busiest GPU engine is ${gpuNow != null ? `${gpuNow}%` : 'not known'}. More on the Power page.`}><span className="mono">{mhzText(clk?.gpu?.coreMhz)} · {watts(clk?.gpu?.powerW)}</span></Tip>} tip="GPU temperature from the Alienware firmware sensor the GPU fans follow." />
      </div>
      <FanBoost controls={controls} status={status} history={history} compact />
      <Section title="Temperature · hover for any moment" className="span2 cp-late" right={<button className="chip" onClick={() => go('Timeline')}>Timeline</button>}>
        <Chart points={history} min={30} max={105} hover={hover} onHover={setHover} series={[{ name: 'CPU', color: COL.cpu, unit: '°', values: history.map((p) => p.cpu_c) }, { name: 'GPU', color: COL.gpu, unit: '°', values: history.map((p) => p.gpu_c) }]} />
      </Section>
      <LightingGlance go={go} />
      <Section title="Power and load · each line on its own scale" className="span2 cp-late" right={clk?.power && <Tip text="The CPU package and the NVIDIA GPU together, as they report it. Where every watt goes is on the Power page."><span className="lbl">measured now <b className="mono">{watts(clk.power.measuredW)}</b></span></Tip>}>
        <LoadChart points={history} hover={hover} onHover={setHover} series={loadSeries(history)} />
      </Section>
      <Moment p={shown} live={hover == null} className="cp-late" />
    </div>
  );
}

/** The keyboard as it is now, small and read-only, with the lighting in a line. Read once when shown (the
 * Lighting page is where it changes). */
function LightingGlance({ go }: { go: (p: Page) => void }) {
  const [st, setSt] = useState<LightingStatus | null>(null);
  useEffect(() => {
    let live = true;
    api.lightingStatus().then((s) => { if (live) setSt(s); }, () => {});
    return () => { live = false; };
  }, []);
  const colors = useMemo(() => (st ? (st.settings.mode === 'custom' || !st.awcc ? shownColors(st.layout, st.settings.keyboard) : st.awcc.keys) : {}), [st]);
  const kb = st?.settings.keyboard;
  const live = st ? liveSummary(liveOf(st.settings)) : '';
  const none = () => {};
  return (
    <Section title={st?.settings.mode === 'custom' ? 'Lighting · custom' : "Lighting · AWCC's look"} right={<button className="chip" onClick={() => go('Lighting')}>Edit</button>}>
      {st ? (
        <button className="kb-glance" onClick={() => go('Lighting')} aria-label="Open Lighting">
          <KeyboardMap layout={st.layout} colors={colors} selected={[]} editable={false} tool="select" onSelect={none} onPaint={none} onStroke={none} />
        </button>
      ) : <p className="muted small">Reading the keyboard…</p>}
      {kb && <p className="small muted glance-line">{st?.settings.mode === 'custom' ? `${live || 'Static colours'} · ${kb.on ? `${kb.brightness}%` : 'backlight off'}` : 'The app leaves the keyboard to AWCC.'}</p>}
    </Section>
  );
}

// ---------------------------------------------------------------- Thermals: modes, fans and the rules
export function Thermals({ status, history, controls, settings, save }: { status: Status | null; history: Point[]; controls: Controls | null; settings: Settings | null; save: (s: Settings) => void }) {
  const m = useModes(status, controls);
  const fans = status?.snapshot?.fans ?? [];
  const [sel, setSel] = useState('0x32');
  const [hover, setHover] = useState<number | null>(null);
  const curve = settings?.fanCurves[sel] ?? [[45, 0], [65, 25], [80, 60], [92, 100]];
  return (
    <div className="cockpit">
      <EnergySaverNote status={status} />
      {controls?.emergency?.active && <div className="note warn span3">Emergency fan rule: every fan is at full until the laptop cools to {controls.emergency.releaseC} °C. Fan changes wait until then.</div>}
      <Section title="Thermal mode" className="span3" right={<span className="lbl">{m.enabled ? `via ${controls?.modes.route} · confirmed by reading it back` : 'read only'}</span>}>
        <div className="mode-cards">
          {[...['0xA3', '0xA0', '0xA1', '0xA4', '0xA5'], '0xAB'].filter((id) => m.available.includes(id)).map((id) => (
            <button key={id} className={`mcard ${m.current === id ? 'on' : ''} ${m.pending === id ? 'pending' : ''}`} disabled={!m.enabled || m.pending != null} onClick={() => m.pick(id)}>
              <b>{PROFILE_NAMES[id]}</b>
              <small>{({ '0xA3': 'Fans low, power limited.', '0xA0': 'Everyday.', '0xA1': 'More power, fans sooner.', '0xA4': 'Fans flat out.', '0xA5': 'Longest battery.', '0xAB': 'Maximum power and fans.' } as Record<string, string>)[id]}</small>
            </button>
          ))}
        </div>
        {m.result && <span className={m.result.ok ? 'm-res ok' : 'm-res bad'}>{m.result.text}</span>}
        <LockedWhy reasons={controls?.modes.reasons ?? []} />
      </Section>
      <FanBoost controls={controls} status={status} history={history} />
      <Section title="Emergency fans · boost key">
        {settings && (
          <>
            <label className="field"><span className="lbl">Emergency fans at {settings.emergencyC} °C</span><input type="range" min={85} max={98} value={settings.emergencyC} onChange={(e) => save({ ...settings, emergencyC: Number(e.target.value) })} /></label>
            <p className="muted small">When a sensor stays this hot, every fan goes to full until it cools. It can be lowered, never raised past 98 °C.</p>
            <label className="field"><span className="lbl">Boost key Fn+F1</span>
              <select value={settings.boostKey} onChange={(e) => save({ ...settings, boostKey: e.target.value })}><option value="awcc">Toggles G-Mode</option><option value="cycle">Steps through the modes</option></select>
            </label>
            <label className="field"><span className="lbl">Back from G-Mode, if the mode before is unknown</span>
              <select value={settings.defaultProfile} onChange={(e) => save({ ...settings, defaultProfile: e.target.value })}>{['0xA3', '0xA0', '0xA1', '0xA4', '0xA5'].map((p) => <option key={p} value={p}>{PROFILE_NAMES[p]}</option>)}</select>
            </label>
            <p className="muted small">Presses within about 2 s are ignored, like AWCC. Alien Cockpit catches the key even when an admin window is in front.</p>
          </>
        )}
      </Section>
      <FanGraphs points={history} fans={fans} title="last minutes" />
      <Section title="Custom fan curve" right={<span className="lbl">applies when fan control unlocks</span>}>
        <div className="row">{(status?.lists?.fans ?? ['0x32', '0x33', '0x3B', '0x3C']).map((f) => <button key={f} className={f === sel ? 'chip on' : 'chip'} onClick={() => setSel(f)}>{f}</button>)}</div>
        <CurveEditor points={curve} onChange={(p) => settings && save({ ...settings, fanCurves: { ...settings.fanCurves, [sel]: p } })} />
        <LockedWhy reasons={controls?.fans.reasons ?? []} />
      </Section>
      <Section title="All fans · hover for any moment" className="span2">
        <Chart points={history} hover={hover} onHover={setHover} series={fans.map((f, i) => ({ name: hex(f.id), color: COL.fans[i % 4], unit: '', values: history.map((p) => p.fans[i] ?? null) }))} />
      </Section>
    </div>
  );
}

// ---------------------------------------------------------------- Power: clocks, where the watts go, load
export function PowerPage({ history }: { history: Point[] }) {
  const clk = useClocks();
  const [hover, setHover] = useState<number | null>(null);
  const shown = hover != null ? history[hover] : history[history.length - 1] ?? null;
  return (
    <div className="cockpit">
      <Section title="Clocks · live, like CPU-Z and GPU-Z" className="span3">
        <ClocksPanel c={clk} />
      </Section>
      <Section title="Power · where it goes" className="span3">
        <PowerPanel c={clk} />
      </Section>
      <Section title="Power and load · each line on its own scale · hover for any moment" className="span2">
        <LoadChart points={history} hover={hover} onHover={setHover} series={loadSeries(history)} />
      </Section>
      <Moment p={shown} live={hover == null} className="" />
    </div>
  );
}

// ---------------------------------------------------------------- fan boost and fan graphs
const LEVELS = [0, 25, 50, 75, 100];

/** The fans where they sit in the laptop, seen from above as you sit (`fanLayout`): the graphics side on the
 * left, the processor side on the right, each primary (larger) at the rear corner and its secondary (smaller)
 * in front of it. Other machines: a row. `onPick`: a fan clicked (Thermals: boost just that one). */
function FanLayout({ fans, compact, flagOf, selected, onPick }: {
  fans: Fan[]; compact?: boolean; flagOf: (f: Fan) => ReturnType<typeof fanFlag>; selected: string | null; onPick?: (id: string) => void;
}) {
  const slots = fanLayout(fans);
  const rotor = (f: Fan, size?: 'lg' | 'sm', name?: string) => {
    const id = hex(f.id);
    const r = <FanRotor rpm={f.rpm} max={f.max_rpm} label={name ?? zoneName(fans, f)} id={id} size={size} {...flagOf(f)} />;
    return onPick ? (
      <div key={f.id} className={`fan-slot ${selected === id ? 'on' : ''}`} role="button" tabIndex={0} aria-pressed={selected === id}
        onClick={() => onPick(id)} onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onPick(id); } }}>{r}</div>
    ) : <div key={f.id} className="fan-slot">{r}</div>;
  };
  if (!slots) return <div className={compact ? 'rotors four' : 'rotors'}>{fans.map((f) => rotor(f))}</div>;
  const side = (s: 'left' | 'right') => slots.filter((x) => x.side === s).map((x) => rotor(x.fan, x.primary ? 'lg' : 'sm', x.name));
  return (
    <div className={`chassis ${compact ? 'compact' : ''}`} aria-label="The fans where they sit, seen from above">
      <span className="lbl ch-edge">Rear · vents</span>
      <div className="ch-grid">
        <div className="ch-col">{side('left')}</div>
        <div className="ch-mid" aria-hidden><span className="ch-chip">GPU</span><span className="ch-chip">CPU</span></div>
        <div className="ch-col">{side('right')}</div>
      </div>
      <span className="lbl ch-edge front">{compact ? 'Front' : 'Front · keyboard'}</span>
    </div>
  );
}

/** The fans spinning, with boost on top of the firmware's curve. `compact`: the Cockpit's panel (all fans at once,
 * smaller rotors); else the Thermals panel (one fan or all). */
function FanBoost({ controls, status, history, compact }: { controls: Controls | null; status: Status | null; history: Point[]; compact?: boolean }) {
  const s = status?.snapshot;
  const fans = s?.fans ?? [];
  const stuck = stuckFans(history, fans);
  const [which, setWhich] = useState<'all' | string>('all');
  const [pending, setPending] = useState<number | null>(null);
  const [res, setRes] = useState<{ ok: boolean; text: string } | null>(null);
  const boosted = controls?.fans.boosted ?? {};
  const ids = fans.map((f) => hex(f.id));
  const targets = which === 'all' ? ids : [which];
  const current = which === 'all' ? (ids.every((f) => (boosted[f] ?? 0) === (boosted[ids[0]] ?? 0)) ? boosted[ids[0]] ?? 0 : null) : boosted[which] ?? 0;
  const apply = async (level: number) => {
    setPending(level);
    setRes(null);
    try {
      const out: BoostChange[] = await api.setFanBoost(targets, level);
      const moved = out.map((c) => `${c.fan} ${c.rpm_before ?? '?'}→${c.rpm_after ?? '?'} rpm`).join(', ');
      setRes(out.every((c) => c.confirmed) ? { ok: true, text: level ? `Boost ${level}% set. ${moved}.` : `Back on the firmware's curve. ${moved}.` } : { ok: false, text: `The firmware didn't confirm every fan: ${out.filter((c) => !c.confirmed).map((c) => c.fan).join(', ')}.` });
    } catch (e) {
      setRes({ ok: false, text: String(e) });
    } finally {
      setPending(null);
    }
  };
  const on = controls?.fans.enabled ?? false;
  return (
    <Section title={compact ? 'Fans · live' : 'Fans · boost on top of the firmware\'s curve'} className={compact ? '' : 'span2'} right={<span className="lbl">{on ? `boost ${current == null ? 'mixed' : current ? `+${current}%` : 'off'}` : 'boost unavailable'}</span>}>
      {!s ? <p className="muted">No fan readings without the hardware service.</p> : (
        <FanLayout fans={fans} compact={compact} flagOf={(f) => fanFlag(f, s, stuck)}
          selected={compact || which === 'all' ? null : which} onPick={compact ? undefined : (id) => setWhich(which === id ? 'all' : id)} />
      )}
      <div className="boost">
        {!compact && (
          <div className="seg small">
            <button className={which === 'all' ? 'on' : ''} onClick={() => setWhich('all')}>All fans</button>
            {ids.map((f) => <button key={f} className={which === f ? 'on' : ''} onClick={() => setWhich(f)}>{f}</button>)}
          </div>
        )}
        <div className="levels">
          {LEVELS.map((l) => (
            <Tip key={l} text={l === 0 ? 'Off: the firmware alone decides the speed.' : `Adds ${l}% boost to ${which === 'all' ? 'every fan' : which}. In Full Speed and G-Mode the fans are already near their limit; boost helps most in Balanced and Performance.`}>
              <button className={`lvl ${current === l ? 'on' : ''} ${pending === l ? 'pending' : ''}`} disabled={!on || pending != null} onClick={() => apply(l)}>{l === 0 ? 'Off' : l === 100 ? 'Max' : `+${l}`}</button>
            </Tip>
          ))}
        </div>
      </div>
      {res && <span className={res.ok ? 'm-res ok' : 'm-res bad'}>{res.text}</span>}
      {!compact && <LockedWhy reasons={on ? controls?.fans.notes ?? [] : controls?.fans.reasons ?? []} />}
    </Section>
  );
}

/** The fan graphs: how each fan really responds to temperature, speed with temperature, and how hard each works. */
export function FanGraphs({ points, fans, title }: { points: Point[]; fans: Fan[]; title: string }) {
  const [sel, setSel] = useState(0);
  const fan = fans[sel];
  if (!fan) return null;
  return (
    <>
      <Section title={`Fan response · speed against temperature · ${title}`} className="span2" right={<div className="seg small">{fans.map((f, i) => <button key={f.id} className={i === sel ? 'on' : ''} onClick={() => setSel(i)}>{hex(f.id)}</button>)}</div>}>
        <FanResponse points={points} fan={fan} fanIndex={sel} />
        <p className="muted small">Each dot is one reading, coloured by mode; the line is the median speed per 2 °C. It shows what the firmware really does in each mode, which is what a custom curve would change.</p>
      </Section>
      <Section title="How hard each fan works">
        <Duty points={points} fans={fans} />
      </Section>
      <Section title={`${hex(fan.id)} speed with its ${sensorName(fan.sensor)} temperature`} className="span3">
        <SpeedAndTemp points={points} fan={fan} fanIndex={sel} />
      </Section>
    </>
  );
}

// ---------------------------------------------------------------- Timeline (live readings and the daily log)
export function Timeline({ live }: { live: Point[] }) {
  const [days, setDays] = useState<LogDay[] | null>(null);
  const [day, setDay] = useState<string | null>(null);
  const [logged, setLogged] = useState<Point[]>([]);
  const [hover, setHover] = useState<number | null>(null);
  useEffect(() => { api.logDays().then((d) => setDays(d.days)); }, []);
  useEffect(() => { if (day) api.logRead(day).then((p) => { setLogged(p); setHover(null); }); }, [day]);
  // "Live": the readings since the app started, still arriving; a day: that day's log.
  const points = day ? logged : live;
  const shown = hover != null ? points[hover] : points[points.length - 1] ?? null;
  const fans: Fan[] = (points.find((p) => p.fans.length)?.fans ?? []).map((_, i) => ({ id: [0x32, 0x33, 0x3b, 0x3c][i] ?? i, sensor: i % 2 === 0 ? 1 : 6, rpm: null, max_rpm: [4200, 4100, 3000, 3000][i] ?? null }));
  return (
    <div className="cockpit">
      <div className="row span3 tl-days">
        <button className={day == null ? 'chip on' : 'chip'} onClick={() => { setDay(null); setHover(null); }}><i className="dot live" /> Live</button>
        {days?.map((d) => <button key={d.day} className={d.day === day ? 'chip on' : 'chip'} onClick={() => setDay(d.day)}>{new Date(d.day + 'T12:00:00').toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' })} <span className="muted">· {(d.bytes / 1_048_576).toFixed(1)} MB</span></button>)}
        {days?.length === 0 && <span className="muted small">No days logged yet. Logging is set in Settings.</span>}
        <span className="spacer" /><span className="lbl">{points.length} readings</span>
      </div>
      {points.length > 0 && (
        <>
          <Section title={day ? 'The day · mode changes marked · hover for any moment' : 'Since the app started · mode changes marked'} className="span3">
            <TimelineBand points={points} hover={hover} onHover={setHover} />
          </Section>
          <Section title="Temperature" className="span2">
            <Chart points={points} min={30} max={105} hover={hover} onHover={setHover} series={[{ name: 'CPU', color: COL.cpu, unit: '°', values: points.map((p) => p.cpu_c) }, { name: 'GPU', color: COL.gpu, unit: '°', values: points.map((p) => p.gpu_c) }]} />
          </Section>
          <Moment p={shown} live={false} />
          <Section title="Power and load" className="span2">
            <LoadChart points={points} hover={hover} onHover={setHover} series={loadSeries(points)} />
          </Section>
          {fans.length > 0 && <FanGraphs points={points} fans={fans} title={day ? 'this day' : 'since the app started'} />}
        </>
      )}
    </div>
  );
}

/** The whole span at a glance: CPU temperature as a band, a mark wherever the thermal mode changed, and the
 * moment under the pointer, shared with the charts below. */
function TimelineBand({ points, hover, onHover }: { points: Point[]; hover: number | null; onHover: (i: number | null) => void }) {
  const W = 1000, H = 44;
  const n = Math.max(2, points.length);
  const area = useMemo(() => {
    const xs = points.map((_, i) => (i / (n - 1)) * W);
    const y = (c: number | null) => (c == null ? H : H - (Math.max(30, Math.min(105, c)) - 30) / 75 * H);
    return `M0 ${H} ` + points.map((p, i) => `L${xs[i].toFixed(1)} ${y(p.cpu_c).toFixed(1)}`).join(' ') + ` L${W} ${H} Z`;
  }, [points, n]);
  const marks = useMemo(() => modeChanges(points), [points]);
  const move = (e: React.PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    onHover(Math.max(0, Math.min(points.length - 1, Math.round(((e.clientX - r.left) / r.width) * (n - 1)))));
  };
  const at = (i: number) => `${(i / (n - 1)) * 100}%`;
  const first = points[0], last = points[points.length - 1];
  return (
    <>
      <div className="tl-band" onPointerMove={move} onPointerLeave={() => onHover(null)}>
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" aria-hidden="true"><path d={area} /></svg>
        {marks.map((m, k) => <i key={m.i} className={`tl-mark ${m.to === 'G-Mode' ? 'g' : ''}`} style={{ left: at(m.i) }} title={`${new Date(points[m.i].at).toLocaleTimeString()}: ${m.from} → ${m.to}`}>{(marks.length <= 8 || k % Math.ceil(marks.length / 8) === 0) && <span>{m.to}</span>}</i>)}
        {hover != null && <i className="tl-cursor" style={{ left: at(hover) }} />}
      </div>
      <div className="tl-axis mono"><span>{first ? new Date(first.at).toLocaleTimeString() : ''}</span><span>{marks.length ? `${marks.length} mode change${marks.length > 1 ? 's' : ''}` : 'no mode changes'}</span><span>{last ? new Date(last.at).toLocaleTimeString() : ''}</span></div>
    </>
  );
}

// ---------------------------------------------------------------- Lighting (lighting.tsx)
export { Lighting } from './lighting';

// ---------------------------------------------------------------- Health (incl. Dell services)
export function HealthView() {
  const [h, setH] = useState<Health | null>(null);
  const [d, setD] = useState<{ ok: boolean; drift: Drift[] } | null>(null);
  useEffect(() => { api.health().then(setH); api.drift().then(setD); }, []);
  const b = h?.battery;
  const real = d?.drift.filter((x) => x.severity !== 'info') ?? [];
  return (
    <div className="cockpit">
      <Section title="Battery" className="span2">
        {b ? (
          <div className="battery">
            <Tip text={`Full charge now holds ${b.full_mwh.toLocaleString()} mWh of the ${b.design_mwh.toLocaleString()} it was built for.`}>
              <div className="cell"><i style={{ width: `${100 - b.wear_pct}%` }} /><span className="mono">{(100 - b.wear_pct).toFixed(0)}%</span></div>
            </Tip>
            <p>{b.wear_pct >= 20 ? `It holds ${b.wear_pct.toFixed(0)}% less than when new, so expect shorter time on battery.` : 'Close to new.'} <span className="muted">{b.cycles != null && `${b.cycles} charge cycles.`}</span></p>
          </div>
        ) : <p className="muted">{h ? 'No battery data.' : 'Reading…'}</p>}
      </Section>
      <Section title="Last 30 days">
        {h?.events ? (
          <ul className="facts">
            <li><Tip text="Blue screens (BugCheck 1001)"><span>Blue screens</span></Tip><b className={h.events.crashes ? 'bad' : 'good'}>{h.events.crashes}</b></li>
            <li><Tip text="Errors the CPU, memory or PCIe reported (WHEA-Logger). Corrected ones are usually harmless."><span>Hardware errors</span></Tip><b className={h.events.hardware_errors ? 'warn' : 'good'}>{h.events.hardware_errors}</b></li>
            <li><Tip text="Power lost or the machine stopped responding (Kernel-Power 41)."><span>Sudden shutdowns</span></Tip><b className={h.events.unexpected_shutdowns ? 'warn' : 'good'}>{h.events.unexpected_shutdowns}</b></li>
          </ul>
        ) : <p className="muted">{h ? 'Event log unavailable.' : 'Reading…'}</p>}
      </Section>
      <Section title="Dell services" className="span2" right={d && <span className={real.length ? 'lbl warn-t' : 'lbl ok-t'}>{real.length ? `${real.length} changed by Dell` : 'as you set them'}</span>}>
        {real.length === 0 && d && <p>All match your choices.</p>}
        {real.map((x) => (
          <div key={x.service} className="drift">
            <span className={`sev ${x.severity}`}>{x.severity}</span><b>{x.service}</b> is <code>{x.actual}</code>; your rule says <code>{x.expected.join(' or ')}</code>.
            <p className="muted small">{x.why}</p>
          </div>
        ))}
      </Section>
      <Section title="Disks">
        {h && h.disks.length > 0 ? <ul className="facts">{h.disks.map((x) => <li key={x.name}><span>{x.name}</span><b>{x.wear_pct ?? '—'}% worn</b></li>)}</ul>
          : <p className="muted small">{h ? 'Disk wear needs the hardware service (Windows only lets admin programs read it).' : 'Reading…'}</p>}
      </Section>
    </div>
  );
}

// ---------------------------------------------------------------- Settings
export function SettingsView({ settings, save, error, go }: { settings: Settings | null; save: (s: Settings) => void; error: string | null; go: (p: Page) => void }) {
  const s = settings;
  if (!s) return null;
  return (
    <div className="cockpit">
      <Section title="Appearance" className="span3">
        <div className="seg small" role="radiogroup">{(['dark', 'gray', 'light'] as const).map((t) => <button key={t} role="radio" aria-checked={s.theme === t} className={s.theme === t ? 'on' : ''} onClick={() => save({ ...s, theme: t })}>{t[0].toUpperCase() + t.slice(1)}</button>)}</div>
        <div className="toggles-grid">
          {([
            ['hover', 'Hover highlight', 'Controls darken slightly under the pointer. Nothing moves or glows.'],
            ['press', 'Click feel', 'Buttons press in and ripple where you click.'],
            ['largeText', 'Larger text', 'Bigger type throughout.'],
          ] as const).map(([k, name, what]) => (
            <label key={k} className="switch">
              <input type="checkbox" checked={s.effects[k]} onChange={(e) => save({ ...s, effects: { ...s.effects, [k]: e.target.checked } })} />
              <span className="sw" aria-hidden />
              <span><b>{name}</b><small className="muted">{what}</small></span>
            </label>
          ))}
        </div>
        <p className="muted small">Windows' "reduce motion" setting turns all motion off regardless.</p>
      </Section>
      <Section title="Live readings">
        <label className="switch">
          <input type="checkbox" checked={s.liveAdmin} onChange={(e) => save({ ...s, liveAdmin: e.target.checked })} />
          <span className="sw" aria-hidden />
          <span><b>Ask for admin at start</b><small className="muted">The Alienware firmware only answers administrators. Without the hardware service, Alien Cockpit restarts itself as administrator (one Windows prompt) so temperatures, fans and modes are live.</small></span>
        </label>
        <label className="switch">
          <input type="checkbox" checked={!!s.autostart} onChange={(e) => save({ ...s, autostart: e.target.checked })} />
          <span className="sw" aria-hidden />
          <span><b>Start at sign-in</b><small className="muted">Starts in the tray when you sign in, as administrator without a prompt (a Task Scheduler task, set up once while Alien Cockpit runs as administrator). Turning it off removes the task.</small></span>
        </label>
      </Section>
      <Section title="Alerts and data">
        <label className="check"><input type="checkbox" checked={s.notify} onChange={(e) => save({ ...s, notify: e.target.checked })} /> Windows notifications for fan faults, stale sensors and boost-key switches</label>
        <label className="check"><input type="checkbox" checked={s.demo} onChange={(e) => save({ ...s, demo: e.target.checked })} /> Show demo data (the October 7 capture) when nothing live is available</label>
        <div className="row"><span className="muted small">Emergency fans ({s.emergencyC} °C) are on Thermals now, beside the fans.</span><button className="chip" onClick={() => go('Thermals')}>Thermals</button></div>
      </Section>
      <Section title="Drivers and BIOS">
        <p>Installs stay with Dell's own signed tool; Alien Cockpit never flashes a BIOS.</p>
        <div className="row"><button className="btn" onClick={() => api.openAlienwareUpdate()}>Open Alienware Update</button></div>
      </Section>
      <LogSection s={s} save={save} />
      {error && <div className="note warn span3">{error}</div>}
    </div>
  );
}

function LogSection({ s, save }: { s: Settings; save: (s: Settings) => void }) {
  const [info, setInfo] = useState<{ days: LogDay[]; totalBytes: number; dir: string } | null>(null);
  const [cleaned, setCleaned] = useState<string | null>(null);
  useEffect(() => { api.logDays().then(setInfo); }, [s.log]);
  const set = (patch: Partial<Settings['log']>) => save({ ...s, log: { ...s.log, ...patch } });
  const perDayMb = (86_400 / s.log.everyS) * 1.3 / 1024;
  return (
    <Section title="Daily log" className="span3" right={<span className="lbl">{info ? `${info.days.length} days · ${(info.totalBytes / 1_048_576).toFixed(1)} MB` : ''}</span>}>
      <label className="check"><input type="checkbox" checked={s.log.enabled} onChange={(e) => set({ enabled: e.target.checked })} /> Keep a daily log of readings and the programs using the machine (stays on this laptop)</label>
      <div className="split3">
        <label className="field"><span className="lbl">Record every</span>
          <select value={s.log.everyS} onChange={(e) => set({ everyS: Number(e.target.value) })}>{[10, 30, 60, 300].map((v) => <option key={v} value={v}>{v < 60 ? `${v} seconds` : `${v / 60} minute${v > 60 ? 's' : ''}`}</option>)}</select>
        </label>
        <label className="field"><span className="lbl">Keep</span>
          <select value={s.log.keepDays} onChange={(e) => set({ keepDays: Number(e.target.value) })}>{[3, 7, 14, 30, 90, 365].map((v) => <option key={v} value={v}>{v} days</option>)}</select>
        </label>
        <label className="field"><span className="lbl">Size cap</span>
          <select value={s.log.maxMb} onChange={(e) => set({ maxMb: Number(e.target.value) })}>{[50, 100, 200, 500, 1000].map((v) => <option key={v} value={v}>{v} MB</option>)}</select>
        </label>
      </div>
      <p className="muted small">About {perDayMb.toFixed(1)} MB a day at this rate. Older days are removed automatically, then the oldest go first if the folder passes the cap; today's log is never removed.</p>
      <div className="row">
        <button className="chip" onClick={() => api.logCleanup().then((r) => { setCleaned(r.length ? `Removed ${r.join(', ')}.` : 'Nothing to remove.'); api.logDays().then(setInfo); })}>Clean up now</button>
        {cleaned && <span className="muted small">{cleaned}</span>}
        {info && <Tip text={info.dir}><span className="muted small">Where the logs are</span></Tip>}
      </div>
    </Section>
  );
}
