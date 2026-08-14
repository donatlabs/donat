import path from 'node:path';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  resolve: { alias: { '@': path.resolve(import.meta.dirname, 'src') } },
  test: {
    // The end-to-end suite is Playwright's, and it needs a running stack.
    // Without this, `vitest` picks up `e2e/*.spec.ts` and fails on an import
    // it cannot satisfy.
    exclude: ['node_modules/**', 'dist/**', 'e2e/**'],
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test-setup.ts'],
  },
});
