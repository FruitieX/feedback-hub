# feedback-hub

In-app feedback for all my private apps, stored in one place so coding agents
can pick it up and fix things.

- **Widget** (`web/src/widget.ts`): a small, discreet corner button. It snapshots the
  page, lets the user circle/box/arrow things on one or more screenshots
  (page snapshot, real screen capture, or an uploaded/pasted image), and sends
  the report with page URL, device info, recent console errors and failed
  requests.
- **Server** (`feedback-hub serve`): Rust + SQLite. Anonymous `POST
  /api/feedback` for apps (origin-checked), everything else behind an admin
  token. Serves the widget and a triage UI at `/`.
- **CLI** (`feedback-hub list|show|comment|status|queue|apps|prompt`): how
  humans and agents read and update feedback.
- **Worker** (`feedback-hub worker`): polls for queued items and runs Claude
  Code, Codex or Hermes in a fresh git worktree of the app's repo.

```
app page ──(gateway injects <script src=…/widget.js>)──▶ widget ──POST──▶ feedback-hub (k8s, private)
                                                                             ▲      │
                     triage UI: queue for claude/codex/hermes ───────────────┘      │ claim
                                                                                    ▼
                                           workstation: feedback-hub worker → agent in ~/worktrees/<repo>/feedback-<id>
```

## Adding the widget to an app

Web apps don't need code changes. In the homelab, the Envoy gateway injects
the widget into HTML pages of opted-in routes, only for LAN/VPN clients — see
`apps/feedback-hub/widget-injection.*` in the homelab repo. To opt an app in,
add an `EnvoyExtensionPolicy` for its HTTPRoute there and an entry to the app
registry (`apps.toml`).

Anywhere else, include the script directly:

```html
<script async src="https://feedback.fruitiex.org/widget.js" data-app="myapp"></script>
```

Optional attributes: `data-position="left"` (bottom-left corner),
`data-version="1.2.3"` (also read from `<meta name="app-version">`),
`data-label`. JavaScript API once loaded (`feedbackhub:ready` event):
`FeedbackHub.open()`, `.setContext({...})`, `.setReporter(name)`,
`.hideButton()`, `.showButton()`.

Native apps POST the same JSON (`source: "android"`, screenshots as data URLs)
without an `Origin` header — see Ferrotune's Android `feedback` module.

## Working on feedback (agents)

```sh
feedback-hub list                     # open/queued/in-progress items across all apps
feedback-hub show 12                  # Markdown report; screenshots saved to ~/.cache/feedback-hub/12/
feedback-hub apps                     # app id → repository
feedback-hub comment 12 "Found it: …"
feedback-hub status 12 resolved --note "Fixed in abc123, verified with …"
feedback-hub status 12 open --note "Need to know whether …"   # needs the owner
feedback-hub queue 12 --agent codex   # hand to the worker
feedback-hub prompt 12                # the prompt the worker would use
```

Configuration lives in `~/.config/feedback-hub/config.toml` (see
`config.example.toml`) or `FEEDBACK_HUB_URL` / `FEEDBACK_HUB_TOKEN`.

## Agent worker

`feedback-hub worker` claims items that were **queued** (from the triage UI or
`feedback-hub queue`), creates `~/worktrees/<repo>/feedback-<id>` from
`origin/<default branch>`, downloads screenshots, and runs the chosen agent
with a prompt containing the report and instructions to fix, verify,
commit/push (per `delivery`), and report back with `feedback-hub status`.
The agent's output tail is posted to the item's timeline; if the agent ends
without resolving, the item returns to `open`. Clean worktrees are removed.

Feedback text is user input. Agents run with permissions bypassed, so the
worker only runs on items an admin queued unless `include_open = true` — keep
it that way unless everyone who can reach the widget is trusted.

## Server configuration

| Variable | Default | |
|---|---|---|
| `FEEDBACK_HUB_ADMIN_TOKEN` | — | required, ≥16 chars |
| `FEEDBACK_HUB_ALLOWED_ORIGINS` | `http://localhost:*` | comma-separated, e.g. `https://*.fruitiex.org` |
| `FEEDBACK_HUB_DATA_DIR` | `./data` | SQLite DB + screenshot files |
| `FEEDBACK_HUB_STATIC_DIR` | — | built `web/dist` |
| `FEEDBACK_HUB_APPS_FILE` | — | app registry TOML (`[[apps]]` id, name, repo, checkout, hosts, notes) |
| `FEEDBACK_HUB_SQLITE_JOURNAL_MODE` | `WAL` | use `DELETE` on NFS |

## Development

```sh
nix develop
cargo test && cargo clippy --all-targets -- -D warnings
cd web && npm ci && npm run typecheck && npm run build
cargo build && (cd web && npm run e2e)   # browser test; screenshots in web/test-results/
nix build .#container
```
