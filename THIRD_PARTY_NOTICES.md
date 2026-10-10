# Third-party notices

## Mermaid 11.17.2 (embedded by default)

`assets/vendor/mermaid.min.js` is the unmodified `dist/mermaid.min.js` from the
`mermaid@11.17.2` npm package (sha256 `581ed7d74bd9048d0e3a91363927d72ef22942d7722546b27f7cc29e35390eb8`),
MIT License, Copyright (c) 2014 - 2022 Knut Sveidqvist. Full text: `assets/vendor/mermaid.LICENSE`.

The bundle also contains code from its own dependencies, whose notices are kept in the
file's `/*! ... */` comments, including DOMPurify (Apache-2.0 OR MPL-2.0, Cure53),
and Bezier/spring helpers (MIT). Build with `--no-default-features` to omit all of it.

To update: replace the file, update the version and hash above, and run
`tests/js/mermaid-parse.mjs` (see docs/MERMAID.md).
