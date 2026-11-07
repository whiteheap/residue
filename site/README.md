# The residue site

One static page that introduces residue and sends readers to the [README](../README.md), which stays the user contract. It is prepared for Cloudflare Workers Static Assets and **not published**: no Cloudflare resource exists for it, and no address serves it yet.

| Path | What it holds |
| --- | --- |
| `public/index.html` | The page: copy, commands and links |
| `public/404.html` | The page for an unknown path, served with status 404 |
| `public/style.css` | All styles. System fonts, grays only, light and dark from the reader's setting |
| `public/logo.svg`, `public/how-it-works.svg` and their `-dark.svg` pair | Copies of `docs/assets/`, the owner. Each page picks the dark one with `<picture>` when the reader's setting is dark. `logo.svg` is also the favicon |
| `public/_headers` | Response headers: Content Security Policy, `nosniff`, no framing, referrer and permissions policies. Cloudflare applies it and does not serve it |
| `wrangler.jsonc` | The Worker: assets only, no script, `public/` as the only upload, 404 page, workers.dev on, preview URLs off |
| `check.sh` | The static checks below |

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

## Publish

Not done. Publishing creates a public address, so it waits for the maintainer's approval, and so does every step below that talks to Cloudflare. Recheck the [Cloudflare docs](https://developers.cloudflare.com/workers/static-assets/) and the account's plan before the first deploy.

Cloudflare documents that "requests to static assets are free and unlimited" and that storing assets has no additional cost ([billing and limitations](https://developers.cloudflare.com/workers/static-assets/billing-and-limitations/)). This Worker has no script and no `run_worker_first`, so every request, the 404 page included, is answered from the assets and none runs Worker code. That statement covers this static page only: not a custom domain's registration, not Workers Builds, not any later dynamic feature.

1. Log in to the Cloudflare account that should own the site, or export a `CLOUDFLARE_API_TOKEN` with the Workers Scripts edit permission. Check which account Wrangler will use:

   ```sh
   cd site
   npx --yes wrangler@4.158.0 login
   npx --yes wrangler@4.158.0 whoami
   ```

2. Check that the account has no Worker named `residue` already: a deploy would replace its code. If this lists deployments, the name is taken; pick another `name` in `wrangler.jsonc`:

   ```sh
   npx --yes wrangler@4.158.0 deployments list --name residue
   ```

3. From a clean checkout of the reviewed commit on `main`, run `sh site/check.sh`, then deploy. Wrangler uploads `site/public` and nothing else, and prints the address (`https://residue.<account subdomain>.workers.dev`) and the version ID:

   ```sh
   cd site
   npx --yes wrangler@4.158.0 deploy --message "site at $(git rev-parse --short HEAD)"
   ```

4. Record the address, the version ID, the commit and the time in the pull request or issue that approved it. Then smoke it:

   ```sh
   URL=https://residue.ACCOUNT.workers.dev
   curl -sI "$URL/"            # 200, text/html, content-security-policy present
   curl -sI "$URL/style.css"   # 200, text/css
   curl -sI "$URL/logo.svg"    # 200, image/svg+xml
   curl -sI "$URL/missing"     # 404
   ```

5. Only now that a real address exists: add `<link rel="canonical">` and Open Graph tags with it to `public/index.html`, decide whether search engines should index it, and add a link to it from the README.

To serve a custom domain instead of workers.dev, add `"routes": [{ "pattern": "DOMAIN", "custom_domain": true }]` to `wrangler.jsonc` and set `"workers_dev": false`. The domain must be a zone on the same account.

Leave https://whiteheap.github.io/residue/ alone. GitHub Pages serves the signed apt, dnf and pacman repositories there; this site never replaces or redirects it.

## Roll back or take it down

Each deploy is a version that holds its assets. To go back to an earlier one:

```sh
npx --yes wrangler@4.158.0 versions list --name residue
npx --yes wrangler@4.158.0 rollback VERSION_ID --name residue --message "why"
```

The first deploy has no earlier version. To take the site down, set `"workers_dev": false` in `wrangler.jsonc` and deploy again, which turns the workers.dev address off, or delete the Worker with `npx --yes wrangler@4.158.0 delete residue`. Both are public changes and need the same approval as a deploy. Reverting a commit changes nothing that is deployed until the next deploy.
