# loomux plugin template

Copy this whole folder to start a new [loomux](https://github.com/willem445/loomux)
pane plugin. Full authoring guide (manifest fields, the broker API, the
sandbox/CSP rules, how to install and open it):
**[`docs/features/pane-plugins.md`](../../docs/features/pane-plugins.md)** in
the loomux repo, or the published page once it ships.

## What's here

| File | Purpose |
| --- | --- |
| `plugin.json` | The manifest — rename `id`, `name`, `version` before you ship. |
| `index.html` | The entry page `plugin.json`'s `entry` field points at. |
| `styles.css` | Linked, not inline — the plugin CSP has no `unsafe-inline` for styles. |
| `main.js` | The "hello world" logic: one `storage` round trip through the broker. |
| `sdk/plugin-sdk.js` | A tiny, dependency-free client for the broker's `request()`/`onEvent()` API. Copy it as-is; nothing to build. |

## Try it

1. Copy this folder somewhere else and give `id` in `plugin.json` a unique
   value.
2. Copy (or rename) the folder into loomux's plugins directory so the
   **folder name matches `id` exactly**:
   `%APPDATA%\loomux\plugins\<id>\` on Windows.
3. In loomux, open a new pane → **Plugin** → pick your plugin from the list.

There is no build step and no dev server: edit the files in place under
`plugins\<id>\`, close and reopen the pane to pick up the change.
