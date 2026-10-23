# Frontend Notes — moved

The backend→frontend contract (and the frontend→backend contract) now live in a single
bidirectional source of truth:

**`C:\dev\coordinated\coordination.md`**

Do not maintain contract details here. When the backend changes an `/api/*` payload, a config gate
that alters an API response, daemon behavior the dashboard depends on, the SOC2 export layout, or the
telemetry contract, update `coordination.md` (§3–§6) in the same change. See `coordination.md` §10
(Coordination Protocol).

Track 0511: `GET /api/trends` adds omit-empty `lastDay` when `data` is empty and an older `project_trend_days` day exists. See `coordination.md`.

Track 0556: `GET /api/verify/health` stays `HEALTHY` | `DEGRADED` | `FAILING`. A HEALTHY `message` may be the cheap sentence `Cheap command recorded: git --version. Not a project-suite pass.` A HEALTHY response with no message keeps the freshness-window card summary. See `coordination.md` §4.2 and §4.6.
