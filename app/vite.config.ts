import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The UI lives in ./ui. Tauri loads http://localhost:5181 in dev and ui/dist in release.
export default defineConfig({
  root: 'ui',
  plugins: [react()],
  clearScreen: false,
  server: { port: 5181, strictPort: true, host: 'localhost' },
  build: { outDir: 'dist', emptyOutDir: true, target: 'es2022' },
  test: { root: '.', include: ['ui/src/**/*.test.ts'] },
} as never);
