#!/usr/bin/env python3
"""Builds the HTML docs in docs/ from the sources in docs/src/.

    python3 docs/build.py

Markdown pages (docs/src/*.md) and the overview (docs/src/index.html) go
through one template with docs/src/style.css inlined, so every page stands
alone: on GitHub Pages, opened as a file, or copied into a notes vault.
Mermaid diagrams are drawn to SVG here, in a light and a dark version, so the
pages need no script to show them.

Needs python-markdown and the mermaid CLI (`mmdc`, from
`npx @mermaid-js/mermaid-cli`, or set MMDC) with a Chromium (CHROMIUM).
Drawn diagrams are cached in target/docs-diagrams/.
"""

import hashlib
import html
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import markdown

DOCS = Path(__file__).resolve().parent
SRC = DOCS / "src"
CACHE = DOCS.parent / "target" / "docs-diagrams"
REPO = "https://github.com/bastianhenneberg/imkr"

# The chapters, in the order of the menu and of prev/next.
GROUPS = [
    ("Start", ["index", "getting-started", "architecture"]),
    ("Guides", ["install", "sessions", "screen", "agents", "computer-use",
                "ai-brain", "machines", "hives", "phone", "plugins", "settings"]),
    ("Reference", ["keys", "commands", "comparison"]),
]

FONT = ("https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wght@12..96,500;"
        "12..96,700;12..96,800&family=IBM+Plex+Sans:wght@400;500;600&family=JetBrains+Mono:"
        "wght@400;600&display=swap")

# Diagram colours, the same honey as the pages. Plain sans-serif: labels are
# measured here, and a web font that is wider where they are shown clips them.
THEMES = {
    "light": {"theme": "base", "themeVariables": {
        "fontFamily": "sans-serif", "fontSize": "14px",
        "primaryColor": "#f6e6c7", "primaryBorderColor": "#c9861a", "primaryTextColor": "#1d1f24",
        "secondaryColor": "#eef0f3", "tertiaryColor": "#ffffff", "lineColor": "#5d616b",
        "textColor": "#1d1f24", "noteBkgColor": "#fff7e6", "noteBorderColor": "#c9861a",
        "clusterBkg": "#faf8f3", "clusterBorder": "#dddad1", "edgeLabelBackground": "#ffffff"}},
    "dark": {"theme": "base", "themeVariables": {
        "darkMode": True, "fontFamily": "sans-serif", "fontSize": "14px",
        "primaryColor": "#3a2d15", "primaryBorderColor": "#e9a83a", "primaryTextColor": "#ecebe6",
        "secondaryColor": "#23262e", "tertiaryColor": "#1a1c21", "lineColor": "#a2a6af",
        "textColor": "#ecebe6", "noteBkgColor": "#2b2416", "noteBorderColor": "#e9a83a",
        "clusterBkg": "#1d1f25", "clusterBorder": "#2c2f36", "edgeLabelBackground": "#1a1c21"}},
}

