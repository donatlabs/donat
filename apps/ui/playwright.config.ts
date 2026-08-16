import { defineConfig } from '@playwright/test';

/**
 * End-to-end tests, against a stack that is already running.
 *
 * They drive a real browser through the real panel, the real engine and the
 * real identity provider — which is the only place several of these behaviours
 * exist at all: a proof of work is solved in a worker, a session cookie is
 * `HttpOnly`, and "this account cannot use the panel" depends on what a token
 * the provider minted actually says.
 *
 * The stack is not started here. The repository's own `docker-compose.yml` is
 * six containers and a database migration; standing it up inside a test run
 * would make a failure mean two different things. Start it first, the way the
 * conformance harness expects a Postgres:
 *
 *   make env && docker compose up -d --build
 *   cd apps/ui && npm run e2e
 *
 * `PANEL_URL` points somewhere else. The operator's password comes from the
 * repository's `.env`, which is the file that stack was started from;
 * `PANEL_PASSWORD` overrides it.
 */
export default defineConfig({
  testDir: './e2e',
  // A login here solves a proof of work in the browser — a second or three of
  // real arithmetic — and every test signs in. On top of that the provider
  // slows down an address that has been failing, which this suite's own
  // wrong-password cases teach it to do, so a run after a run is slower.
  timeout: 180_000,
  expect: { timeout: 15_000 },
  // These share one identity provider and create accounts in it; running them
  // at once would have them delete each other's fixtures.
  workers: 1,
  fullyParallel: false,
  reporter: process.env.CI ? 'list' : [['list']],
  use: {
    baseURL: process.env.PANEL_URL ?? 'http://localhost:5180',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
});
