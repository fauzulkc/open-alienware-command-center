import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { api, hex, PROFILE_NAMES, type Controls, type Point, type Settings, type Status } from './api';
import { Mark, Tip } from './components';
import { applyEffects, installEffects } from './fx';
import { mergeHistory, nearestBoost } from './logic';
import { Palette, type Runnable } from './palette';
import { applyTheme, type Theme } from './theme';
import { HealthView, Lighting, Overview, PowerPage, SettingsView, Thermals, Timeline } from './views';

/** Six pages in the tabs; Settings sits behind the gear (the redesign, 2026-11-25). */
const PAGES = ['Cockpit', 'Thermals', 'Power', 'Lighting', 'Timeline', 'Health'] as const;
export type Page = (typeof PAGES)[number] | 'Settings';

const SOURCE: Record<string, { dot: string; text: string; long: string }> = {
  service: { dot: 'live', text: 'Live', long: 'Live, from the hardware service.' },
  direct: { dot: 'live', text: 'Live', long: 'Live, read directly (Alien Cockpit runs as administrator).' },
  demo: { dot: 'demo', text: 'Demo', long: 'Demo data: the October 7 capture, not this moment.' },
  unavailable: { dot: 'off', text: 'Windows only', long: 'No firmware readings: start Alien Cockpit as administrator or install the hardware service. Windows-side numbers still show.' },
};
const MODE_ORDER = ['0xA3', '0xA0', '0xA1', '0xA4', '0xA5', '0xAB'];

/** The window's own controls: the native title bar is off, so the whole top strip drags the window. The window
 * module is loaded with the page, so the first click doesn't wait for it. */
async function win(action: 'min' | 'max' | 'close') {
  if (!api.inApp) return;
  const w = getCurrentWindow();
  if (action === 'min') await w.minimize();
  if (action === 'max') await w.toggleMaximize();
  if (action === 'close') await w.close(); // the window goes; Alien Cockpit stays in the tray
}

