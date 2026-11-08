# The residue site

One static page that introduces residue and sends readers to the [README](../README.md), which stays the user contract. It is served at https://residue.whiteheap.com by Cloudflare Workers Static Assets: the Worker `residue` on the account that holds the `whiteheap.com` zone. A change to `site/` deploys itself when it lands on `main` (see "Deploy").

| Path | What it holds |
| --- | --- |
| `public/index.html` | The page: copy, commands and links. The hero leads with one install command, copied from the README |
| `public/404.html` | The page for an unknown path, served with status 404 |
| `public/style.css` | All styles, in the look of [whiteheap.com](https://whiteheap.com): dark only, grays in oklch, IBM Plex Mono for the wordmark, headings, labels and code, the system sans for reading |
| `public/hero.js` | The one script: the hero's pixel stack on a canvas, and the copy button beside the install command. It loads and stores nothing, stops off screen and in a hidden tab, and draws one still frame under reduced motion. Without it the page reads the same: the canvas stays black (and with scripting off the hero shrinks to its text), the copy button stays hidden, and one click still selects the command |
| `public/ibm-plex-mono-latin-400-normal.woff2`, `public/ibm-plex-mono-latin-500-normal.woff2`, `public/ibm-plex-mono-OFL.txt` | IBM Plex Mono 400 and 500, latin subset, and its SIL Open Font License: `files/` and `LICENSE` of `@fontsource/ibm-plex-mono` 5.3.0, the version whiteheap.com uses. `check.sh` holds their SHA-256. To update: `npm pack @fontsource/ibm-plex-mono@VERSION`, copy the three files, and replace the hashes in `check.sh` |
| `public/logo-dark.svg`, `public/how-it-works-dark.svg`, `public/logo.svg` | Copies of `docs/assets/`, the owner. The page shows the dark drawings. `logo.svg` is the favicon only: its dark strokes and light fills read on a light or a dark tab bar |
| `public/og.png`, `public/apple-touch-icon.png`, `public/icon-192.png` | The social image that link previews show, 1200 by 630: the logo, the name and the h1. The home-screen icon, 180 by 180. The PNG favicon, 192 by 192, for browsers and search engines that do not use the SVG one. Copies of `docs/assets/`, where `render.sh` renders them from `og.svg`, `apple-touch-icon.svg` and `icon-192.svg` |
| `public/robots.txt`, `public/sitemap.xml` | One group that lets every crawler in, search engines and AI alike, and the sitemap's address. The sitemap lists the page's canonical URL, with no `lastmod`: the page has no content date to give |
| `public/llms.txt` | The page for agents ([llmstxt.org](https://llmstxt.org)): what residue does, then links to install it, the docs and the source. Docs links point to the raw markdown on GitHub, which an agent reads without the page around it. Its commands are copied from the README |
| `public/.well-known/security.txt` | Where to report a vulnerability ([RFC 9116](https://www.rfc-editor.org/rfc/rfc9116.html)): the repository's private advisory form and `SECURITY.md`. Renew `Expires`, at most a year ahead, before `check.sh` fails 30 days ahead of it |
| `public/b0581e2550b0bcc904c146120f116cc0.txt` | The IndexNow key. Search engines fetch it to check that a submission came from this site (see "Search engines and agents"). It is public by design |
| `public/whiteheap-mark-white.svg` | The Whiteheap mark beside "by whiteheap" in the header. A copy of `brand/whiteheap-mark-white.svg` in [whiteheap/whiteheap.com](https://github.com/whiteheap/whiteheap.com), the owner (taken at 3431501); copy it again when the brand kit changes |
| `public/install.sh` | The install script behind `curl -fsSL https://residue.whiteheap.com/install.sh \| sh`: POSIX sh, served as `text/plain` so it reads in a browser. `docs/installation.md` documents it; `check.sh` shellchecks it and checks the key hashes it pins; `.github/workflows/install.yml` runs it through every channel |
| `public/_headers` | Response headers: Content Security Policy, `nosniff`, no framing, referrer and permissions policies, `text/plain` for `install.sh`, `charset=utf-8` for every `.txt` file, and a one-day `max-age` for the fonts and drawings, which change rarely. The pages, `style.css` and `hero.js` keep Cloudflare's default, `max-age=0, must-revalidate`, because they change together and their names carry no version. Cloudflare applies it and does not serve it. HSTS comes from the zone instead (see "HTTPS") |
| `public/_redirects` | Redirects: `/download/macos` to `residue-macos.dmg` in the newest [residue release](https://github.com/whiteheap/residue/releases/latest), whose name never changes. Cloudflare applies it and does not serve it; `check.sh` accepts a link to a redirect's source |
| `wrangler.jsonc` | The Worker: assets only, no Worker script, `public/` as the only upload, 404 page, the Custom Domain `residue.whiteheap.com`, workers.dev and preview URLs off |
| `check.sh` | The static checks below |
| `smoke.sh` | The live check after a deploy: every served file matches `public/`, byte for byte, with the 404 page, the exact CSP header from `_headers`, the zone's HSTS header, the type of the script and the fonts (with `nosniff`, a wrong type would block them), of the text files, the sitemap and the images, each redirect's code and destination, and a 200 for each named search and AI crawler. It asks for `*/*`, because Cloudflare adds its Web Analytics script to the HTML it sends a browser. Reads only |

One same-origin script and two same-origin fonts; no analytics, no cookies, no forms, nothing loaded from another origin. The home page's JSON-LD block is data for search engines, which browsers do not run. Keep it that way: the Content Security Policy allows the site's own scripts, styles, images and fonts, and requests to the site itself (`connect-src`, which Lighthouse's robots.txt and llms.txt audits need), and nothing else, and `check.sh` holds it to that exact value.

## Change the page

- The hero's h1 and lede are the repository's GitHub description, word for word: its first sentence is the h1 and its second the lede. The meta description, `og:description`, the JSON-LD `description` and the `>` summary in `llms.txt` repeat it whole, and the social image and `og:image:alt` show the h1. Change the description and the page together, in the same pull request: `gh repo edit whiteheap/residue --description "..."`, then the h1, the lede, the meta tags, the JSON-LD, `llms.txt`, and the headline in `docs/assets/og.svg` (render it again, below).
- Copy follows the Whiteheap writing style: the job first, the project's own words (candidate, keep, unknown), a choice as a list, and every warning before the action it qualifies. Shorter is not the test; keep every command, flag, caveat and link.
- Every command on the page and in `llms.txt` is copied exactly from the README, and so is the minimum Git version. If the README changes one, change the page and `llms.txt` in the same pull request.
- Link to README sections for detail instead of copying flag tables, install alternatives or release notes. Do not add a version number: the page links to the latest release instead.
- The caveat section stays visible and keeps its four points: residue deletes directories, the activity probe cannot see everything, a write can still land after the last check, reclaimable space is an estimate. The README's "What removal deletes" and "Limits" are their owners.
- The "automatic", "what it keeps" and "where it fits" sections say only what the README says today. Its "Reclaim storage automatically", "What removal deletes, and what it keeps", "The trash" and "Where it fits" are their owners; a new claim, or a new tool in "where it fits", lands in the README first. In "automatic", the caveat (these removals delete, without the trash) comes before the dry run and the install command.
- The hero has one install command, the README's first. A link beside it, "read the script", opens `/install.sh`, what that command runs; keep the two in step. Homebrew stays visible in the note under it.
- After a change to the logo or the dark chart in `docs/assets/`, copy them again: `cp docs/assets/logo.svg docs/assets/logo-dark.svg docs/assets/how-it-works-dark.svg site/public/`. The social image and the PNG icons draw the logo too, so render them again (Chrome draws them in the site's own font) and copy them: `sh docs/assets/render.sh og.svg apple-touch-icon.svg icon-192.svg`, then `cp docs/assets/og.png docs/assets/apple-touch-icon.png docs/assets/icon-192.png site/public/`. Look at each PNG before you commit it.
- A new file in `public/` is published. Add it to the allowlist in `check.sh` on purpose (one name per line, sorted), or keep it out of `public/`. Keep `public/` flat, but for `.well-known/`: `smoke.sh` fetches each file as `/NAME` or `/.well-known/NAME`.
- A new page needs what `index.html` has: a title that starts with `residue · `, a description, a canonical URL, the Open Graph and Twitter tags and the icon links, and its URL in `sitemap.xml`. A page that should not be found, like `404.html`, has `noindex` instead and stays out of the sitemap. `check.sh` rule 9 checks both.
- A new script, or a change to what `hero.js` may do, needs `check.sh` rule 6 and the policy in `_headers` changed on purpose, in the same pull request.

## Check it

From the repository root:

```sh
sh site/check.sh
```

It needs no network, and needs jq. It checks that `public/` holds only the allowlisted files, that the artwork matches `docs/assets/` and the fonts their pinned hashes, that each command and the Git version appear in the README, that every local link and `#fragment` resolves, that every README anchor the page and `llms.txt` link to is a README heading, that the only script is `/hero.js` and it loads and stores nothing, that no page or image has an inline script (other than the home page's JSON-LD) or handler, a form, an embed or a third-party resource, that the fonts are the pinned files with `font-display: swap` and a preload, and that the Content Security Policy is the exact one in the script. It also checks that `install.sh` passes shellcheck (when shellcheck is installed) and that the SHA-256 and fingerprint it pins for the signing key match `packaging/signing-key.asc` (the apt keyring and fingerprint checks need gpg).

For search engines, link previews and agents (rule 9), it checks that `robots.txt` has one group that allows every crawler and names the sitemap, that `sitemap.xml` lists exactly the public pages' canonical URLs, that each page's title starts with `residue · ` and each public page has a description, a canonical URL, and Open Graph and Twitter tags, that the social image is 1200 by 630 and the icons their sizes, that the JSON-LD parses and has the fields search engines need, that `llms.txt` has the [llmstxt.org](https://llmstxt.org) shape and passes Lighthouse's llms.txt audit, that `_headers` serves every `.txt` file with `charset=utf-8` (`wrangler dev` adds it, Cloudflare does not), that `security.txt` expires more than 30 days and less than a year from now, and that the IndexNow key file holds its key.

To see the page as Cloudflare would serve it, with the headers and the 404 page, run it locally. This starts a local server only: it needs no Cloudflare account and uploads nothing.

```sh
cd site
WRANGLER_SEND_METRICS=false npx --yes wrangler@4.158.0 dev --local --port 8799
curl -sI http://127.0.0.1:8799/          # 200, with the headers from public/_headers
curl -sI http://127.0.0.1:8799/missing   # 404, the body is public/404.html
sh smoke.sh http://127.0.0.1:8799        # the live checks, against this server
```

Then look at it at a phone width and a desktop width, with reduced motion on, and tab through it with the keyboard.

## Deploy

A change to `site/` deploys itself when it lands on `main`: merging the pull request is the approval. The maintainer approved the site on 2026-11-07 and, the same day, asked for every change to deploy this way.

`.github/workflows/site.yml` runs `check.sh` on every pull request that touches `site/`, `README.md` or `docs/assets/`. On a push to `main` that touches `site/`, it runs `check.sh` again, deploys `site/public` with Wrangler under the message `site at SHORT_SHA`, and runs `smoke.sh` against https://residue.whiteheap.com, which waits up to a minute for every served file to match this commit byte for byte and for the 404 page, the Content Security Policy header and the zone's HSTS header. A failed deploy or smoke fails the run, and GitHub notifies whoever merged. The run's log has the Cloudflare version ID. Deploys run one at a time: a newer one waits for the one in flight, and of several waiting only the newest runs.

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

Leave https://whiteheap.github.io/residue/ alone. GitHub Pages serves the signed apt, dnf and pacman repositories there; this site never replaces or redirects it.

## Search engines and agents

The page has a canonical URL, Open Graph and Twitter tags, a social image and JSON-LD, and no `noindex`: search engines may index it, and a link to it shows `og.png` in previews. The README links to it. `robots.txt` lets every crawler in and names the sitemap, and `llms.txt` tells agents what residue is and where its install steps, docs and source are.

After a deploy that changed the page, tell search engines through IndexNow. Send only the URLs whose content changed, never unchanged ones:

```sh
curl -sS -o /dev/null -w '%{http_code}\n' -X POST https://api.indexnow.org/indexnow \
  -H 'Content-Type: application/json; charset=utf-8' \
  -d '{"host":"residue.whiteheap.com","key":"b0581e2550b0bcc904c146120f116cc0","keyLocation":"https://residue.whiteheap.com/b0581e2550b0bcc904c146120f116cc0.txt","urlList":["https://residue.whiteheap.com/"]}'
```

It prints `200` or `202` when IndexNow accepts the URLs. The key is the name of `public/b0581e2550b0bcc904c146120f116cc0.txt`, which search engines fetch to check that the request came from this site.

Bing Webmaster Tools verifies the site through the `msvalidate.01` tag in `index.html`; keep it. After a change to `robots.txt` or `sitemap.xml` goes live, submit the sitemap again in Bing Webmaster Tools and Google Search Console.

### HTTPS

http:// answers with a 301 to https://. The whiteheap.com zone's HSTS setting (SSL/TLS > Edge Certificates) owns `Strict-Transport-Security`: it sends the header on every host in the zone and replaces any a Worker sends, so `_headers` sets none. On 2026-11-08 it sent `max-age=2592000; includeSubDomains; preload`. `smoke.sh` checks that the header is there, with `includeSubDomains` and a `max-age` of at least 2592000 (30 days), and skips it over plain http, where browsers ignore it. To see it, run `curl -sI https://residue.whiteheap.com/ | grep -i strict-transport`.

## Roll back or take it down

The usual way back is a revert on `main`: it deploys like any other change. Each deploy is also a Cloudflare version that holds its assets, so you can go back to one at once:

```sh
npx --yes wrangler@4.158.0 versions list --name residue
npx --yes wrangler@4.158.0 rollback VERSION_ID --name residue --message "why"
```

The next push to `main` that touches `site/` deploys over a rollback, so revert the change on `main` too.

To take the site down, remove `residue.whiteheap.com` from the Worker's Settings > Domains & Routes in the Cloudflare dashboard, and the `routes` entry from `wrangler.jsonc` so the next deploy does not attach it again, or delete the Worker with `npx --yes wrangler@4.158.0 delete residue` and remove `.github/workflows/site.yml`. Either takes the site away from every reader, so it waits for the maintainer's word.
