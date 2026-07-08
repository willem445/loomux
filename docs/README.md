# Loomux documentation site

The user-facing documentation for loomux, published to **GitHub Pages** at
<https://willem445.github.io/loomux/>. This folder is the whole site: Markdown
pages plus one `_config.yml`.

> This `README.md` is a **contributor** note — it is excluded from the published
> site (`exclude:` in `_config.yml`). The reader-facing entry point is
> [`index.md`](index.md).

## Tooling decision: GitHub-Pages-native Jekyll (not a Node SSG)

The brief was to optimize for **zero maintenance** and **no new repo toolchain
burden**. Two options were on the table:

1. **GitHub Pages' native Jekyll** — checked-in Markdown + `_config.yml`, no
   build dependencies committed to this repo.
2. **A minimal Node SSG** (Astro/Eleventy/VitePress) — we already run Node in CI,
   so it's feasible.

**We chose Jekyll (option 1).** Rationale:

- **No repo-local toolchain to maintain.** There is no `package.json`, no
  `Gemfile`, no lockfile in `docs/`. The Jekyll runtime is provided *by the CI
  action* (`actions/jekyll-build-pages`, which bundles the `github-pages` gem),
  not pinned in this repo. A Node SSG would add a dependency tree we'd have to
  keep patched (Dependabot noise, build breakage on major bumps) — exactly the
  maintenance burden the brief said to avoid.
- **Content is just Markdown.** The docs are prose; they don't need a component
  framework or a JS build step.
- **The theme is a pinned remote theme.** `remote_theme:
  just-the-docs/just-the-docs@v0.12.0` gives a sidebar, search, and
  light/dark — a real docs UX — without vendoring anything. It's pinned to a
  release tag so an upstream change can't silently break a release-day publish;
  bumping it is a deliberate one-line edit.
- **Node stays in CI for what needs it** (the app's typecheck/build/tests). The
  docs deploy is a separate, self-contained workflow that pulls in nothing from
  the app toolchain.

Trade-off accepted: the Jekyll build only runs in CI/Pages (no Ruby is assumed on
a contributor's machine), so a broken `_config.yml` or a bad theme pin is caught
by the workflow's **build job**, not locally. That's why the docs workflow runs a
**build-only dry-run on PRs that touch `docs/`** (see below).

## Layout

```
docs/
  _config.yml            site config + theme pin
  index.md               Home (nav_order 1)
  getting-started.md     (2)
  core-concepts.md       (3) — panes/grid/shortcut table
  orchestration.md       (4) — groups, board, label handshake, autonomous stub
  features/
    index.md             "Features" nav parent
    git-view.md
    github-issues.md
    voice-prompts.md
    steering.md
    session-browser.md
  troubleshooting.md     (6)
  README.md              this file (excluded from the site)
```

## How it's published

[`.github/workflows/docs.yml`](../.github/workflows/docs.yml) builds and deploys
via the official GitHub Pages Actions flow
(`configure-pages` → `jekyll-build-pages` → `upload-pages-artifact` →
`deploy-pages`). It runs:

- **on release** — tag pushes matching `v*` (the same trigger as
  `release.yml`, which it deliberately does **not** modify), so the site
  refreshes with each release;
- **on `workflow_dispatch`** — a manual button for docs-only fixes between
  releases;
- **on pull requests that touch `docs/`** — a **build-only dry-run** (the deploy
  job is skipped) so a broken config is caught before it ships. The app's regular
  CI (`ci.yml`) does **not** build the docs, so PR CI on code changes stays fast.

### One-time human setup (required once, can't be automated here)

GitHub Pages must be told to take its content from **GitHub Actions** rather than
a branch:

> **Settings → Pages → Build and deployment → Source → "GitHub Actions".**

Until that's set, `deploy-pages` has nowhere to publish. This is a repo setting an
agent/workflow can't flip — do it once and every subsequent release publishes
automatically.

## Editing

- Add a page: create `foo.md` with front matter (`title`, `layout: default`,
  `nav_order`; add `parent: Features` for a feature sub-page). Keep `nav_order`
  values sane so the sidebar orders correctly.
- Cross-page links use **repo-relative paths without `.md`** (e.g.
  `[git view](features/git-view)`), which Jekyll + `baseurl` resolve correctly on
  the published `/loomux/` site.
- **Honesty rule:** document only what ships on `main`. Verify every flag,
  shortcut, and behavior against the code/README before writing it. No invented
  features; no fake screenshots.

## Local preview (optional)

You don't need this — the CI build is authoritative — but if you have Ruby and
want a local preview:

```sh
cd docs
gem install bundler jekyll
# minimal Gemfile is not committed; install the github-pages gem to match CI:
gem install github-pages
jekyll serve
```

(Kept out of the committed toolchain on purpose — see the decision above.)