export default function App() {
  const [page, setPage] = useState<Page>('Cockpit');
  const [status, setStatus] = useState<Status | null>(null);
  const [history, setHistory] = useState<Point[]>([]);
  const [controls, setControls] = useState<Controls | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [finding, setFinding] = useState(false);
  const [toast, setToast] = useState<{ text: string; ok: boolean } | null>(null);
  const hist = useRef<Point[]>([]);
  const mainRef = useRef<HTMLElement>(null);
  // A page opens at its top, not where the last one was scrolled to.
  useEffect(() => { mainRef.current?.scrollTo(0, 0); }, [page]);

  useEffect(() => {
    installEffects();
    let live = true;
    let busy = false;
    // Once a second, only while the window is shown, one poll at a time, and only the new chart points.
    const tick = async () => {
      if (busy || document.hidden) return;
      busy = true;
      try {
        const last = hist.current[hist.current.length - 1]?.t;
        const [s, h, c] = await Promise.all([api.status(), api.history(last), api.controls()]);
        if (!live) return;
        setStatus(s);
        setControls(c);
        const next = mergeHistory(hist.current, h);
        if (next !== hist.current) { hist.current = next; setHistory(next); }
      } catch (e) {
        if (live) setError(String(e));
      } finally {
        busy = false;
      }
    };
    tick();
    const id = setInterval(tick, 1000);
    const onShow = () => { if (!document.hidden) tick(); };
    document.addEventListener('visibilitychange', onShow);
    // Ctrl K (or Ctrl /) anywhere in the window opens Find anything. Only this window's own keys: nothing global.
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && (e.key.toLowerCase() === 'k' || e.key === '/')) { e.preventDefault(); setFinding(true); }
    };
    window.addEventListener('keydown', onKey);
    api.settings().then((s) => { setSettings(s); applyTheme(s.theme as Theme); applyEffects(s.effects); });
    return () => { live = false; clearInterval(id); document.removeEventListener('visibilitychange', onShow); window.removeEventListener('keydown', onKey); };
  }, []);

  useEffect(() => {
    if (!toast) return;
    const id = setTimeout(() => setToast(null), 4500);
    return () => clearTimeout(id);
  }, [toast]);

  // Shown at once; if the app refuses the change (an invalid value, or the sign-in task can't be set up), the
  // screen goes back to what is actually saved, with the reason.
  const saved = useRef<Settings | null>(null);
  useEffect(() => { if (settings && !saved.current) saved.current = settings; }, [settings]);
  const save = useCallback((s: Settings) => {
    setSettings(s);
    applyTheme(s.theme as Theme);
    applyEffects(s.effects);
    api.saveSettings(s).then(
      (ok) => { saved.current = ok; setSettings(ok); setError(null); },
      (e) => {
        setError(String(e));
        const back = saved.current;
        if (back) { setSettings(back); applyTheme(back.theme as Theme); applyEffects(back.effects); }
      },
    );
  }, []);

  const commands = useMemo<Runnable[]>(() => {
    const fans = status?.snapshot?.fans.map((f) => hex(f.id)) ?? [];
    const modesOk = controls?.modes.enabled ?? false;
    const fansOk = controls?.fans.enabled ?? false;
    const available = status?.lists?.profiles ?? MODE_ORDER;
    const list: Runnable[] = [];
    list.push({
      id: 'boost', group: 'Do it', title: 'Fan boost on all fans', words: 'fans cooling boost level off max', hint: fansOk ? 'Type a level: “boost 50”. Each fan is read back after.' : 'Fan boost is unavailable now.',
      titleFor: (n) => (n == null ? 'Fan boost on all fans' : nearestBoost(n) === 0 ? 'Fan boost off on all fans' : `Fan boost +${nearestBoost(n)}% on all fans`),
      run: async (n) => {
        if (!fansOk) throw new Error((controls?.fans.reasons ?? ['Fan boost is unavailable.']).join(' '));
        if (n == null) { setPage('Thermals'); return; }
        const level = nearestBoost(n);
        const out = await api.setFanBoost(fans, level);
        return out.every((c) => c.confirmed) ? (level ? `Fan boost +${level}% set and confirmed.` : 'Fan boost off: back on the firmware curve.') : `The firmware didn't confirm every fan: ${out.filter((c) => !c.confirmed).map((c) => c.fan).join(', ')}.`;
      },
    });
    for (const id of MODE_ORDER.filter((m) => available.includes(m))) {
      list.push({
        id: `mode-${id}`, group: 'Modes', title: PROFILE_NAMES[id], words: `mode thermal ${id === '0xAB' ? 'gmode g mode boost key' : ''}`,
        hint: id === '0xAB' ? 'What the boost key Fn+F1 toggles' : undefined,
        run: async () => {
          if (!modesOk) throw new Error((controls?.modes.reasons ?? ['Mode switching is unavailable.']).join(' '));
          const c = await api.setMode(id);
          return c.confirmed ? (c.unchanged ? `Already ${c.after}.` : `Switched to ${c.after}.`) : `Asked for ${c.requested}; the firmware still reports ${c.after ?? 'nothing'}.`;
        },
      });
    }
    for (const p of [...PAGES, 'Settings'] as Page[]) list.push({ id: `page-${p}`, group: 'Pages', title: p, words: 'go open page', run: () => setPage(p) });
    const extra: [string, string, string][] = [
      ['Thermals', 'Custom fan curve', 'curve fans'], ['Thermals', 'Emergency fans temperature', 'emergency safety hot'], ['Thermals', 'Boost key Fn+F1', 'boost key fn f1 gmode'],
      ['Power', 'Clocks, like CPU-Z and GPU-Z', 'ghz mhz clock core cpu gpu'], ['Power', 'Where the power goes', 'watts power battery'], ['Power', 'What is using the machine', 'processes ram cpu disk'],
      ['Lighting', 'Keyboard colours', 'keys rgb colour color'], ['Lighting', 'Screen mirror and effects', 'mirror clock breathing wave effect'], ['Lighting', 'Laser, lightning, seismic, mouse', 'key presses mouse boost trail'],
      ['Timeline', 'Logged days', 'history log day'], ['Health', 'Battery, disks, Dell services', 'battery wear disk dell drift'],
      ['Settings', 'Daily log settings', 'log keep cap'], ['Settings', 'Start at sign-in, admin', 'autostart admin startup'],
    ];
    for (const [p, title, words] of extra) list.push({ id: `go-${title}`, group: 'Go to', title, hint: p, words, run: () => setPage(p as Page) });
    if (settings) {
      for (const t of ['dark', 'gray', 'light'] as const) list.push({ id: `theme-${t}`, group: 'Settings', title: `Theme: ${t[0].toUpperCase()}${t.slice(1)}`, words: 'appearance colour', run: () => save({ ...settings, theme: t }) });
      list.push({ id: 'text', group: 'Settings', title: settings.effects.largeText ? 'Larger text: off' : 'Larger text: on', words: 'font size', run: () => save({ ...settings, effects: { ...settings.effects, largeText: !settings.effects.largeText } }) });
      list.push({ id: 'notify', group: 'Settings', title: settings.notify ? 'Windows notifications: off' : 'Windows notifications: on', words: 'alerts', run: () => save({ ...settings, notify: !settings.notify }) });
    }
    return list;
  }, [status, controls, settings, save]);

  const src = SOURCE[status?.source ?? 'unavailable'];
  const sensor = (n: string) => status?.snapshot?.sensors.find((x) => x.name === n)?.celsius;
  const go = (p: Page) => setPage(p);
  return (
    <div className="app">
      <header className="topbar" data-tauri-drag-region>
        <div className="brand" data-tauri-drag-region><Mark /><span className="brand-nm" data-tauri-drag-region>Alien Cockpit</span><span className="role">OSS</span></div>
        <nav className="tabsbar">
          {PAGES.map((p) => <button key={p} className={p === page ? 'on' : ''} aria-current={p === page ? 'page' : undefined} onClick={() => setPage(p)}>{p}</button>)}
        </nav>
        <span className="spacer" data-tauri-drag-region />
        {page !== 'Cockpit' && status?.snapshot && (
          <Tip text="The CPU and GPU temperatures and the thermal mode, on every page. The Cockpit shows them in full.">
            <button className="pill vpill" onClick={() => setPage('Cockpit')}>CPU <b className="mono">{sensor('CPU') ?? '—'}°</b> GPU <b className="mono">{sensor('GPU') ?? '—'}°</b> <b>{status.snapshot.profile_name ?? ''}</b></button>
          </Tip>
        )}
        <Tip text="Any page, mode, fan boost level or setting: type, then Enter. Ctrl K from anywhere in the window.">
          <button className="pill find" onClick={() => setFinding(true)}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="6" /><path d="M20 20l-4.5-4.5" /></svg>
            <span className="find-t">Find anything</span><span className="mono find-k">Ctrl K</span>
          </button>
        </Tip>
        {api.inApp && status?.source === 'unavailable' && (
          <Tip text="The Alienware firmware only answers administrators. Restarts Alien Cockpit as administrator (one Windows prompt)."><button className="btn small-btn" onClick={() => api.restartElevated().catch((e) => setError(String(e)))}>Get live readings</button></Tip>
        )}
        <Tip text={status?.error ? `${src.long} (${status.error})` : src.long}><span className="pill"><i className={`dot ${src.dot}`} /><span className="pill-text">{src.text}</span></span></Tip>
        <div className="winbtns">
          <button aria-label="Settings" className={page === 'Settings' ? 'on' : ''} onClick={() => setPage(page === 'Settings' ? 'Cockpit' : 'Settings')}><svg viewBox="0 0 24 24" className="gear"><path d="M4 7h10M18 7h2M4 17h4M12 17h8" /><circle cx="16" cy="7" r="2" /><circle cx="10" cy="17" r="2" /></svg></button>
          <button aria-label="Minimize" onClick={() => win('min')}><svg viewBox="0 0 12 12"><path d="M2 6.5h8" /></svg></button>
          <button aria-label="Maximize" onClick={() => win('max')}><svg viewBox="0 0 12 12"><rect x="2.5" y="2.5" width="7" height="7" rx="1.5" /></svg></button>
          <button aria-label="Close to tray" className="close" onClick={() => win('close')}><svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6" /></svg></button>
        </div>
      </header>
      <main className="main" ref={mainRef}>
        {page === 'Cockpit' && <Overview status={status} history={history} controls={controls} go={go} />}
        {page === 'Thermals' && <Thermals status={status} history={history} controls={controls} settings={settings} save={save} />}
        {page === 'Power' && <PowerPage history={history} />}
        {page === 'Lighting' && <Lighting />}
        {page === 'Timeline' && <Timeline live={history} />}
        {page === 'Health' && <HealthView />}
        {page === 'Settings' && <SettingsView settings={settings} save={save} error={error} go={go} />}
      </main>
      {finding && <Palette commands={commands} onClose={() => setFinding(false)} onDone={(text, ok) => setToast({ text, ok })} />}
      {toast && <div className={`toast ${toast.ok ? 'ok' : 'bad'}`} role="status">{toast.text}</div>}
    </div>
  );
}
