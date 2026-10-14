# Agent notes for feedback-hub

- Use the flake dev shell: `nix develop --command …`.
- Before committing run: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, and in `web/`: `npm run typecheck && npm run build`, then
  `cargo build && (cd web && npm run e2e)` for widget/triage UI changes. Look at the
  screenshots in `web/test-results/` when changing UI.
- If `web/package-lock.json` changes, update `npmDepsHash` in `flake.nix`
  (`nix build .#web` prints the expected hash).
- The triage UI (`web/src/triage/`) is React + Radix UI primitives styled in
  `web/static/app.css`. The widget (`web/src/widget.ts`) stays dependency-free.
- The widget runs inside other apps: keep it self-contained in its shadow root,
  never throw into the host page, and keep the bundle small.
- `POST /api/feedback` is anonymous (network-gated + origin check); every other
  API needs the admin token. Don't loosen that.
- Claude Code may only be driven through the `claude` CLI (subscription use);
  never call the Anthropic API with its login or let other harnesses do so.
- Deployment lives in `~/homelab/apps/feedback-hub` (private ingress only).
