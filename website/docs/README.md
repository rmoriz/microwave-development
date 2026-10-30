# Microwave Server documentation site

Docusaurus 3.10 site that publishes a curated subset of the repository's
`docs/` folder. The markdown files are the source of truth and are never
copied: `docusaurus.config.ts` points the docs plugin at `../../docs` and lists
the pages that are published.

```sh
npm install
npm run dev      # http://localhost:3000/docs/
npm run build    # static site into build/
npm run preview  # serve build/ locally
```

## Adding a page

1. Write the guide as `docs/<name>.md` in the repository.
2. Add the file name to `docs.include` in `docusaurus.config.ts`.
3. Add the doc id (the file name without `.md`) to `sidebars.ts`.

Pages that are not in `include` stay repository-only, which is how the internal
review and pedant scratch reports are kept off the site.

## Links

Links inside `docs/` that point outside that folder (`../crates/...`,
`../Dockerfile`, `superpowers/...`) are rewritten to GitHub URLs at build time
by `plugins/github-relative-links.mjs`, so the same markdown renders correctly
on GitHub and on the site. Links between published pages can use the plain
`./guide.md` form.

## Broken anchors

`docs/remaining-work.md` marks each gap with a raw `<a id="gap-...">` anchor.
Docusaurus does not treat those as headings, so `onBrokenAnchors` is `warn`;
the anchors themselves work.

## CI

A change under `docs/` or `website/docs/` runs the `claims` and `docs` jobs
only; fmt, clippy, the test suite, the Postgres suite and the homepage build
are skipped by the filters in `.github/workflows/ci.yml`. Editing a workflow
runs everything.
