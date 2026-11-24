// The Lighting page: the keyboard's colours per key, brightness and effects, when it goes dark, and the zone
// lights. Contract: docs/lighting-control.md. The decisions live in logic.ts.
import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';
import { api, type KeyboardLight, type KeyEffect, type KeyInfo, type LightingSettings, type LightingStatus, type LiveSettings, type Rgb } from './api';
import { Section, Tip } from './components';
import { debounce, effectInfo, EFFECTS, groupMembers, LIVE_KINDS, liveOf, liveSummary, MOUSE_MODES, TAP_KINDS, toggleTap, keyAt, keysAlong, keysInRect, normHex, palette, selectionColor, shownColors, toggleKey, unionKeys, zoneLabel } from './logic';

/** Live preview writes one key per call, so only small selections preview while the colour moves; bigger ones
 * show on the keyboard when the change is saved (250 ms after it stops). */
const PREVIEW_MAX = 6;
const SAVE_MS = 250;

// ---------------------------------------------------------------- the keyboard map

const U = 40;
const GAP = 4;
const SHORT: Record<string, string> = {
  'Speaker Mute': 'Mute', 'Volume Down': 'Vol −', 'Volume Up': 'Vol +', 'Microphone Mute': 'Mic', Delete: 'Del',
  'Left Shift': 'Shift', 'Right Shift': 'Shift', 'Left Ctrl': 'Ctrl', 'Right Ctrl': 'Ctrl', 'Left Alt': 'Alt', 'Right Alt': 'Alt',
  'Left Windows': 'Win', 'Right Windows': 'Win', 'Page Up': 'PgUp', 'Page Down': 'PgDn', 'Page Down (2)': 'PgDn',
  'Up Arrow': '↑', 'Left Arrow': '←', 'Right Arrow': '→', 'Down Arrow': '↓', 'Caps Lock': 'Caps', Backspace: 'Backspace',
  'Up / Page Up': '↑', 'Down / Page Down': '↓', 'Windows lock': 'Win',
};
/** The x17 R2's keycap legends, as on the real keys and in AWCC's view: the main legend, and the second one
 * printed below it (the shifted symbol, or the Fn function). */
const LEGENDS: Record<string, { main: string; sub?: string }> = {
  ESC: { main: 'ESC' }, '`': { main: '`', sub: '~' }, '1': { main: '1', sub: '!' }, '2': { main: '2', sub: '@' }, '3': { main: '3', sub: '#' },
  '4': { main: '4', sub: '$' }, '5': { main: '5', sub: '%' }, '6': { main: '6', sub: '^' }, '7': { main: '7', sub: '&' }, '8': { main: '8', sub: '*' },
  '9': { main: '9', sub: '(' }, '0': { main: '0', sub: ')' }, '-': { main: '-', sub: '_' }, '=': { main: '=', sub: '+' },
  '[': { main: '[', sub: '{' }, ']': { main: ']', sub: '}' }, '|': { main: '\\', sub: '|' }, ';': { main: ';', sub: ':' }, "'": { main: "'", sub: '"' },
  ',': { main: ',', sub: '<' }, '.': { main: '.', sub: '>' }, '/': { main: '/', sub: '?' },
  Home: { main: 'HOME', sub: 'PRT SCRN' }, End: { main: 'END', sub: 'PAUSE' }, Delete: { main: 'DEL', sub: 'INSERT' },
  Backspace: { main: '⟵' }, Tab: { main: 'TAB' }, 'Caps Lock': { main: 'CAPS' }, Enter: { main: '↵' },
  'Left Shift': { main: 'SHIFT' }, 'Right Shift': { main: 'SHIFT' }, 'Left Ctrl': { main: 'CTRL' }, 'Right Ctrl': { main: 'CTRL' },
  Fn: { main: 'FN' }, 'Left Windows': { main: '⊞' }, 'Windows lock': { main: '⊞', sub: 'LOCK' }, 'Left Alt': { main: 'ALT' }, 'Right Alt': { main: 'ALT' },
  'Up / Page Up': { main: '▲', sub: 'PAGE UP' }, 'Down / Page Down': { main: '▼', sub: 'PAGE DN' }, 'Left Arrow': { main: '◀' }, 'Right Arrow': { main: '▶' },
  'Microphone Mute': { main: 'MIC', sub: '×' }, 'Speaker Mute': { main: 'MUTE' }, 'Volume Up': { main: 'VOL', sub: '+' }, 'Volume Down': { main: 'VOL', sub: '−' },
};
const legendOf = (name: string) => LEGENDS[name] ?? { main: keyLabel(name).toUpperCase() };
/** A colour too dark to light a legend (the key is off or at brightness 0). */
const isDark = (c: Rgb) => { const v = parseInt(c.slice(1), 16); return ((v >> 16) & 255) + ((v >> 8) & 255) + (v & 255) < 24; };

