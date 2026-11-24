import { useEffect, useState } from 'react';
import { api, hex, PROFILE_NAMES, sensorName, type BoostChange, type Controls, type Drift, type Fan, type Health, type LogDay, type ModeChange, type Point, type Proc, type Settings, type Status } from './api';
import { Chart, CurveEditor, FanRotor, Gauge, LoadChart, LockedWhy, ModeDock, Section, Tip, type LoadSeries } from './components';
import { Duty, FanResponse, SpeedAndTemp } from './fangraphs';
import { alertText, fanFlag, stuckFans, verdict, zoneName } from './logic';

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

function Moment({ p, live }: { p: Point | null; live: boolean }) {
  const [tab, setTab] = useState<'cpu' | 'ram' | 'gpu' | 'disk'>('cpu');
  const fmt = (x: Proc) => (tab === 'cpu' || tab === 'gpu' ? `${x.value.toFixed(1)}%` : tab === 'ram' ? `${x.value >= 1024 ? (x.value / 1024).toFixed(1) + ' GB' : Math.round(x.value) + ' MB'}` : `${x.value >= 1024 ? (x.value / 1024).toFixed(1) + ' MB/s' : Math.round(x.value) + ' KB/s'}`);
  const list = p?.top[tab] ?? [];
  const top = list[0]?.value || 1;
  return (
    <Section title={live ? 'What is using the machine now' : `What was using the machine at ${p ? new Date(p.at).toLocaleTimeString() : ''}`} className="moment" right={
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
            <span className="p-bar"><i style={{ width: `${(x.value / top) * 100}%` }} /></span>
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
export function Overview({ status, history, controls }: { status: Status | null; history: Point[]; controls: Controls | null }) {
  const s = status?.snapshot;
  const cpu = sensorC(status, 'CPU');
  const gpu = sensorC(status, 'GPU');
  const w = status?.power?.cpuPackageW ?? null;
  const v = verdict(status);
  const stuck = stuckFans(history, s?.fans ?? []);
  const m = useModes(status, controls);
  const [hover, setHover] = useState<number | null>(null);
  const shown = hover != null ? history[hover] : history[history.length - 1] ?? null;
  const gpuNow = history[history.length - 1]?.gpu_pct;
  return (
    <div className="cockpit">
      <div className={`verdict ${v.tone}`}><span className="v-dot" />{v.text}</div>
      <div className="hero">
        <Gauge label="CPU" value={cpu} sub={<Tip text="CPU package power from Windows' energy meter (no driver needed)."><span className="mono">{w != null ? `${w.toFixed(1)} W` : '—'}</span></Tip>} tip="CPU temperature from the Alienware firmware sensor the CPU fans follow. TjMax is 100 °C: the CPU slows itself down there." />
        <div className="hero-mid">
          <span className="lbl">Thermal mode</span>
          <span className="mode-now">{s?.profile_name ?? '—'}</span>
          <ModeDock current={m.current} enabled={m.enabled} pending={m.pending} onPick={m.pick} available={m.available} />
          {m.result && <span className={m.result.ok ? 'm-res ok' : 'm-res bad'}>{m.result.text}</span>}
          {!m.enabled && <LockedWhy reasons={controls?.modes.reasons ?? []} />}
        </div>
        <Gauge label="GPU" value={gpu} sub={<Tip text="The busiest GPU engine right now, from Windows' counters."><span className="mono">{gpuNow != null ? `${gpuNow}% busy` : '—'}</span></Tip>} tip="GPU temperature from the Alienware firmware sensor the GPU fans follow." />
      </div>
      <EnergySaverNote status={status} />
      {controls?.emergency?.active && <div className="note warn span3">Emergency fan rule: every fan is at full until the laptop cools to {controls.emergency.releaseC} °C. Fan changes wait until then.</div>}
      {s?.alerts.map((a, i) => <div key={i} className="note warn">{alertText(a)}</div>)}
      <Section title="Temperature · hover for any moment" className="span2">
        <Chart points={history} min={30} max={105} hover={hover} onHover={setHover} series={[{ name: 'CPU', color: COL.cpu, unit: '°', values: history.map((p) => p.cpu_c) }, { name: 'GPU', color: COL.gpu, unit: '°', values: history.map((p) => p.gpu_c) }]} />
      </Section>
      <Moment p={shown} live={hover == null} />
      <Section title="Fans · live">
        <div className="rotors">
          {(s?.fans ?? []).map((f) => (
            <FanRotor key={f.id} rpm={f.rpm} max={f.max_rpm} label={zoneName(s?.fans ?? [], f)} id={hex(f.id)} {...(s ? fanFlag(f, s, stuck) : {})} />
          ))}
          {!s && <p className="muted">No fan readings without the hardware service.</p>}
        </div>
      </Section>
      <Section title="Power and load · each line on its own scale · hover for any moment" className="span3">
        <LoadChart points={history} hover={hover} onHover={setHover} series={loadSeries(history)} />
      </Section>
    </div>
  );
}

// ---------------------------------------------------------------- Performance
export function Performance({ status, history, controls, settings, save }: { status: Status | null; history: Point[]; controls: Controls | null; settings: Settings | null; save: (s: Settings) => void }) {
  const m = useModes(status, controls);
  const fans = status?.snapshot?.fans ?? [];
  const [sel, setSel] = useState('0x32');
  const [hover, setHover] = useState<number | null>(null);
  const curve = settings?.fanCurves[sel] ?? [[45, 0], [65, 25], [80, 60], [92, 100]];
  return (
    <div className="cockpit">
      <EnergySaverNote status={status} />
      <Section title="Thermal mode" className="span3" right={<span className="lbl">{m.enabled ? `via ${controls?.modes.route}` : 'read only'}</span>}>
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
        <p className="muted small">Each switch is checked first (this laptop's model and BIOS, a mode its firmware reported) and confirmed by reading the mode back.</p>
      </Section>
      <Section title="Boost key · Fn+F1" className="span3">
        <p>Toggles G-Mode, then back to the mode you were in, ignoring presses within about 2 s, like AWCC. While an admin window is in front AWCC can't hear the key; Alien Cockpit catches it then.</p>
        {settings && (
          <div className="split">
            <label className="field"><span className="lbl">Key does</span>
              <select value={settings.boostKey} onChange={(e) => save({ ...settings, boostKey: e.target.value })}><option value="awcc">Toggle G-Mode</option><option value="cycle">Step through modes</option></select>
            </label>
            <label className="field"><span className="lbl">Back from G-Mode, when the mode before is unknown (after a restart)</span>
              <select value={settings.defaultProfile} onChange={(e) => save({ ...settings, defaultProfile: e.target.value })}>{['0xA3', '0xA0', '0xA1', '0xA4', '0xA5'].map((p) => <option key={p} value={p}>{PROFILE_NAMES[p]}</option>)}</select>
            </label>
          </div>
        )}
      </Section>
      <FanBoost controls={controls} fans={fans} />
      <FanGraphs points={history} fans={fans} title="last minutes" />
      <Section title="All fans · hover for any moment" className="span3">
        <Chart points={history} hover={hover} onHover={setHover} series={fans.map((f, i) => ({ name: hex(f.id), color: COL.fans[i % 4], unit: '', values: history.map((p) => p.fans[i] ?? null) }))} />
      </Section>
      <Section title="Custom fan curve" className="span3" right={<span className="lbl">saved · applies when fan control unlocks</span>}>
        <div className="row">{(status?.lists?.fans ?? ['0x32', '0x33', '0x3B', '0x3C']).map((f) => <button key={f} className={f === sel ? 'chip on' : 'chip'} onClick={() => setSel(f)}>{f}</button>)}</div>
        <CurveEditor points={curve} onChange={(p) => settings && save({ ...settings, fanCurves: { ...settings.fanCurves, [sel]: p } })} />
        <LockedWhy reasons={controls?.fans.reasons ?? []} />
      </Section>
    </div>
  );
}

// ---------------------------------------------------------------- fan boost and fan graphs
const LEVELS = [0, 25, 50, 75, 100];

function FanBoost({ controls, fans }: { controls: Controls | null; fans: Fan[] }) {
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
  return (
    <Section title="Fan boost · more cooling on top of the firmware's curve" className="span3" right={<span className="lbl">{controls?.fans.enabled ? 'on' : 'unavailable'}</span>}>
      <div className="boost">
        <div className="seg small">
          <button className={which === 'all' ? 'on' : ''} onClick={() => setWhich('all')}>All fans</button>
          {ids.map((f) => <button key={f} className={which === f ? 'on' : ''} onClick={() => setWhich(f)}>{f}</button>)}
        </div>
        <div className="levels">
          {LEVELS.map((l) => (
            <Tip key={l} text={l === 0 ? 'Off: the firmware alone decides the speed.' : `Adds ${l}% boost. In Full Speed and G-Mode the fans are already near their limit; boost helps most in Balanced and Performance.`}>
              <button className={`lvl ${current === l ? 'on' : ''} ${pending === l ? 'pending' : ''}`} disabled={!controls?.fans.enabled || pending != null} onClick={() => apply(l)}>{l === 0 ? 'Off' : l === 100 ? 'Max' : `+${l}%`}</button>
            </Tip>
          ))}
        </div>
      </div>
      {res && <span className={res.ok ? 'm-res ok' : 'm-res bad'}>{res.text}</span>}
      <LockedWhy reasons={controls?.fans.enabled ? controls.fans.notes ?? [] : controls?.fans.reasons ?? []} />
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

// ---------------------------------------------------------------- History (the daily log)
export function History() {
  const [days, setDays] = useState<LogDay[] | null>(null);
  const [day, setDay] = useState<string | null>(null);
  const [points, setPoints] = useState<Point[]>([]);
  const [hover, setHover] = useState<number | null>(null);
  useEffect(() => { api.logDays().then((d) => { setDays(d.days); if (d.days[0]) setDay(d.days[0].day); }); }, []);
  useEffect(() => { if (day) api.logRead(day).then((p) => { setPoints(p); setHover(null); }); }, [day]);
  const shown = hover != null ? points[hover] : points[points.length - 1] ?? null;
  const fans: Fan[] = (points.find((p) => p.fans.length)?.fans ?? []).map((_, i) => ({ id: [0x32, 0x33, 0x3b, 0x3c][i] ?? i, sensor: i % 2 === 0 ? 1 : 6, rpm: null, max_rpm: [4200, 4100, 3000, 3000][i] ?? null }));
  return (
    <div className="cockpit">
      <Section title="Logged days" className="span3" right={<span className="lbl">{points.length} readings</span>}>
        {days?.length === 0 && <p className="muted">No days logged yet. Logging is set in Settings.</p>}
        <div className="row">{days?.map((d) => <button key={d.day} className={d.day === day ? 'chip on' : 'chip'} onClick={() => setDay(d.day)}>{new Date(d.day + 'T12:00:00').toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' })} <span className="muted">· {(d.bytes / 1_048_576).toFixed(1)} MB</span></button>)}</div>
      </Section>
      {points.length > 0 && (
        <>
          <Section title="Power and load · hover for any moment" className="span2">
            <LoadChart points={points} hover={hover} onHover={setHover} series={loadSeries(points)} />
          </Section>
          <Moment p={shown} live={false} />
          <Section title="Temperature" className="span2">
            <Chart points={points} min={30} max={105} hover={hover} onHover={setHover} series={[{ name: 'CPU', color: COL.cpu, unit: '°', values: points.map((p) => p.cpu_c) }, { name: 'GPU', color: COL.gpu, unit: '°', values: points.map((p) => p.gpu_c) }]} />
          </Section>
          {fans.length > 0 && <FanGraphs points={points} fans={fans} title="this day" />}
        </>
      )}
    </div>
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
export function SettingsView({ settings, save, error }: { settings: Settings | null; save: (s: Settings) => void; error: string | null }) {
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
      <Section title="Live readings" className="span3">
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
      <Section title="Safety" className="span3">
        <label className="field"><span className="lbl">Emergency fans at {s.emergencyC} °C</span><input type="range" min={85} max={98} value={s.emergencyC} onChange={(e) => save({ ...s, emergencyC: Number(e.target.value) })} /></label>
        <p className="muted small">When a sensor stays this hot, every fan goes to full until it cools. It can be lowered, never raised past 98 °C.</p>
      </Section>
      <Section title="Alerts and data" className="span3">
        <label className="check"><input type="checkbox" checked={s.notify} onChange={(e) => save({ ...s, notify: e.target.checked })} /> Windows notifications for fan faults, stale sensors and boost-key switches</label>
        <label className="check"><input type="checkbox" checked={s.demo} onChange={(e) => save({ ...s, demo: e.target.checked })} /> Show demo data (the October 7 capture) when nothing live is available</label>
      </Section>
      <LogSection s={s} save={save} />
      <Section title="Drivers and BIOS" className="span3">
        <p>Installs stay with Dell's own signed tool; Alien Cockpit never flashes a BIOS.</p>
        <div className="row"><button className="btn" onClick={() => api.openAlienwareUpdate()}>Open Alienware Update</button></div>
      </Section>
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
