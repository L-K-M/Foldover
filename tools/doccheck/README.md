# Documentation checks

- `python3 tools/doccheck/checkdocs.py .` checks every relative link and anchor in the repository's Markdown files. It also reports leaked scratch paths, unbalanced code fences and table rows whose cell count differs from their header. Run it from the repository root.
- `node tools/doccheck/mmdcheck.mjs <repo-root>` parses every Mermaid diagram in the same files with mermaid 11. It needs `npm i mermaid@11 jsdom@25` in this folder (or any folder on Node's module path).
- `python3 -m unittest discover -s tools/doccheck -p 'test_*.py'` tests the first checker.

Both checkers read tracked and new files and skip what `.gitignore` excludes, such as `dist/` and `build/`. Each exits with status 1 when it finds a problem, which fails the `docs` CI job.

Run both after every documentation change; both must report 0 problems.