/** The space bar: no LED and no id on the x17 R2, drawn unlit in its gap (docs/lighting-control.md). */
const SPACE = { row: 5, col: 4.25, width: 5 };
const keyLabel = (n: string) => SHORT[n] ?? n;

type Pt = { x: number; y: number };
type Drag = { kind: 'select'; start: Pt; base: number[]; moved: boolean } | { kind: 'paint'; done: Set<number>; last: Pt };

/** The keyboard drawn from `layout` (key units: col/width across, one row high). Click selects a key,
 * Shift/Ctrl-click adds or removes one, dragging draws a selection box; with the brush, click or drag paints. */
export function KeyboardMap({ layout, colors, names, selected, editable, tool, onSelect, onPaint, onStroke }: {
  layout: KeyInfo[];
  colors: Record<number, Rgb>;
  names?: Record<number, string>;
  selected: number[];
  editable: boolean;
  tool: 'select' | 'paint';
  onSelect: (ids: number[]) => void;
  onPaint: (id: number) => void;
  onStroke: () => void;
}) {
  const svg = useRef<SVGSVGElement>(null);
  const drag = useRef<Drag | null>(null);
  const [box, setBox] = useState<{ a: Pt; b: Pt } | null>(null);
  // Ids AWCC lists without a physical key here (international keys, a second Page Up/Down) aren't drawn.
  const all = layout;
  layout = useMemo(() => all.filter((k) => k.present !== false), [all]);
  const cols = Math.max(1, ...layout.map((k) => k.col + k.width));
  const rows = Math.max(1, ...layout.map((k) => k.row + 1));
  const sel = useMemo(() => new Set(selected), [selected]);

  const at = (e: ReactPointerEvent): Pt => {
    const r = svg.current!.getBoundingClientRect();
    return { x: ((e.clientX - r.left) / r.width) * cols, y: ((e.clientY - r.top) / r.height) * rows };
  };
  const down = (e: ReactPointerEvent) => {
    if (!editable || e.button !== 0) return;
    const p = at(e);
    const id = keyAt(layout, p.x, p.y);
    svg.current!.setPointerCapture(e.pointerId);
    if (tool === 'paint') {
      drag.current = { kind: 'paint', done: new Set(id != null ? [id] : []), last: p };
      if (id != null) onPaint(id);
      return;
    }
    const add = e.shiftKey || e.ctrlKey || e.metaKey;
    drag.current = { kind: 'select', start: p, base: add ? selected : [], moved: false };
    onSelect(id == null ? (add ? selected : []) : add ? toggleKey(selected, id) : [id]);
  };
  const move = (e: ReactPointerEvent) => {
    const d = drag.current;
    if (!d) return;
    const p = at(e);
    if (d.kind === 'paint') {
      for (const id of keysAlong(layout, d.last, p)) if (!d.done.has(id)) { d.done.add(id); onPaint(id); }
      d.last = p;
      return;
    }
    if (!d.moved && Math.hypot(p.x - d.start.x, p.y - d.start.y) < 0.35) return;
    d.moved = true;
    setBox({ a: d.start, b: p });
    onSelect(unionKeys(d.base, keysInRect(layout, d.start, p)));
  };
  const up = () => {
    const d = drag.current;
    drag.current = null;
    setBox(null);
    if (d?.kind === 'paint') onStroke();
  };

  return (
    <div className={`kbmap ${editable ? `editable ${tool}` : ''}`}>
      <svg ref={svg} viewBox={`0 0 ${cols * U} ${rows * U}`} role="img" aria-label="Keyboard map" onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={up}>
        {rows >= 6 && (
          <g>
            <title>Space: no light on this keyboard</title>
            <rect x={SPACE.col * U + GAP / 2} y={SPACE.row * U + GAP / 2} width={SPACE.width * U - GAP} height={U - GAP} rx={6} className="kb-cap kb-unlit" />
          </g>
        )}
        {layout.map((k) => {
          const c = colors[k.id] ?? '#000000';
          const x = k.col * U + GAP / 2, y = k.row * U + GAP / 2, w = k.width * U - GAP, h = U - GAP;
          const on = sel.has(k.id);
          // Like AWCC and the real keyboard: a dark keycap whose legends carry the key's light, with a faint edge
          // in the same colour. A dark key (off, or brightness 0) keeps grey legends so the map stays readable.
          const lit = !isDark(c);
          const ink = lit ? c : 'var(--kb-unlit-ink)';
          const lg = legendOf(k.name);
          const word = lg.main.length > 1 && /[A-Z]/.test(lg.main);
          return (
            <g key={k.id}>
              <title>{names?.[k.id] ?? k.name}</title>
              <rect x={x} y={y} width={w} height={h} rx={6} className="kb-cap" style={lit ? { stroke: c, strokeOpacity: 0.45 } : undefined} />
              {lg.sub ? (
                <>
                  <text x={x + w / 2} y={y + h * 0.36} className={`kb-lbl ${word ? 'word' : ''}`} fill={ink}>{lg.main}</text>
                  <text x={x + w / 2} y={y + h * 0.72} className="kb-lbl sub" fill={ink}>{lg.sub}</text>
                </>
              ) : (
                <text x={x + w / 2} y={y + h / 2} className={`kb-lbl ${word ? 'word' : ''}`} fill={ink}>{lg.main}</text>
              )}
              {on && <rect x={x - 1} y={y - 1} width={w + 2} height={h + 2} rx={7} className="kb-ring" />}
              {on && <rect x={x + 1.5} y={y + 1.5} width={w - 3} height={h - 3} rx={4.5} className="kb-ring-in" />}
            </g>
          );
        })}
        {box && (
          <rect className="kb-box" x={Math.min(box.a.x, box.b.x) * U} y={Math.min(box.a.y, box.b.y) * U} width={Math.abs(box.a.x - box.b.x) * U} height={Math.abs(box.a.y - box.b.y) * U} />
        )}
      </svg>
    </div>
  );
}

