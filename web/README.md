# honya — web

Operator guide for the honya homepage and its release plumbing. The homepage is
a **TanStack Start** app (React 19 + Tailwind v4) that is **prerendered to static
HTML** and served by Cloudflare Pages; it also hosts `install.sh` / `install.ps1`,
the one-line installers that download release binaries built by
`.github/workflows/release.yml`.

## Repo layout

```
web/
├── package.json           scripts: dev / build / generate-routes / preview
├── vite.config.ts         TanStack Start + Tailwind v4 + Nitro; prerender enabled
├── tsconfig.json
├── wrangler.toml          Cloudflare Pages config (output dir = .output/public)
├── README.md              this file
├── src/
│   ├── router.tsx         createRouter
│   ├── routes/
│   │   ├── __root.tsx     <html>, shared <head> (fonts, favicon, OG), skip link
│   │   ├── index.tsx      the homepage (hero + animated terminal, five-screen
│   │   │                  tab demo, pipeline, install, features, trust, FAQ)
│   │   └── changelog.tsx  the /changelog page (renders src/data/changelog.ts)
│   ├── components/        Header, Footer, Brand, Terminal (animated), ScreenTabs,
│   │                      PipelineDiagram, InstallCard, Reveal, icons
│   ├── data/              site.ts (urls/version), changelog.ts (release history)
│   ├── hooks/             useScrolled (sticky-header shadow)
│   └── styles/app.css     Tailwind v4 @theme tokens + the washi/sumi component CSS
└── public/                <- copied verbatim into the build output
    ├── install.sh         curl | bash installer (served at /install.sh)
    ├── install.ps1        irm | iex installer (served at /install.ps1)
    ├── _headers           Pages headers (installer MIME, asset cache, security)
    └── _redirects         /install → /install.sh
```

The build prerenders `/` and `/changelog` to static HTML under **`.output/public`**,
then copies everything in `public/` (the installers, `_headers`, `_redirects`)
alongside it. That directory is the Cloudflare Pages deploy root — it is fully
static (no runtime server).

## Design & framework notes

- **Aesthetic**: washi paper (`#F3EFE6`) / sumi ink, 藍 indigo (`#3A5078`) accent,
  with the 本屋 wordmark. Tokens live in the `@theme` block in `src/styles/app.css`
  and are mirrored into `:root` so the bespoke component CSS (the terminal mock,
  the five-screen panel, the pipeline SVG, the changelog timeline) can keep using
  `var(--washi)` etc. New/elevated sections use Tailwind utilities + those tokens.
- **Fonts** come from the Google Fonts CDN in one combined request (see the
  `<link>`s in `src/routes/__root.tsx`): **Zen Kaku Gothic New** (Latin/UI),
  **Noto Serif JP** (本屋 wordmark + JP glyphs), **JetBrains Mono** (terminal/code),
  **Noto Sans Thai Looped** (Thai body). The page is Thai-first (`<html lang="th">`);
  the looped Thai face sits *after* the Latin/JP faces in every stack so it only
  supplies Thai glyphs (preserves metric-locked fallbacks + tabular-nums).
- **Interactivity is React, hydrated on top of the prerender**: the animated
  "play one chapter" terminal (`Terminal.tsx`, a `requestAnimationFrame` state
  machine that pauses offscreen/when hidden), the five-screen tab switcher
  (`ScreenTabs.tsx`, arrow-key roving), the clipboard + OS-aware install command
  (`InstallCard.tsx`), the sticky-header shadow (`useScrolled`), and the
  scroll-reveal (`Reveal.tsx`). Scroll-reveal is gated behind a `.js` class set by
  an inline `<head>` script, so prerendered/no-JS visitors see all content.
- **Changelog** is data, not markup: edit `src/data/changelog.ts` (newest first).

## Commands

This project uses **bun** (`bun.lock` is the committed lockfile).

```sh
bun install        # first time
bun run dev        # local dev server at http://localhost:3000
bun run build      # prerender to .output/public (what CI deploys)
bun run preview    # preview the built output
```

To preview the *static* output exactly as Pages serves it:

```sh
bun run build
cd .output/public && python3 -m http.server 8000
```

(Note: `/install` → `/install.sh` and the `_headers` rules are Cloudflare Pages
features, so they only take effect on the deployed site, not under a plain static
server.)

## Cloudflare Pages setup (one-time, dashboard)

1. Create a Pages project named **`honya`** (Workers & Pages → Create → Pages).
   The name must match `--project-name=honya` in `pages.yml`.
2. Add the custom domain **`honya.altqx.com`** (project → Custom domains).
3. Create a Cloudflare API token with **Pages: Edit** and note your **Account ID**.
4. Add both as GitHub repo secrets:
   - `CLOUDFLARE_API_TOKEN`  — the Pages:Edit token
   - `CLOUDFLARE_ACCOUNT_ID` — your account ID

CI (`.github/workflows/pages.yml`) runs `bun install --frozen-lockfile && bun run build` in `web/`, then
publishes `web/.output/public` via `cloudflare/wrangler-action@v3` on any push to
`main` that touches `web/**`. No manual `wrangler` runs are needed for normal deploys.

## Cutting a release

Release binaries are produced by `.github/workflows/release.yml` and consumed by
`install.sh` / `install.ps1`. Tag a semver version and push the tag:

```sh
git tag vX.Y.Z
git push --tags
```

The pushed `v*` tag runs `release.yml`, which cross-compiles `honya` for each
target triple and attaches, per target, exactly two assets to the GitHub Release:

```
honya-<target>.tar.gz   gzip tar with a single executable named honya
honya-<target>.sha256   sha256 checksum line for the .tar.gz (no .tar.gz in the name)
```

Shipped target triples:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`
- `aarch64-pc-windows-msvc`

`install.sh` resolves "latest" from
`https://api.github.com/repos/altqx/honya/releases/latest` (override with the
`HONYA_VERSION` env var) and downloads from
`https://github.com/altqx/honya/releases/download/<tag>/honya-<target>.tar.gz`.
