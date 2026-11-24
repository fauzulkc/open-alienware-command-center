import { useCallback, useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { api, type Controls, type Point, type Settings, type Status } from './api';
import { Mark, Tip } from './components';
import { applyEffects, installEffects } from './fx';
import { mergeHistory } from './logic';
import { applyTheme, type Theme } from './theme';
import { HealthView, History, Lighting, Overview, Performance, SettingsView } from './views';

const PAGES = ['Cockpit', 'Performance', 'History', 'Lighting', 'Health', 'Settings'] as const;
type Page = (typeof PAGES)[number];

const SOURCE: Record<string, { dot: string; text: string; long: string }> = {
  service: { dot: 'live', text: 'Live', long: 'Live, from the hardware service.' },
  direct: { dot: 'live', text: 'Live', long: 'Live, read directly (Alien Cockpit runs as administrator).' },
  demo: { dot: 'demo', text: 'Demo', long: 'Demo data: the October 7 capture, not this moment.' },
  unavailable: { dot: 'off', text: 'Windows only', long: 'No firmware readings: start Alien Cockpit as administrator or install the hardware service. Windows-side numbers still show.' },
};

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
  const hist = useRef<Point[]>([]);

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
    api.settings().then((s) => { setSettings(s); applyTheme(s.theme as Theme); applyEffects(s.effects); });
    return () => { live = false; clearInterval(id); document.removeEventListener('visibilitychange', onShow); };
  }, []);

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

  const src = SOURCE[status?.source ?? 'unavailable'];
  return (
    <div className="app">
      <header className="topbar" data-tauri-drag-region>
        <div className="brand" data-tauri-drag-region><Mark /><span className="brand-nm" data-tauri-drag-region>Alien Cockpit</span><span className="role">OSS</span></div>
        <nav className="tabsbar">
          {PAGES.map((p) => <button key={p} className={p === page ? 'on' : ''} onClick={() => setPage(p)}>{p}</button>)}
        </nav>
        <span className="spacer" data-tauri-drag-region />
        {api.inApp && status?.source === 'unavailable' && (
          <Tip text="The Alienware firmware only answers administrators. Restarts Alien Cockpit as administrator (one Windows prompt)."><button className="btn small-btn" onClick={() => api.restartElevated().catch((e) => setError(String(e)))}>Get live readings</button></Tip>
        )}
        <Tip text={status?.error ? `${src.long} (${status.error})` : src.long}><span className="pill"><i className={`dot ${src.dot}`} /><span className="pill-text">{src.text}</span></span></Tip>
        {settings && (
          <div className="themes" role="radiogroup" aria-label="Theme">
            {(['dark', 'gray', 'light'] as const).map((t) => <button key={t} role="radio" aria-checked={settings.theme === t} className={settings.theme === t ? 'on' : ''} onClick={() => save({ ...settings, theme: t })}>{t}</button>)}
          </div>
        )}
        <div className="winbtns">
          <button aria-label="Minimize" onClick={() => win('min')}><svg viewBox="0 0 12 12"><path d="M2 6.5h8" /></svg></button>
          <button aria-label="Maximize" onClick={() => win('max')}><svg viewBox="0 0 12 12"><rect x="2.5" y="2.5" width="7" height="7" rx="1.5" /></svg></button>
          <button aria-label="Close to tray" className="close" onClick={() => win('close')}><svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6" /></svg></button>
        </div>
      </header>
      <main className="main">
        {page === 'Cockpit' && <Overview status={status} history={history} controls={controls} />}
        {page === 'Performance' && <Performance status={status} history={history} controls={controls} settings={settings} save={save} />}
        {page === 'History' && <History />}
        {page === 'Lighting' && <Lighting />}
        {page === 'Health' && <HealthView />}
        {page === 'Settings' && <SettingsView settings={settings} save={save} error={error} />}
      </main>
    </div>
  );
}
