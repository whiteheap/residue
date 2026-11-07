# The residue site

One static page that introduces residue and sends readers to the [README](../README.md), which stays the user contract. It is served at https://residue.whiteheap.com by Cloudflare Workers Static Assets: the Worker `residue` on the account that holds the `whiteheap.com` zone. A change to `site/` deploys itself when it lands on `main` (see "Deploy").

| Path | What it holds |
| --- | --- |
| `public/index.html` | The page: copy, commands and links |
| `public/404.html` | The page for an unknown path, served with status 404 |
| `public/style.css` | All styles. System fonts, grays only, light and dark from the reader's setting |
| `public/logo.svg`, `public/how-it-works.svg` and their `-dark.svg` pair | Copies of `docs/assets/`, the owner. Each page picks the dark one with `<picture>` when the reader's setting is dark. `logo.svg` is also the favicon |
| `public/whiteheap-mark-black.svg`, `public/whiteheap-mark-white.svg` | The Whiteheap mark beside "by whiteheap" in the header: black on light, white on dark. Copies of `brand/whiteheap-mark-black.svg` and `brand/whiteheap-mark-white.svg` in [whiteheap/whiteheap.com](https://github.com/whiteheap/whiteheap.com), the owner (taken at 3431501); copy them again when the brand kit changes |
| `public/_headers` | Response headers: Content Security Policy, `nosniff`, no framing, referrer and permissions policies. Cloudflare applies it and does not serve it |
| `wrangler.jsonc` | The Worker: assets only, no script, `public/` as the only upload, 404 page, the Custom Domain `residue.whiteheap.com`, workers.dev and preview URLs off |
| `check.sh` | The static checks below |
| `smoke.sh` | The live check after a deploy: every served file matches `public/`, byte for byte, with the 404 page and the CSP header. Reads only |

No JavaScript, no web font, no analytics, no cookies, no forms, nothing loaded from another origin. Keep it that way: the Content Security Policy blocks anything else.

## Change the page

- Every command on the page is copied exactly from the README, and so is the minimum Git version. If the README changes one, change the page in the same pull request.
- Link to README sections for detail instead of copying flag tables, install alternatives or release notes. Do not add a version number: the page links to the latest release instead.
- The caveat section stays visible and keeps its four points: residue deletes directories, the activity probe cannot see everything, a write can still land after the last check, reclaimable space is an estimate. The README's "What removal deletes" and "Limits" are their owners.
- After a change to the logo or the chart in `docs/assets/`, light or dark, copy all four again: `cp docs/assets/logo.svg docs/assets/logo-dark.svg docs/assets/how-it-works.svg docs/assets/how-it-works-dark.svg site/public/`.
- A new file in `public/` is published. Add it to the allowlist in `check.sh` on purpose, or keep it out of `public/`.

## Check it

From the repository root:

```sh
sh site/check.sh
```

It needs no network. It checks that `public/` holds only the allowlisted files, that the artwork matches `docs/assets/`, that each command and the Git version appear in the README, that every local link and `#fragment` resolves, that every README anchor the page links to is a README heading, and that no page has a script, form, embed or third-party resource.

To see the page as Cloudflare would serve it, with the headers and the 404 page, run it locally. This starts a local server only: it needs no Cloudflare account and uploads nothing.

```sh
cd site
WRANGLER_SEND_METRICS=false npx --yes wrangler@4.158.0 dev --local --port 8799
curl -sI http://127.0.0.1:8799/          # 200, with the headers from public/_headers
curl -sI http://127.0.0.1:8799/missing   # 404, the body is public/404.html
```

Then look at it at a phone width and a desktop width, in light and dark, and tab through it with the keyboard.

## Deploy

A change to `site/` deploys itself when it lands on `main`: merging the pull request is the approval. The maintainer approved the site on 2026-11-07 and, the same day, asked for every change to deploy this way.

`.github/workflows/site.yml` runs `check.sh` on every pull request that touches `site/`, `README.md` or `docs/assets/`. On a push to `main` that touches `site/`, it runs `check.sh` again, deploys `site/public` with Wrangler under the message `site at SHORT_SHA`, and runs `smoke.sh` against https://residue.whiteheap.com, which waits up to a minute for every served file to match this commit byte for byte and for the 404 page and the Content Security Policy header. A failed deploy or smoke fails the run, and GitHub notifies whoever merged. The run's log has the Cloudflare version ID. Deploys run one at a time: a newer one waits for the one in flight, and of several waiting only the newest runs.

Cloudflare documents that "requests to static assets are free and unlimited" and that storing assets has no additional cost ([billing and limitations](https://developers.cloudflare.com/workers/static-assets/billing-and-limitations/)). This Worker has no script and no `run_worker_first`, so every request, the 404 page included, is answered from the assets and none runs Worker code. That statement covers this static page only: not a custom domain's registration, not Workers Builds, not any later dynamic feature.

### The token

The deploy job reads `CLOUDFLARE_API_TOKEN` from the GitHub environment `site`, which only `main` can use; a pull request never sees it. The account ID is in the workflow: it names the account and grants nothing. To create or rotate the token:

1. In the Cloudflare account that holds `whiteheap.com`, open [Account API tokens](https://dash.cloudflare.com/?to=/:account/api-tokens), select **Create Token**, and use the **Edit Cloudflare Workers** template.
2. Limit it to that account and to the zone `whiteheap.com`, and name it `residue site deploy (GitHub Actions)`.
3. Store it. Without a value on the command line, `gh` prompts for it, so it stays out of the shell history:

   ```sh
   gh secret set CLOUDFLARE_API_TOKEN -R whiteheap/residue --env site
   ```

4. Deploy once to prove it: `gh workflow run site.yml -R whiteheap/residue --ref main`, then `gh run watch -R whiteheap/residue`.

### By hand

When the workflow cannot run, deploy from a clean checkout of `main` with your own Cloudflare login. Check that `whoami` names the account that holds `whiteheap.com`. Wrangler uploads `site/public` and nothing else, attaches the Custom Domain (Cloudflare creates its DNS record and certificate; it refuses a hostname that already has a record), and prints the version ID:

```sh
cd site
npx --yes wrangler@4.158.0 login
npx --yes wrangler@4.158.0 whoami
sh check.sh
npx --yes wrangler@4.158.0 deploy --message "site at $(git rev-parse --short HEAD)"
sh smoke.sh
```

The page has `<link rel="canonical">` and Open Graph tags with that address, and no `noindex`: search engines may index it. The README links to it.

Leave https://whiteheap.github.io/residue/ alone. GitHub Pages serves the signed apt, dnf and pacman repositories there; this site never replaces or redirects it.

## Roll back or take it down

The usual way back is a revert on `main`: it deploys like any other change. Each deploy is also a Cloudflare version that holds its assets, so you can go back to one at once:

```sh
npx --yes wrangler@4.158.0 versions list --name residue
npx --yes wrangler@4.158.0 rollback VERSION_ID --name residue --message "why"
```

The next push to `main` that touches `site/` deploys over a rollback, so revert the change on `main` too.

To take the site down, remove `residue.whiteheap.com` from the Worker's Settings > Domains & Routes in the Cloudflare dashboard, and the `routes` entry from `wrangler.jsonc` so the next deploy does not attach it again, or delete the Worker with `npx --yes wrangler@4.158.0 delete residue` and remove `.github/workflows/site.yml`. Either takes the site away from every reader, so it waits for the maintainer's word.
