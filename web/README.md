# honya — web

Operator guide for the honya homepage and its release plumbing. The homepage is
a static site served by Cloudflare Pages; it also hosts `install.sh`, the
`curl | bash` installer that downloads release binaries built by
`.github/workflows/release.yml`.

## Repo layout

```
web/
├── wrangler.toml          Cloudflare Pages config (output dir = public)
├── README.md              this file
└── public/                <- DEPLOY ROOT (everything here is published)
    ├── index.html         the homepage (htmx shell: hero + animated terminal,
    │                       five-screen tab switcher, install CTA, footer)
    ├── changelog.html     the changelog page (served at /changelog) — per-version
    │                       history; update it on every feature/fix (see CLAUDE.md)
    ├── partials/          htmx fragments loaded into the five-screen panel:
    │   ├── screen-shelf.html
    │   ├── screen-project.html
    │   ├── screen-translate.html   (default screen, also rendered inline)
    │   ├── screen-reader.html
    │   └── screen-lexicon.html
    ├── install.sh         the curl | bash installer (served at /install.sh)
    ├── _headers           Pages headers (install.sh MIME, security headers)
    └── _redirects         /install → /install.sh
```

`web/public` is the deploy root: whatever lives under it is what ships.

The homepage is built with **htmx 2.x** (loaded from the unpkg CDN). The five
screen tabs (書架 / 棚 / 訳 / 読 / 辞) are `<button>`s with `hx-get` to the
matching `partials/screen-*.html`, swapped into a fixed-height `#screen-panel`
via `hx-swap="innerHTML"`. The default **訳 Translate** screen is rendered inline
in the panel so it works before htmx loads and produces no layout shift. The
hero (incl. the animated terminal) and install CTA are inline in `index.html`.

Edit the homepage in place and push — there is no build step.

### Typography

All fonts come from the Google Fonts CDN in one combined request (see the single
`<link>` in `<head>`): **Zen Kaku Gothic New** (Latin/UI sans), **Noto Serif JP**
(the 本屋 wordmark and Japanese glyphs), **JetBrains Mono** (terminal/code), and
**Noto Sans Thai Looped** for the Thai body copy. The page is Thai-first
(`<html lang="th">`), so Thai text is the bulk of the content.

The font stacks are CSS variables (`--sans`, `--serif`, `--mono`) in `:root`.
`Noto Sans Thai Looped` is deliberately placed **after** the Latin/JP face in each
stack: those faces don't carry Thai glyphs, so Thai falls through to the looped
face while Latin keeps Zen Kaku Gothic New / JetBrains Mono untouched. That order
preserves the metric-locked fallback (no font-swap reflow) for Latin and the
tabular-nums alignment in the terminal mock.

## Cloudflare Pages setup (one-time, dashboard)

1. Create a Pages project named **`honya`** (Workers & Pages → Create → Pages).
   The project name must match `--project-name=honya` in `pages.yml`.
2. Add the custom domain **`honya.altqx.com`** to the project
   (project → Custom domains → Set up a custom domain) and follow the DNS
   prompts.
3. Create a Cloudflare API token with the **Pages: Edit** permission
   (My Profile → API Tokens) and note your **Account ID** (Workers & Pages
   overview, right sidebar).
4. Add both as GitHub repo secrets (Settings → Secrets and variables → Actions):
   - `CLOUDFLARE_API_TOKEN`  — the Pages:Edit token
   - `CLOUDFLARE_ACCOUNT_ID` — your account ID

CI publishes via `cloudflare/wrangler-action@v3`; no manual `wrangler` runs are
needed for normal deploys.

## Cutting a release

Release binaries are produced by `.github/workflows/release.yml` and consumed by
`install.sh`. Tag a semver version and push the tag:

```sh
git tag vX.Y.Z
git push --tags
```

The pushed `v*` tag runs `release.yml`, which cross-compiles `honya` for each
target triple and attaches, per target, exactly two assets to the GitHub
Release:

```
honya-<target>.tar.gz   gzip tar with a single executable named honya
honya-<target>.sha256   sha256 checksum line for the .tar.gz (no .tar.gz in the name)
```

Shipped target triples:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

`install.sh` resolves "latest" from
`https://api.github.com/repos/altqx/honya/releases/latest` (override with the
`HONYA_VERSION` env var) and downloads from
`https://github.com/altqx/honya/releases/download/<tag>/honya-<target>.tar.gz`.

## Local preview

Serve the deploy root and open the printed URL:

```sh
cd web/public
python3 -m http.server
# → Serving HTTP on 0.0.0.0 port 8000 (http://0.0.0.0:8000/)
```
