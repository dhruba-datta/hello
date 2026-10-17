# Hello website

The product page for Hello, meant for **hello.minutecampus.com**. Plain HTML, CSS and JavaScript:
no build step, no dependencies, no tracking. It uses the system font, so nothing is loaded from other servers.

| File | Contents |
|------|----------|
| `index.html` | All page content |
| `styles.css` | Styles (dark by default, light when the system asks; brand gradient from the app icon) |
| `app.js` | Hero typing demo, indicator picker (five styles, Top or Bottom), Bangla examples, mobile menu, active section |
| `assets/` | App icon, favicon and touch icon |

## Run locally

```bash
npx --yes serve@14.2.4 -l 4321 website   # from the repository root
```

Then open http://localhost:4321.

## Publish

The site is the Cloudflare Worker `hello-website` (static assets only, set up in `wrangler.jsonc`),
served at hello.minutecampus.com. From this `website` folder:

```bash
npx --yes wrangler@4 deploy
```

| File | Purpose |
|------|---------|
| `_redirects` | `/download` → the latest `Hello.dmg` on this repository's GitHub Releases |
| `_headers` | Short cache for `appcast.xml` so updates show up quickly |
| `appcast.xml` | Update feed read by the app; written by `../release-hello.sh` |

Shipping a new version is described in [Releasing](../README.md#releasing) in the main README.
