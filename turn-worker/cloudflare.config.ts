// The ManyDesk relay: hands each app short-lived Cloudflare TURN credentials, so the
// TURN key's secret stays here and never ships inside ManyDesk.
import { bindings, defineConfig } from "cf/config";
import * as entrypoint from "./src/index.ts" with { type: "cf-worker" };

export default defineConfig({
  worker: {
    name: "manydesk-turn",
    entrypoint,
    compatibilityDate: "2026-11-01",
    workersDev: true,
    env: {
      TURN_KEY_ID: bindings.secret(),
      TURN_KEY_SECRET: bindings.secret(),
      // per IP, per Cloudflare location; the app asks about once a day
      LIMIT: bindings.rateLimit({ namespace: "4701", simple: { limit: 10, period: 60 } }),
    },
  },
});
