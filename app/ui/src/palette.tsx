// Ctrl K: every page, mode, fan boost level and common setting as a command (the redesign, 2026-11-25). Nothing
// runs until Enter; a change still goes through the same checked calls as its button and reports its result.
import { useEffect, useMemo, useRef, useState } from 'react';
import { matchCommands, queryNumber, type Command } from './logic';

export interface Runnable extends Command {
  /** Runs it; `n` is the number typed in the query, if any. Returns what happened, in words. */
  run: (n: number | null) => Promise<string | void> | string | void;
  /** The title with the typed number filled in ("Fan boost +50% on all fans"). */
  titleFor?: (n: number | null) => string;
}

export function Palette({ commands, onClose, onDone }: { commands: Runnable[]; onClose: () => void; onDone: (text: string, ok: boolean) => void }) {
  const [q, setQ] = useState('');
  const [at, setAt] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const shown = useMemo(() => matchCommands(commands, q), [commands, q]);
  const n = queryNumber(q);
  useEffect(() => { input.current?.focus(); }, []);
  useEffect(() => setAt(0), [q]);

  const run = async (c: Runnable) => {
    onClose();
    try {
      const text = await c.run(n);
      if (text) onDone(text, true);
    } catch (e) {
      onDone(String(e), false);
    }
  };
  const key = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') { e.preventDefault(); onClose(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); setAt((i) => Math.min(shown.length - 1, i + 1)); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setAt((i) => Math.max(0, i - 1)); }
    else if (e.key === 'Enter' && shown[at]) { e.preventDefault(); run(shown[at]); }
  };

  let group = '';
  return (
    <div className="pal-scrim" onPointerDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="palette" role="dialog" aria-label="Find anything">
        <label className="pal-in">
          <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="6" /><path d="M20 20l-4.5-4.5" /></svg>
          <input ref={input} value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={key} placeholder="A page, a mode, “boost 50”, a setting…" aria-label="Find anything" spellCheck={false} />
          <kbd>Esc</kbd>
        </label>
        <div className="pal-list" role="listbox">
          {shown.length === 0 && <p className="muted small" style={{ padding: '10px 12px' }}>Nothing matches.</p>}
          {shown.map((c, i) => {
            const head = c.group !== group ? (group = c.group) : null;
            return (
              <div key={c.id}>
                {head && <span className="lbl pal-group">{head}</span>}
                <button role="option" aria-selected={i === at} className={`pal-row ${i === at ? 'on' : ''}`} onPointerEnter={() => setAt(i)} onClick={() => run(c)}>
                  <span>{c.titleFor ? c.titleFor(n) : c.title}{c.hint && <small>{c.hint}</small>}</span>
                  {i === at && <kbd>Enter</kbd>}
                </button>
              </div>
            );
          })}
        </div>
        <div className="pal-foot"><span><kbd>↑</kbd> <kbd>↓</kbd> move</span><span><kbd>Enter</kbd> run</span><span className="muted">Numbers fill in the value</span></div>
      </div>
    </div>
  );
}
