// Dark is the default. The choice is saved in the app's settings; a local copy (when storage works) only
// avoids a flash of the wrong theme while the settings load.
export type Theme = 'dark' | 'gray' | 'light';

export function applyTheme(t: Theme) {
  const theme: Theme = t === 'gray' || t === 'light' ? t : 'dark';
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem('x17-theme', theme);
  } catch {
    /* private mode or blocked storage: the saved setting still applies */
  }
}

export function initialTheme(): Theme {
  try {
    const t = localStorage.getItem('x17-theme');
    if (t === 'gray' || t === 'light' || t === 'dark') return t;
  } catch {
    /* ignore */
  }
  return 'dark';
}
