# Vendored front-end code

`web/public/number-flow.js` is **not our code**: it is [NumberFlow](https://number-flow.barvian.me/vanilla) (animated numbers) bundled into one file so the Trunk/Vercel build needs no npm step and no bundler.

| Package | Version | Licence | npm integrity (sha512) |
| --- | --- | --- | --- |
| `number-flow` (Maxwell Barvian) | 0.6.2 | MIT, see `number-flow.LICENSE` | `MCnImG4Q5vPwhSXnov56nOuyyKn6LC+Qd7II1UiKc+ACRtug5iAtn0+CwXNxM38AC5lSowEY+oYEtZX2qMnUyw==` |
| `esm-env` (its only dependency) | 1.2.2 | MIT, see `esm-env.LICENSE` | `Epxrv+Nr/CaL4ZcFGPJIYLWFom+YeV1DqMLHJoEd9SYRxNbaFruBwfEX/kkHUJf55j2+TUbmDcmuilbP1TmXHA==` |

Importing it registers the custom element `<number-flow>`. The app creates one inside `<span class="flow">` hosts through `lifeui.flow(id, value)` in `web/public/lifeui.js`; it falls back to plain text if the script has not loaded.

## Rebuild or upgrade (in a NEW, empty directory outside the repo; never run package scripts)

```powershell
mkdir $env:TEMP\nf; cd $env:TEMP\nf
npm install number-flow@<version> --ignore-scripts --no-audit --no-fund   # compare the integrity values above
Set-Content entry.mjs 'import "number-flow";'
<repo>\node_modules\.bin\esbuild.cmd entry.mjs --bundle --minify --format=esm --target=es2020 --legal-comments=none "--banner:js=/* number-flow <version> (MIT) + esm-env <version> (MIT), bundled with esbuild: see web/vendor/README.md */" --outfile=<repo>\web\public\number-flow.js
```

Then check the output for network or `eval` use (`fetch(`, `XMLHttpRequest`, `eval(`, `new Function`), update the table above, and bump the service-worker cache name in `web/public/sw.js`.

Why not the React libraries from the redesign brief (shadcn/ui, Radix Themes, Base UI, Vaul)? They are React components and this app is Leptos (Rust/WASM). Their design language (tokens, radius, soft badges, accessible dialog behaviour, a Vaul-style drag-to-dismiss sheet) is re-implemented in `web/style.css`, `web/src/ui.rs` and `web/public/lifeui.js`.
