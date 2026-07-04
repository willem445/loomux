# loomux

Installer/launcher for [**Loomux**](https://github.com/willem445/loomux) — a
sleek terminal multiplexer for AI agent management.

```sh
npx loomux            # download + launch in one shot
npm install -g loomux # then run `loomux` anytime
```

Loomux is a native (Tauri) desktop app, so this package doesn't contain the
app itself — it fetches the matching [GitHub release](https://github.com/willem445/loomux/releases)
asset for your platform (Windows installer, macOS `.dmg`, or Linux
`AppImage`), installs/caches it, and launches it.

Pass `--reinstall` to force a fresh download instead of launching a cached or
already-installed copy:

```sh
npx loomux --reinstall
```

Requires Node 18+. Builds are unsigned for now; on macOS the launcher clears
the quarantine flag for you.
