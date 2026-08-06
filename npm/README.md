# loomux-desktop

Installer/launcher for [**Loomux**](https://github.com/willem445/loomux) — a
sleek terminal multiplexer for AI agent management.

```sh
npx loomux-desktop            # download + launch in one shot
npm install -g loomux-desktop # then run `loomux` anytime
```

> Published as `loomux-desktop` because the bare `loomux` name on npm belongs
> to an unrelated tmux tool. The command it installs is still `loomux`.

Loomux is a native (Tauri) desktop app, so this package doesn't contain the
app itself — it fetches the matching [GitHub release](https://github.com/willem445/loomux/releases)
asset for your platform (Windows installer, macOS `.dmg`, or Linux
`AppImage`), installs/caches it, and launches it.

The command is deliberately small:

```sh
loomux            # launch the installed app (installs it first if missing)
loomux update     # install/refresh the app from the latest GitHub release
loomux version    # print this launcher's version
loomux help       # full usage
```

Plain `loomux` never updates an existing install — reinstalling silently
kills a running Loomux, so the update decision belongs to you: run
`loomux update` when you want a new version.

On Linux the app is a cached AppImage: plain `loomux` launches the newest
cached build without downloading, and `loomux update` fetches the latest
release.

Requires Node 18+. Builds are unsigned for now; on macOS the launcher clears
the quarantine flag for you.
