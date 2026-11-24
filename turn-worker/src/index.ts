// POST /v1/ice → {"iceServers": [...]} with credentials valid for 24 h (as the app's
// Cloudflare provider caches them). Everything else is 404.
interface Env {
  TURN_KEY_ID: string;
  TURN_KEY_SECRET: string;
  LIMIT: RateLimit;
}

const TTL = 86_400;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname !== "/v1/ice") return json({ error: "not found" }, 404);
    if (req.method !== "POST") return json({ error: "use POST" }, 405);

    const ip = req.headers.get("cf-connecting-ip") ?? "unknown";
    const { success } = await env.LIMIT.limit({ key: ip });
    if (!success) return json({ error: "too many requests" }, 429);

    const res = await fetch(`https://rtc.live.cloudflare.com/v1/turn/keys/${env.TURN_KEY_ID}/credentials/generate-ice-servers`, {
      method: "POST",
      headers: { authorization: `Bearer ${env.TURN_KEY_SECRET}`, "content-type": "application/json" },
      body: JSON.stringify({ ttl: TTL }),
    });
    if (!res.ok) {
      console.error("turn upstream", res.status, await res.text());
      return json({ error: "relay unavailable" }, 502);
    }
    const data = (await res.json()) as { iceServers?: unknown };
    return json({ iceServers: data.iceServers ?? [], ttl: TTL });
  },
} satisfies ExportedHandler<Env>;