// ---------------------------------------------------------------- small inputs

/** A colour: the system picker plus the hex, typed. Typing commits on Enter or leaving the field. */
function ColourField({ value, onChange, onCommit, disabled, label }: { value: Rgb; onChange: (c: Rgb) => void; onCommit?: () => void; disabled?: boolean; label: string }) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  const ok = normHex(text) != null;
  const commit = () => {
    const c = normHex(text);
    if (c && c !== value) { onChange(c); onCommit?.(); }
    else setText(value);
  };
  return (
    <span className="colour">
      <input type="color" aria-label={label} value={value.toLowerCase()} disabled={disabled} onChange={(e) => onChange(e.target.value.toUpperCase())} onBlur={onCommit} />
      <input className={`hexin ${ok ? '' : 'bad'}`} aria-label={`${label}, as #RRGGBB`} value={text} disabled={disabled} spellCheck={false} maxLength={7}
        onChange={(e) => setText(e.target.value)} onBlur={commit} onKeyDown={(e) => { if (e.key === 'Enter') commit(); if (e.key === 'Escape') setText(value); }} />
    </span>
  );
}

function Swatches({ colors, current, onPick, disabled }: { colors: Rgb[]; current: Rgb | null; onPick: (c: Rgb) => void; disabled?: boolean }) {
  return (
    <div className="swatches" role="group" aria-label="AWCC colours">
      {colors.map((c) => <button key={c} className={`swatch ${current === c ? 'on' : ''}`} style={{ background: c }} title={c} aria-label={c} disabled={disabled} onClick={() => onPick(c)} />)}
    </div>
  );
}

function Switch({ checked, onChange, name, what }: { checked: boolean; onChange: (v: boolean) => void; name: string; what?: string }) {
  return (
    <label className="switch">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="sw" aria-hidden />
      <span><b>{name}</b>{what && <small className="muted">{what}</small>}</span>
    </label>
  );
}

// ---------------------------------------------------------------- the page

type Problem = { text: string; rolledBack: boolean };

