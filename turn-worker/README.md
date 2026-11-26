# ManyDesk relay

A Cloudflare Worker that hands ManyDesk short-lived Cloudflare Realtime TURN credentials. The TURN key's secret stays in the Worker, so it never ships inside the app.

- `POST /v1/ice` returns `{"iceServers": [...], "ttl": 86400}`, the answer of Cloudflare's `generate-ice-servers`. The app caches it for 24 hours.
- Each IP may ask 10 times a minute per Cloudflare location, and all callers together 1,200 times a minute per location; past either limit the answer is `429`. Requests without `cf-connecting-ip` get `400`.
- Credentials still last 24 hours: a relayed session renews its TURN allocation with them, so a shorter lifetime would cut long sessions. Bound the spend on the account instead: set a usage notification for Realtime TURN in the Cloudflare dashboard (Notifications → Billing / usage).
- The app's address for it is `RELAY` in `app/src/turncfg.rs`.

## Deploy

Requires Node.js 22.18 or later and the `cf` CLI, signed in with `cf auth login`.

```bash
npm install
cf deploy --secrets-file <file>
```

The secrets file is JSON with `TURN_KEY_ID` and `TURN_KEY_SECRET`, a key made with `cf realtime turn keys create`. Keep it outside the repository. The repository's Actions secrets `CLOUDFLARE_TURN_KEY_ID` and `CLOUDFLARE_TURN_KEY_SECRET` hold the same key.

Traffic through Cloudflare's TURN counts against the account's 1,000 GB free each month.
