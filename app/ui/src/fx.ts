// Interface effects, each switchable in Settings → Appearance (stored with the app's settings): hover (a
// colour change on controls, nothing moves or glows), press (a press and a ripple on click), large text.
// Nothing follows the pointer. They respect Windows' "reduce motion" setting regardless.
import type { Effects } from './api';

export function applyEffects(fx: Effects) {
  const d = document.documentElement.dataset;
  d.fxHover = fx.hover ? 'on' : 'off';
  d.fxPress = fx.press ? 'on' : 'off';
  d.text = fx.largeText ? 'large' : 'normal';
}

let installed = false;

/** One document listener: the press position for ripples. */
export function installEffects() {
  if (installed) return;
  installed = true;
  const root = document.documentElement;
  document.addEventListener('pointerdown', (e) => {
    if (root.dataset.fxPress !== 'on') return;
    const b = (e.target as Element | null)?.closest?.('button') as HTMLElement | null;
    if (!b || (b as HTMLButtonElement).disabled) return;
    const r = b.getBoundingClientRect();
    b.style.setProperty('--rx', `${e.clientX - r.left}px`);
    b.style.setProperty('--ry', `${e.clientY - r.top}px`);
    b.classList.remove('rippling');
    void b.offsetWidth; // restart the animation
    b.classList.add('rippling');
  }, { passive: true });
  document.addEventListener('animationend', (e) => {
    if ((e as AnimationEvent).animationName === 'ripple') (e.target as Element).classList.remove('rippling');
  });
}
