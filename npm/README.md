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
loomux update     # install/refresh the app from the newest release on your channel
loomux version    # print this launcher's version
loomux help       # full usage
```

Plain `loomux` never updates an existing install — reinstalling silently
kills a running Loomux, so the update decision belongs to you: run
`loomux update` when you want a new version.

`loomux update` picks the newest release **on the channel you are already on**
— a stable install gets the newest stable, a beta/RC install gets the newest
build of either kind — and it refuses to install an older build over a newer
one. To switch channels, install that build yourself from the
[releases page](https://github.com/willem445/loomux/releases).

On Linux the app is a cached AppImage: plain `loomux` launches the newest
cached build without downloading, and `loomux update` fetches a new one. Quit
the running AppImage before updating — it cannot be overwritten in place.

`loomux --reinstall` remains a deprecated alias for `loomux update`. Its
meaning changed in v1.1: it used to install the release matching the launcher's
own version, and now it installs the newest release on your channel.

Requires Node 18+. Builds are unsigned for now; on macOS the launcher clears
the quarantine flag for you.
