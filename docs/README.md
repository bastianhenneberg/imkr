# imkr documentation

The documentation as a website: open [index.html](index.html) in a browser (or the GitHub Pages
site, once it serves this folder). Every page stands alone, diagrams included.

The sources are Markdown in [src/](src/), readable right here on GitHub; start with
[Getting started](src/getting-started.md).

Rebuild the HTML after changing a source:

```bash
python3 docs/build.py
```

It needs python-markdown and the mermaid CLI (`npx @mermaid-js/mermaid-cli`) with Chromium.
`src/keys.md` and `src/commands.md` come from the code: `IMKR_WRITE_DOCS=1 cargo test docs_`.