PAGE_CSS = """
.shell { display: grid; grid-template-columns: minmax(0, 230px) minmax(0, 1fr); gap: 40px;
  max-width: 1240px; margin: 0 auto; padding-inline: 20px; padding-block: 28px 80px; }
.toc { position: sticky; top: calc(env(safe-area-inset-top, 0px) + 20px); align-self: start;
  max-height: calc(100vh - 40px); overflow-y: auto; display: grid; gap: 18px; font-size: 0.92rem; }
.toc .brand { font: 800 1.3rem var(--display); color: var(--ink); text-decoration: none; }
.toc .grp { display: grid; gap: 2px; }
.toc .grp > span { margin-bottom: 4px; }
.toc a.item { color: var(--ink); text-decoration: none; padding: 3px 10px; border-radius: 6px;
  border-left: 2px solid transparent; }
.toc a.item:hover { background: var(--surface); }
.toc a.item[aria-current] { border-left-color: var(--honey); background: var(--surface); font-weight: 600; }
.toc .groups { display: grid; gap: 18px; }
.toc details > summary { display: none; }
main { min-width: 0; }
.doc { max-width: 78ch; display: grid; gap: 16px; }
.doc h1 { margin-bottom: 8px; }
.doc h2 { margin-top: 28px; padding-top: 24px; border-top: 1px solid var(--line); }
.doc h3 { margin-top: 12px; }
.doc ul, .doc ol { margin: 0; padding-left: 1.3em; display: grid; gap: 6px; }
.doc pre { margin: 0; background: var(--term-bg); color: var(--term-fg); border-radius: 8px;
  padding: 12px 14px; overflow-x: auto; font: 400 0.84rem/1.55 var(--mono); }
.doc pre code { background: none; padding: 0; font-size: inherit; color: inherit; }
.doc blockquote { margin: 0; border-left: 3px solid var(--honey); padding: 8px 14px;
  background: var(--surface); border-radius: 0 8px 8px 0; }
.doc table { display: block; overflow-x: auto; }
.doc td code, .doc th code { white-space: nowrap; }
.diagram { margin: 0; background: var(--surface); border: 1px solid var(--line); border-radius: 12px;
  padding: 18px; overflow-x: auto; text-align: center; }
.diagram svg { max-width: 100%; height: auto; }
.d-dark { display: none; }
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) .d-light { display: none; }
  :root:not([data-theme="light"]) .d-dark { display: block; }
}
:root[data-theme="dark"] .d-light { display: none; }
:root[data-theme="dark"] .d-dark { display: block; }
.pager { display: flex; justify-content: space-between; gap: 12px; flex-wrap: wrap;
  margin-top: 48px; padding-top: 20px; border-top: 1px solid var(--line); }
.pager a { display: grid; gap: 2px; padding: 10px 14px; border: 1px solid var(--line);
  border-radius: 10px; text-decoration: none; color: var(--ink); background: var(--surface); min-width: 0; }
.pager a span { font-size: 0.8rem; color: var(--muted); }
.pager a:hover { border-color: var(--honey); }
.edit { font-size: 0.85rem; color: var(--muted); margin-top: 16px; }
@media (max-width: 860px) {
  .shell { grid-template-columns: minmax(0, 1fr); gap: 20px; }
  .toc { position: static; max-height: none; }
  .toc details > summary { display: block; cursor: pointer; font-weight: 600; padding: 8px 0; }
  .toc details:not([open]) .groups { display: none; }
}
"""


def mmdc_command():
    if os.environ.get("MMDC"):
        return [os.environ["MMDC"]]
    found = shutil.which("mmdc")
    if found:
        return [found]
    for bin_dir in Path.home().glob(".npm/_npx/*/node_modules/.bin"):
        if (bin_dir / "mmdc").exists():
            return [str(bin_dir / "mmdc")]
    return ["npx", "--yes", "@mermaid-js/mermaid-cli"]


def draw(source, svg_id):
    """One diagram, light and dark, as SVG text; cached by content."""
    out = {}
    for name, config in THEMES.items():
        key = hashlib.sha256(json.dumps([source, config, svg_id]).encode()).hexdigest()[:20]
        cached = CACHE / f"{key}.svg"
        if not cached.exists():
            CACHE.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory() as tmp:
                tmp = Path(tmp)
                (tmp / "d.mmd").write_text(source)
                (tmp / "c.json").write_text(json.dumps(config))
                (tmp / "p.json").write_text(json.dumps({
                    "executablePath": os.environ.get("CHROMIUM", shutil.which("chromium") or ""),
                    "args": ["--no-sandbox"]}))
                subprocess.run(mmdc_command() + [
                    "-q", "-i", str(tmp / "d.mmd"), "-o", str(tmp / "d.svg"),
                    "-c", str(tmp / "c.json"), "-p", str(tmp / "p.json"),
                    "-b", "transparent", "-I", f"{svg_id}-{name}"], check=True)
                svg = (tmp / "d.svg").read_text()
                # Coordinates come with 16 decimals; one is plenty and
                # makes the pages several times smaller.
                svg = re.sub(r"-?\d+\.\d{2,}",
                             lambda m: f"{float(m.group()):.1f}".removesuffix(".0"), svg)
                cached.write_text(svg)
        out[name] = cached.read_text()
    return (f'<figure class="diagram"><div class="d-light">{out["light"]}</div>'
            f'<div class="d-dark">{out["dark"]}</div></figure>')


