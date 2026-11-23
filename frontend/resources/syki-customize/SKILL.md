---
name: syki-customize
description: Operate, configure, troubleshoot, reset, and extend syki (user.css, plugins/<id>/plugin.js, syki-cli, Named Pipe JSON-RPC). Load when answering syki usage/config questions, building custom UI/workflows, or controlling syki from an external AI agent.
---

# syki-customize (Token-Lean Skill)

Windows-native (Rust + WebView2) fast-boot memo & workflow platform. Core stays fast; extensions load lazily via `user.css` and `plugins/<id>/plugin.js`.

## 1. Progressive Disclosure Routing (Read Only What's Needed)
| Intent | Load |
|---|---|
| Usage, 16 features, shortcuts, troubleshooting, **reset/recovery** | `references/usage-and-shortcuts.md` |
| `config.json` (LLM / `gemini-decision-1` / STT / semantic), `user.css`, responsive `@media`, `@media print`, `QcTheme` | `references/config-and-css.md` |
| `syki-cli` (10 subcommands), Named Pipe JSON-RPC, `cat \| syki-cli`, `exec.shell` Allowlist | `references/cli-jsonrpc-pipeline.md` |
| Verified real-world plugin patterns (12 recipes) | `references/real-world-recipes.md` |
| Exact TypeScript contracts (`SykiAPI`, `SykiEventMap`, `ActionHandle`, `PolicyDecision`) | `syki-api.d.ts` |

## 2. Paths & Single-Source Precedence
1. **Portable (wins)**: `<exe_dir>/user.css`, `<exe_dir>/plugins/<id>/plugin.js` (+ optional Shadow-DOM-only `style.css`)
2. **User data**: `%APPDATA%\syki-sok\` (override: `SYKI_DATA_DIR`, instance: `SYKI_INSTANCE_ID`)

## 3. Public Extension Surface (`apiVersion: 1`)
- **CSS hooks**: `data-syki-part` (`toolbar|tabs|tab-dock|statusbar|editor|gutter|preview|terminal|side-panel|inline-bar|find-bar|running-tasks|settings`), `data-syki-action="<id>"`, `data-priority="primary|secondary"`, `[data-syki-label]`. Use `tokens.css` vars only.
- **JS modules (`window.syki`)**: `editor`, `knowledge` (notes scoped to notes dir), `exec` (`llm` w/ `stripSecrets=true`, `shell(prog, args[])`, `pty`, `vision`, `generateImage`, `render.*`), `ui` (`registerAction` w/ `ActionHandle.update`, `mountPanel` Shadow DOM, `registerSettings`, `decide`, `review`, `diffConfirm`, `setToken`, `setChrome`), `flow` (`steps`, `parallel`, `decide` -> `gemini-decision-1` 0-output-token default, `loop` max 10), `input` (`voice`, `meeting`, `captureScreen`), `sys` (`locale`, `on`, `registerCommand` w/ `exposeRpc`, `runCommand`, `stripSecrets`, `clipboard`, `storage`).
- **Allowed `permissions`**: `llm`, `decision`, `voice`, `meeting`, `vision`, `image`, `semantic`, `pty`, `shell:<prog>`.

## 4. Hard Bans (Fail Static Check / Blocked at Runtime)
- Never access `chrome.webview` or `window.backend` directly.
- Never load external `http://` / `https://` resources or CDNs (CSP + 403 blocked).
- Never inject raw DOM into `toolbar`/`statusbar` (use `ui.registerAction`); custom UI belongs in `ui.mountPanel` Shadow DOM.
- Never hardcode raw hex colors in CSS/JS (use `tokens.css` vars). Never declare unknown `permissions`.

## 5. External Agent Control (CLI / JSON-RPC)
- Pipe stdin to daily scrap: `<cmd> | syki-cli`
- Invoke `exposeRpc: true` plugin command headlessly (auto-reattaches from 3–5MB `Detached` tray without stealing focus):
  `syki-cli plugin run <cmd-id> --args '{"k":"v"}'` (add `--show` to focus window)
- Dev hot-reload & health: `syki-cli plugin reload <id>` / `syki-cli plugin status [id]` / `syki-cli plugin list`

## 6. Mandatory Completion Gate (Before Handing Off Any Plugin)
```powershell
syki-cli plugin check <path/to/plugin.js>   # must exit 0, "ok": true, "errors": []
node --check <path/to/plugin.js>            # must exit 0
```

## 7. Quick Reset / Recovery (Non-Destructive First)
- **Skip all extensions**: `syki.exe --safe-mode`
- **Reset CSS**: remove active `user.css` + `<data_dir>/qc-theme-cache.json`
- **Reset trust/permissions**: delete `<data_dir>/trust-registry.json`
- **Full state reset (keep `scraps/`!)**: remove `config.json`, `session.json`, `semindex/`, or `webview2/` as needed (see `references/usage-and-shortcuts.md`).
