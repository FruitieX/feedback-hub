# Agent worker

`feedback-hub worker` runs on a machine with the app repositories and coding
agent CLIs installed. It only talks to the hub (outbound), so it can sit behind
NAT on a workstation.

## Runs

Queueing an item creates a **run**: agent, optional model and reasoning effort,
and optional instructions. The worker claims queued runs (up to `max_parallel`
at once), creates `~/worktrees/<repo>/feedback-<id>` from
`origin/<default branch>`, downloads the screenshots, and starts the agent.

The prompt contains the full report (message, screenshots, logs, the item's
timeline including comments) and how to work: follow the repo's AGENTS.md, fix
and verify, deliver per `delivery` (`push`, `branch` or `pr`), then mark the
item resolved with `feedback-hub status`, or set it back to `open` with a
question if it can't proceed. The instructions typed in the UI are appended.
`feedback-hub prompt <id>` prints the prompt a run would get.

If the agent ends without resolving the item, it goes back to `open`. Clean
worktrees are removed afterwards.

While a run is going, the worker streams the agent's activity (messages, tool
calls, results and the starting prompt) to the hub, polls for commands every
couple of seconds, and sends a heartbeat listing the agents it offers. A
message sent to a finished run requeues it on the same worker and resumes the
agent session there.

## Agents, models and efforts

| Agent    | Started as                                                         | Steering                                           |
| -------- | ------------------------------------------------------------------ | -------------------------------------------------- |
| `claude` | `claude -p --input-format stream-json --output-format stream-json` | live: messages are written to the running session  |
| `codex`  | `codex exec --json` / `codex exec resume <session>`                | after the current turn, or interrupt + resume      |
| `hermes` | `hermes chat --format stream-json` / `--resume <session>`          | after the current turn, or interrupt + resume      |

Any other CLI can be added with `kind = "command"` (plain output, no steering);
see `config.example.toml`.

Models and the reasoning efforts each model supports are read from the CLIs
themselves: at startup, every 30 minutes, and when the UI's refresh button is
pressed. Claude Code is asked through its stream-json `initialize` request (no
model call), Codex through `app-server` `model/list`, and Hermes through its
provider model cache. The UI only offers those, and the hub rejects other
choices while a worker is online. `[agents.<name>] models = [...]` pins a list
instead.

The agent, model and effort last queued from the UI are stored on the hub and
preselected next time, on every device.

## Send & fix from the widget

Signing in to the triage UI also sets an HttpOnly owner cookie on the hub. The
widget on the owner's apps (same site as the hub) asks `/api/widget/session`
with it and then offers **Send & fix**, which submits the feedback and queues
it for the last-used agent in one go. Other widget users never see the button,
and the hub ignores `dispatch` without the cookie.

## Claude Code

Claude Code is only ever driven through the official `claude` CLI with the
local login (no API calls, no token reuse), so it works with a subscription.
Hermes is refused Claude models and the `anthropic` provider, because Hermes
calls the API with Claude Code's stored login. Keep `max_parallel` /
`[agents.claude] max_parallel` modest to stay within usage limits.

Give the worker its own long-lived subscription token (`claude setup-token`,
saved to a file referenced by `[agents.claude] oauth_token_file`). Otherwise
runs share the interactive login, whose rotating refresh token is revoked when
several `claude` processes refresh it at once ("OAuth session expired and could
not be refreshed"). The token is passed to `claude` only, never to other
agents.

## Trust

Feedback text is user input, and agents run with permissions bypassed. The
worker therefore only runs items an admin queued, unless `include_open = true`.
Keep it that way unless everyone who can reach the widget is trusted.
