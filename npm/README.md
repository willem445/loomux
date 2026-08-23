# orrerix

Installer/launcher for [**Orrerix**](https://github.com/willem445/loomux) — a
sleek terminal multiplexer for AI agent management.

```sh
npx orrerix            # download + launch in one shot
npm install -g orrerix # then run `orrerix` anytime
```

Orrerix is a native (Tauri) desktop app, so this package doesn't contain the
app itself — it fetches the matching [GitHub release](https://github.com/willem445/loomux/releases)
asset for your platform (Windows installer, macOS `.dmg`, or Linux
`AppImage`), installs/caches it, and launches it.

The command is deliberately small:

```sh
orrerix            # launch the installed app (installs it first if missing)
orrerix update     # install/refresh the app from the newest release on your channel
orrerix version    # print this launcher's version
orrerix help       # full usage
```

Plain `orrerix` never updates an existing install — reinstalling silently
kills a running app, so the update decision belongs to you: run
`orrerix update` when you want a new version.

`orrerix update` picks the newest release **on the channel you are already on**
— a stable install gets the newest stable, a beta/RC install gets the newest
build of either kind — and it refuses to install an older build over a newer
one. To switch channels, install that build yourself from the
[releases page](https://github.com/willem445/loomux/releases). If it cannot read
the version you have installed, it stops and says so rather than guessing.

On Linux the app is a cached AppImage: plain `orrerix` launches the newest
cached build without downloading, and `orrerix update` fetches a new one. Quit
the running AppImage before updating — it cannot be overwritten in place.

`orrerix --reinstall` remains a deprecated alias for `orrerix update`. Its
meaning changed in v1.1: it used to install the release matching the launcher's
own version, and now it installs the newest release on your channel.

## Upgrading from `loomux-desktop`

This package was called `loomux-desktop`, and the command it installed was
`loomux`. Both were renamed with the app. There is no compatibility shim, so:

```sh
npm uninstall -g loomux-desktop
npm install -g orrerix
```

The launcher still recognises an app installed under the old name, so
`orrerix update` will see your existing version and refuse to downgrade it, and
a cached Linux AppImage downloaded by the old launcher is still launched
rather than re-downloaded. What it will **not** do is upgrade a pre-rename
install in place: the installer keys off the product name, so a fresh Orrerix
install lands beside the old app rather than replacing it. Uninstall the old
one whenever you're ready — nothing here does it for you.

Requires Node 18+. Builds are unsigned for now; on macOS the launcher clears
the quarantine flag for you.