export function Lighting() {
  const [st, setSt] = useState<LightingStatus | null>(null);
  const [draft, setDraftState] = useState<LightingSettings | null>(null);
  const draftRef = useRef<LightingSettings | null>(null);
  const saved = useRef<LightingSettings | null>(null);
  const seq = useRef(0);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [busy, setBusy] = useState<'load' | 'restore' | 'import' | null>('load');
  const [sel, setSel] = useState<number[]>([]);
  const [tool, setTool] = useState<'select' | 'paint'>('select');
  const [brush, setBrush] = useState<Rgb>('#FF00FF');

  const setDraft = (s: LightingSettings) => { draftRef.current = s; setDraftState(s); };
  const take = (s: LightingStatus) => { saved.current = s.settings; setSt(s); };
  const fail = (e: unknown) => {
    setProblem({ text: String(e), rolledBack: saved.current != null });
    if (saved.current) setDraft(saved.current);
  };

  // Saving: the newest settings, 250 ms after the last change or at once on release. A refusal puts the page
  // back to the last saved settings; answers to older requests are ignored.
  const sendRef = useRef<(s: LightingSettings) => void>(() => {});
  const saver = useMemo(() => debounce<LightingSettings>((s) => sendRef.current(s), SAVE_MS), []);
  sendRef.current = (s) => {
    const n = ++seq.current;
    api.lightingSet(s).then(
      (r) => {
        saved.current = r.settings;
        if (n !== seq.current) return;
        setSt(r);
        setProblem(null);
        if (!saver.pending) setDraft(r.settings);
      },
      (e) => {
        if (n !== seq.current) return;
        saver.cancel();
        fail(e);
      },
    );
  };
  // Live key preview while a colour moves (one key per call, not saved).
  const preview = useMemo(() => debounce<{ ids: number[]; color: Rgb }>(({ ids, color }) => {
    for (const id of ids) api.lightingPreviewKey(id, color).catch((e) => setProblem({ text: String(e), rolledBack: false }));
  }, 40, 80), []);

  useEffect(() => {
    let live = true;
    api.lightingStatus().then(
      (s) => { if (!live) return; take(s); setDraft(s.settings); setBusy(null); },
      (e) => { if (!live) return; setProblem({ text: String(e), rolledBack: false }); setBusy(null); },
    );
    return () => { live = false; saver.flush(); preview.cancel(); };
  }, [saver, preview]);

  const update = (next: LightingSettings, now = false) => {
    setDraft(next);
    saver.schedule(next);
    if (now) saver.flush();
  };
  const setKb = (patch: Partial<KeyboardLight>, now = false) => {
    const d = draftRef.current;
    if (d) update({ ...d, keyboard: { ...d.keyboard, ...patch } }, now);
  };
  const setLive = (patch: Partial<LiveSettings>, now = false) => {
    const d = draftRef.current;
    if (d) update({ ...d, live: { ...liveOf(d), ...patch } }, now);
  };
  // Layer 2: one effect, either drawn by the app (clock, screen mirror) or the keyboard's own.
  const pickEffect = (id: 'none' | 'clock' | 'screen' | KeyEffect) => {
    const d = draftRef.current;
    if (!d) return;
    const app = id === 'clock' || id === 'screen';
    const effect: KeyEffect = app || id === 'none' ? 'static' : id;
    update({ ...d, keyboard: { ...d.keyboard, effect }, live: { ...liveOf(d), kind: app ? id : 'off' } }, true);
  };
  const commit = () => saver.flush();
  const canPreview = () => draftRef.current?.mode === 'custom' && draftRef.current.keyboard.effect === 'static';

  const restore = async () => {
    saver.cancel();
    preview.cancel();
    seq.current++;
    setBusy('restore');
    try {
      const r = await api.lightingRestoreAwcc();
      take(r);
      setDraft(r.settings);
      setProblem(null);
      setSel([]);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(null);
    }
  };
  const startFromAwcc = async () => {
    saver.cancel();
    preview.cancel();
    seq.current++;
    setBusy('import');
    try {
      let r = await api.lightingImportAwcc();
      if (r.settings.mode !== 'custom') r = await api.lightingSet({ ...r.settings, mode: 'custom' });
      take(r);
      setDraft(r.settings);
      setProblem(null);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(null);
    }
  };
  const goCustom = () => {
    const d = draftRef.current;
    if (!d || d.mode === 'custom') return;
    const blank = Object.keys(d.keyboard.keys).length === 0 && Object.keys(d.keyboard.groups).length === 0;
    if (blank && st?.awcc) startFromAwcc();
    else update({ ...d, mode: 'custom' }, true);
  };

  const colourKeys = (ids: number[], c: Rgb, now = false) => {
    const d = draftRef.current;
    if (!d || ids.length === 0) return;
    const keys = { ...d.keyboard.keys };
    for (const id of ids) keys[id] = c;
    setKb({ keys }, now);
    if (!now && ids.length <= PREVIEW_MAX && canPreview()) preview.schedule({ ids, color: c });
  };
  const clearKeys = (ids: number[]) => {
    const d = draftRef.current;
    if (!d) return;
    const keys = { ...d.keyboard.keys };
    for (const id of ids) delete keys[id];
    setKb({ keys }, true);
  };
  const paint = (id: number) => {
    const d = draftRef.current;
    if (!d) return;
    setDraft({ ...d, keyboard: { ...d.keyboard, keys: { ...d.keyboard.keys, [id]: brush } } });
    saver.schedule(draftRef.current!);
    if (canPreview()) api.lightingPreviewKey(id, brush).catch((e) => setProblem({ text: String(e), rolledBack: false }));
  };

  const layout = st?.layout ?? [];
  const groups = st?.groups ?? [];
  const pal = useMemo(() => palette(st?.awcc?.keys), [st?.awcc]);
  const names = useMemo(() => Object.fromEntries(layout.map((k) => [k.id, k.name])), [layout]);

  if (!st || !draft) {
    return (
      <div className="cockpit">
        <Section title="Keyboard lighting" className="span3">
          <p className="muted">{busy === 'load' ? 'Reading the keyboard…' : `Couldn't read the lighting: ${problem?.text ?? 'no answer'}`}</p>
        </Section>
      </div>
    );
  }

  const kb = draft.keyboard;
  const custom = draft.mode === 'custom';
  const fx = effectInfo(kb.effect);
  const perKey = custom && kb.effect === 'static';
  const colors = custom ? shownColors(layout, kb) : st.awcc ? st.awcc.keys : shownColors(layout, kb);
  const selColour = selectionColor(layout, kb, sel);
  const isGroup = (g: string) => { const m = groupMembers(layout, g); return m.length > 0 && m.length === sel.length && m.every((id) => sel.includes(id)); };
  const own = sel.filter((id) => kb.keys[id] != null).length;
  const zoneIds = Object.keys(draft.zones).map(Number).sort((a, b) => a - b);
  const last = st.lastApply;
  const live = liveOf(draft);
  const hardware = kb.effect !== 'static' && live.kind !== 'clock' && live.kind !== 'screen';
  const effectId = live.kind === 'clock' || live.kind === 'screen' ? live.kind : kb.effect === 'static' ? 'none' : kb.effect;
  const effectWhat = hardware ? fx.what : (LIVE_KINDS.find((k) => k.id === (effectId === 'none' ? 'off' : effectId)) ?? LIVE_KINDS[0]).what;
  const mouseMode = MOUSE_MODES.find((k) => k.id === live.mouse) ?? MOUSE_MODES[0];
  const liveNow = liveSummary(live);
  const displayOff = draft.clockOnDisplayOff ? 'clock' : draft.offOnDisplayOff ? 'dark' : 'keep';

  return (
    <div className="cockpit">
      <Section title="Keyboard lighting" className="span3" right={last && <span className={last.ok ? 'lbl ok-t' : 'lbl warn-t'}>{last.ok ? `applied ${new Date(last.at).toLocaleTimeString()}` : 'last write failed'}</span>}>
        <div className="lt-head">
          <div className="seg" role="radiogroup" aria-label="Who sets the keyboard's lighting">
            <button role="radio" aria-checked={!custom} className={!custom ? 'on' : ''} disabled={busy != null} onClick={() => custom && restore()}>Your AWCC look</button>
            <button role="radio" aria-checked={custom} className={custom ? 'on' : ''} disabled={busy != null} onClick={goCustom}>Custom</button>
          </div>
          <div className="lt-actions">
            <Tip text={`Writes AWCC's own preset${st.awcc ? ` "${st.awcc.preset}"` : ''} to all keys and stops the app writing.`}>
              <button className={`chip ${busy === 'restore' ? 'pending' : ''}`} disabled={busy != null} onClick={restore}>Back to my AWCC look</button>
            </Tip>
            <Tip text="Copies AWCC's colours, key by key, into Custom so you can change them from there. Replaces your custom key colours.">
              <button className={`chip ${busy === 'import' ? 'pending' : ''}`} disabled={busy != null || !st.awcc} onClick={startFromAwcc}>Start from my AWCC look</button>
            </Tip>
          </div>
        </div>
        <p className="muted small lt-why">
          {custom
            ? 'Custom: the app writes these colours to the keyboard, and again after sleep and wake. AWCC stays installed; its preset is untouched and can be put back any time.'
            : `Your AWCC look: the app doesn't write to the keyboard${st.awcc ? `; AWCC's preset "${st.awcc.preset}" stays as it is` : ''}. Choose Custom to set your own colours.`}
        </p>
        {last && !last.ok && <p className="m-res bad">{last.text}</p>}
        {!st.device.keyboard && <p className="m-res bad">The keyboard wasn't found{st.device.error ? `: ${st.device.error}` : '.'} Settings are still saved.</p>}
        {st.device.keyboard && st.device.error && <p className="m-res bad">{st.device.error}</p>}
      </Section>

      {problem && (
        <div className="note warn span3" role="alert">
          {problem.rolledBack ? `That change wasn't applied: ${problem.text} The page shows the last saved settings again.` : problem.text}
        </div>
      )}

      <Section title={custom ? (perKey ? '1 · Base profile · click, Shift/Ctrl-click or drag' : `1 · Base profile · ${fx.name} runs on the keyboard`) : "Keyboard map · AWCC's look"} className="span2"
        right={perKey && (
          <div className="seg small" role="radiogroup" aria-label="Map tool">
            <button role="radio" aria-checked={tool === 'select'} className={tool === 'select' ? 'on' : ''} onClick={() => setTool('select')}>Select</button>
            <button role="radio" aria-checked={tool === 'paint'} className={tool === 'paint' ? 'on' : ''} onClick={() => setTool('paint')}>Paint</button>
          </div>
        )}>
        <KeyboardMap layout={layout} colors={colors} names={names} selected={perKey && tool === 'select' ? sel : []} editable={perKey} tool={tool}
          onSelect={setSel} onPaint={paint} onStroke={commit} />
        {perKey && (
          <div className="row">
            {groups.map((g) => <button key={g} className={`chip ${isGroup(g) ? 'on' : ''}`} onClick={() => { setTool('select'); setSel(groupMembers(layout, g)); }}>{g}</button>)}
            <button className="chip" onClick={() => { setTool('select'); setSel(layout.map((k) => k.id)); }}>All keys</button>
            <button className="chip" disabled={sel.length === 0} onClick={() => setSel([])}>None</button>
          </div>
        )}
        <p className="muted small">
          {!custom ? `${layout.filter((k) => k.present !== false).length} keys as AWCC lights them.`
            : !perKey ? `${fx.what} Per-key colours are kept and come back with Static.`
            : !kb.on ? 'The keyboard is off: the map shows it dark. Colours are kept.'
            : `Shown at ${kb.brightness}% brightness.`}
        </p>
      </Section>

      {custom ? (
        <Section title={tool === 'paint' && perKey ? 'Brush' : sel.length ? `${sel.length} key${sel.length > 1 ? 's' : ''} selected` : 'Selected keys'}>
          {!perKey ? (
            <p className="muted small">Choose Static under Whole keyboard to colour keys one by one.</p>
          ) : tool === 'paint' ? (
            <>
              <ColourField label="Brush colour" value={brush} onChange={setBrush} />
              <Swatches colors={pal} current={brush} onPick={setBrush} />
              <p className="muted small">Click or drag over keys to paint them. The change is saved when you let go.</p>
            </>
          ) : (
            <>
              <ColourField label="Colour of the selected keys" value={selColour ?? '#000000'} disabled={sel.length === 0} onChange={(c) => colourKeys(sel, c)} onCommit={commit} />
              <Swatches colors={pal} current={selColour} disabled={sel.length === 0} onPick={(c) => colourKeys(sel, c, true)} />
              {sel.length === 0 ? <p className="muted small">Select keys on the map, or pick a group.</p> : (
                <div className="row">
                  <Tip text="Removes the selected keys' own colours, so they show their group's colour or the base colour.">
                    <button className="chip" disabled={own === 0} onClick={() => clearKeys(sel)}>Use the base colour</button>
                  </Tip>
                  <span className="muted small">{own ? `${own} with their own colour` : 'All on the base or group colour'}</span>
                </div>
              )}
            </>
          )}
        </Section>
      ) : (
        <Section title="AWCC's look">
          {st.awcc ? (
            <>
              <p>Preset <b>"{st.awcc.preset}"</b>: {new Set(Object.values(st.awcc.keys)).size} colours over {layout.filter((k) => k.present !== false).length} keys.</p>
              <Swatches colors={pal.slice(0, new Set(Object.values(st.awcc.keys)).size)} current={null} onPick={() => {}} disabled />
              <p className="muted small">Start from it to change single keys; it stays in AWCC either way.</p>
            </>
          ) : <p className="muted">AWCC's lighting preset couldn't be read.</p>}
        </Section>
      )}

      {custom && (
        <Section title="Base profile · whole keyboard" className="span2">
          <div className="lt-grid">
            <Switch checked={kb.on} onChange={(v) => setKb({ on: v }, true)} name={kb.on ? 'Backlight on' : 'Backlight off'} what="Off keeps every colour for when it's back on." />
            <label className="field">
              <span className="lbl">Brightness {kb.brightness}%</span>
              <input type="range" min={0} max={100} value={kb.brightness} disabled={!kb.on}
                onChange={(e) => setKb({ brightness: Number(e.target.value) })} onPointerUp={commit} onKeyUp={commit} />
            </label>
            <div className="field">
              <span className="lbl">Base colour</span>
              <ColourField label="Base colour" value={kb.color} onChange={(c) => setKb({ color: c })} onCommit={commit} />
              <small className="muted">{hardware ? `The colour ${fx.name.toLowerCase()} uses.` : 'For keys without their own or a group colour.'}</small>
            </div>
          </div>
          <p className="muted small">Your colours on the map are the base profile. The effect and the live interactions draw over them.</p>
        </Section>
      )}

      {custom && (
        <Section title="2 · Effect" className="span3">
          <div className="seg small" role="radiogroup" aria-label="Effect">
            <button role="radio" aria-checked={effectId === 'none'} className={effectId === 'none' ? 'on' : ''} onClick={() => pickEffect('none')}>None</button>
            <button role="radio" aria-checked={effectId === 'clock'} className={effectId === 'clock' ? 'on' : ''} onClick={() => pickEffect('clock')}>Clock</button>
            <button role="radio" aria-checked={effectId === 'screen'} className={effectId === 'screen' ? 'on' : ''} onClick={() => pickEffect('screen')}>Screen mirror</button>
            {EFFECTS.filter((e) => e.id !== 'static').map((e) => <button key={e.id} role="radio" aria-checked={effectId === e.id} className={effectId === e.id ? 'on' : ''} onClick={() => pickEffect(e.id)}>{e.name}</button>)}
          </div>
          <p className="small">{effectWhat}</p>
          {hardware && (fx.tempo || fx.color2) && (
            <div className="lt-grid">
              {fx.tempo && (
                <label className="field">
                  <span className="lbl">Tempo {kb.tempo}</span>
                  <input type="range" min={0} max={255} value={kb.tempo} onChange={(e) => setKb({ tempo: Number(e.target.value) })} onPointerUp={commit} onKeyUp={commit} />
                </label>
              )}
              {fx.color2 && (
                <div className="field">
                  <span className="lbl">Second colour</span>
                  <ColourField label="Second colour" value={kb.color2} onChange={(c) => setKb({ color2: c })} onCommit={commit} />
                </div>
              )}
            </div>
          )}
          {live.kind === 'screen' && (
            <div className="lt-grid">
              <label className="field">
                <span className="lbl">Variety {Math.round(live.spread * 100)}%</span>
                <input type="range" min={0} max={100} step={5} value={Math.round(live.spread * 100)} onChange={(e) => setLive({ spread: Number(e.target.value) / 100 })} onPointerUp={commit} onKeyUp={commit} />
                <small className="muted">0% is one colour, the screen's average; 100% each area its own.</small>
              </label>
              <label className="field">
                <span className="lbl">Colour depth {live.saturation.toFixed(1)}x</span>
                <input type="range" min={1} max={4} step={0.1} value={live.saturation} onChange={(e) => setLive({ saturation: Number(e.target.value) })} onPointerUp={commit} onKeyUp={commit} />
              </label>
              <label className="field">
                <span className="lbl">Follows within {(live.smoothMs / 1000).toFixed(1)} s</span>
                <input type="range" min={0} max={3000} step={100} value={live.smoothMs} onChange={(e) => setLive({ smoothMs: Number(e.target.value) })} onPointerUp={commit} onKeyUp={commit} />
              </label>
              <div className="field">
                <span className="lbl">Read the screen on</span>
                <div className="seg small" role="radiogroup" aria-label="Screen reader">
                  <button role="radio" aria-checked={live.gpu} className={live.gpu ? 'on' : ''} onClick={() => setLive({ gpu: true }, true)}>Graphics card</button>
                  <button role="radio" aria-checked={!live.gpu} className={!live.gpu ? 'on' : ''} onClick={() => setLive({ gpu: false }, true)}>Processor</button>
                </div>
                <small className="muted">{live.gpu ? 'About 3% of one processor core; the graphics driver reserves about 215 MB.' : 'About 13% of one processor core, 2 MB.'}</small>
              </div>
            </div>
          )}
          <p className="muted small">{hardware
            ? 'This one runs on the keyboard\'s own chip: smooth, but in place of your colours, so the live interactions rest while it runs.'
            : 'None, Clock and Screen mirror are drawn by the app, with the live interactions on top. The others run on the keyboard\'s own chip.'}</p>
        </Section>
      )}

      {custom && (
        <Section title="3 · Live interactions" className="span3" right={liveNow && !hardware && <span className="lbl ok-t">{liveNow} {kb.on && kb.brightness > 0 ? 'on' : '(waits for the backlight)'}</span>}>
          {hardware && <p className="m-res">They rest while {fx.name} runs on the keyboard. Choose None, Clock or Screen mirror under Effect to use them.</p>}
          <div className="field lt-fx">
            <span className="lbl">Key presses</span>
            <div className="row">
              {TAP_KINDS.map((t) => <button key={t.id} role="switch" aria-checked={live.tap.includes(t.id)} className={`chip ${live.tap.includes(t.id) ? 'on' : ''}`} onClick={() => setLive({ tap: toggleTap(live.tap, t.id) }, true)}>{t.name}</button>)}
            </div>
            <p className="small">{live.tap.length ? live.tap.map((t) => TAP_KINDS.find((k) => k.id === t)!.what).join(' ') : 'Off. Pick any to combine them.'}</p>
          </div>
          {live.tap.length > 0 && (
            <div className="lt-grid">
              {(live.tap.includes('laser') || live.tap.includes('lightning')) && (
                <div className="field">
                  <span className="lbl">Beam colour</span>
                  {live.kind === 'screen' ? (
                    <small className="muted">Over the screen mirror the beams show your own key colours.</small>
                  ) : (
                    <>
                      <ColourField label="Beam colour" value={live.tapColor === 'keys' ? '#FFFFFF' : live.tapColor} disabled={live.tapColor === 'keys'} onChange={(c) => setLive({ tapColor: c })} onCommit={commit} />
                      <button className={`chip ${live.tapColor === 'keys' ? 'on' : ''}`} onClick={() => setLive({ tapColor: live.tapColor === 'keys' ? '#FFFFFF' : 'keys' }, true)}>Your key colours</button>
                    </>
                  )}
                </div>
              )}
              <label className="field">
                <span className="lbl">Speed {live.tapSpeed} keys/s</span>
                <input type="range" min={10} max={80} step={5} value={live.tapSpeed} onChange={(e) => setLive({ tapSpeed: Number(e.target.value) })} onPointerUp={commit} onKeyUp={commit} />
                {live.tap.includes('seismic') && <small className="muted">Seismic erupts in the pressed key's colour from your static colours.</small>}
              </label>
            </div>
          )}
          <div className="field lt-fx">
            <span className="lbl">Mouse</span>
            <div className="seg small" role="radiogroup" aria-label="Mouse">
              {MOUSE_MODES.map((k) => <button key={k.id} role="radio" aria-checked={live.mouse === k.id} className={live.mouse === k.id ? 'on' : ''} onClick={() => setLive({ mouse: k.id }, true)}>{k.name}</button>)}
            </div>
            <p className="small">{mouseMode.what}{live.mouse !== 'off' && ' Your screen is mapped onto the keys: top-left is Esc, bottom-right the arrows.'}</p>
          </div>
          {live.mouse !== 'off' && (
            <div className="lt-grid">
              <label className="field">
                <span className="lbl">Trail {live.fadeMs === 0 ? 'stays' : `${(live.fadeMs / 1000).toFixed(1)} s`}</span>
                <input type="range" min={0} max={6000} step={250} value={live.fadeMs} onChange={(e) => setLive({ fadeMs: Number(e.target.value) })} onPointerUp={commit} onKeyUp={commit} />
              </label>
              {live.mouse === 'boost' && live.kind === 'off' && (
                <label className="field">
                  <span className="lbl">Dimmed keys {live.dim}%</span>
                  <input type="range" min={5} max={100} step={5} value={live.dim} onChange={(e) => setLive({ dim: Number(e.target.value) })} onPointerUp={commit} onKeyUp={commit} />
                </label>
              )}
            </div>
          )}
          <p className="muted small">
            Drawn by the app while it's open, in the tray too; they pause while the keyboard is dark (sleep, lid, display off). Key presses and the pointer are read only to draw these and never stored.
          </p>
        </Section>
      )}

      <Section title="Keep dark" className={custom ? '' : 'span3'}>
        <div className="lt-stack">
          <Switch checked={draft.offOnSleep} onChange={(v) => update({ ...draft, offOnSleep: v }, true)} name="When the laptop sleeps" what="Back on when it wakes." />
          <Switch checked={draft.offOnLidClose} onChange={(v) => update({ ...draft, offOnLidClose: v }, true)} name="With the lid closed" />
          <div className="field">
            <span className="lbl">When the display turns off</span>
            <div className="seg small" role="radiogroup" aria-label="When the display turns off">
              <button role="radio" aria-checked={displayOff === 'keep'} className={displayOff === 'keep' ? 'on' : ''} onClick={() => update({ ...draft, offOnDisplayOff: false, clockOnDisplayOff: false }, true)}>Stay lit</button>
              <button role="radio" aria-checked={displayOff === 'dark'} className={displayOff === 'dark' ? 'on' : ''} onClick={() => update({ ...draft, offOnDisplayOff: true, clockOnDisplayOff: false }, true)}>Go dark</button>
              <button role="radio" aria-checked={displayOff === 'clock'} className={displayOff === 'clock' ? 'on' : ''} onClick={() => update({ ...draft, offOnDisplayOff: false, clockOnDisplayOff: true }, true)}>Show the clock</button>
            </div>
            {displayOff === 'clock' && <small className="muted">The keyboard shows the time in big digits until the display is back.</small>}
          </div>
          <Switch checked={draft.dimOnBatterySaver} onChange={(v) => update({ ...draft, dimOnBatterySaver: v }, true)} name="Dim on Energy Saver" what="30% brightness while Windows Energy Saver is on." />
          <Switch checked={draft.pauseAwcc} onChange={(v) => update({ ...draft, pauseAwcc: v }, true)} name="Pause AWCC's lighting while writing"
            what="AWCC's two lighting programs are held for the moment of each write, so only one program talks to the keyboard at a time." />
        </div>
        {!custom && <p className="muted small">These apply in Custom. With your AWCC look the app does nothing on sleep or wake.</p>}
      </Section>

      <Section title="Zone lights" className="span3" right={<span className="lbl warn-t">Sent to the controller, but these lights don't light on this laptop</span>}>
        {zoneIds.length === 0 ? <p className="muted small">No zone colours set.</p> : (
          <div className="zones">
            {zoneIds.map((id) => (
              <div key={id} className="zone">
                <ColourField label={zoneLabel(id)} value={draft.zones[id]} onChange={(c) => update({ ...draft, zones: { ...draft.zones, [id]: c } })} onCommit={commit} />
                <span className="small">{zoneLabel(id)}</span>
              </div>
            ))}
          </div>
        )}
        <p className="muted small">The zone controller takes the colours, but the lid and rear lights on this laptop stay dark. Nothing here depends on them.</p>
      </Section>
    </div>
  );
}
