import path from 'node:path';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { '@': path.resolve(__dirname, 'src') },
    // `@refinest/react` and `@refinest/ui-shadcn` declare react as a peer.
    // If an install ever leaves a nested copy, their hooks run against a
    // different React instance than the one that rendered them ("Invalid
    // hook call"). Forcing one resolution is React's own monorepo advice.
    dedupe: ['react', 'react-dom'],
  },
  // `npm run dev` in front of a running stack. The container build proxies the
  // same three prefixes in nginx (nginx.conf.template) — the panel, the engine
  // and the identity provider have to share one origin, or neither the
  // engine's session cookie nor the provider's comes back.
  //
  //   DONAT_UPSTREAM=http://localhost:8090 npm run dev
  //
  // `/auth/v1` needs no entry of its own: the engine serves the provider's
  // login API there itself (DONAT_OIDC.login_api).
  server: {
    port: 5174,
    proxy: {
      ...(process.env.DONAT_UPSTREAM
        ? {
            '/auth': { target: process.env.DONAT_UPSTREAM, changeOrigin: false },
            '/v1': { target: process.env.DONAT_UPSTREAM, changeOrigin: false, ws: true },
          }
        : {}),
    },
  },
});