def title_of(page):
    if page == "index":
        return "Overview"
    first = (SRC / f"{page}.md").read_text().splitlines()[0]
    return first.lstrip("# ").strip()


def body_of(page):
    diagrams = []

    def keep(match):
        diagrams.append(html.unescape(match.group(1)))
        return f"\n\nDIAGRAM{len(diagrams) - 1}DIAGRAM\n\n"

    if page == "index":
        text = (SRC / "index.html").read_text()
        text = re.sub(r'<pre class="mermaid">\n?(.*?)</pre>', keep, text, flags=re.S)
        body = text
    else:
        text = (SRC / f"{page}.md").read_text()
        text = re.sub(r"```mermaid\n(.*?)```", keep, text, flags=re.S)
        body = markdown.markdown(text, extensions=["tables", "fenced_code", "toc"])
        body = f'<article class="doc">{body}</article>'
    for i, source in enumerate(diagrams):
        drawn = draw(source, f"{page}-{i}")
        body = re.sub(rf"(<p>)?DIAGRAM{i}DIAGRAM(</p>)?", lambda _: drawn, body)
    # Pages link to each other as .md in the sources, .html here.
    body = re.sub(r'href="([a-z-]+)\.md(#[^"]*)?"', r'href="\1.html\2"', body)
    return body


def nav(current):
    groups = []
    for name, pages in GROUPS:
        items = "".join(
            f'<a class="item" href="{p}.html"{" aria-current=\"page\"" if p == current else ""}>'
            f"{html.escape(title_of(p))}</a>" for p in pages)
        groups.append(f'<div class="grp"><span class="eyebrow">{name}</span>{items}</div>')
    return (f'<nav class="toc" aria-label="Chapters"><a class="brand" href="index.html">imkr 🐝</a>'
            f'<details open><summary>Chapters</summary><div class="groups">{"".join(groups)}</div></details></nav>')


def pager(page, order):
    i = order.index(page)
    links = []
    if i > 0:
        links.append(f'<a href="{order[i - 1]}.html"><span>← Back</span>{html.escape(title_of(order[i - 1]))}</a>')
    else:
        links.append("<span></span>")
    if i + 1 < len(order):
        links.append(f'<a href="{order[i + 1]}.html" style="text-align:right"><span>Next →</span>'
                     f"{html.escape(title_of(order[i + 1]))}</a>")
    source = "index.html" if page == "index" else f"{page}.md"
    return (f'<div class="pager">{"".join(links)}</div>'
            f'<p class="edit">Source: <a href="{REPO}/blob/main/docs/src/{source}">docs/src/{source}</a></p>')


def main():
    if subprocess.run([sys.executable, "-c", "import markdown"]).returncode:
        sys.exit("needs python-markdown")
    css = (SRC / "style.css").read_text() + PAGE_CSS
    order = [p for _, pages in GROUPS for p in pages]
    for page in order:
        title = "imkr" if page == "index" else f"{title_of(page)} · imkr"
        main_class = "page" if page == "index" else ""
        out = f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>{html.escape(title)}</title>
<link rel="stylesheet" href="{FONT}">
<style>
{css}</style>
</head>
<body>
<div class="shell">
{nav(page)}
<main class="{main_class}">
{body_of(page)}
{pager(page, order)}
</main>
</div>
</body>
</html>
"""
        (DOCS / f"{page}.html").write_text(out)
        print(f"docs/{page}.html", len(out) // 1024, "KB")


if __name__ == "__main__":
    main()
