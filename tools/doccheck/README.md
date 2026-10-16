# Documentation checks

- `python3 tools/doccheck/checkdocs.py .` checks every relative link and anchor across `README.md`, `CLAUDE.md`, `docs/PLAN.md` and `docs/design/*.md`. It also reports leaked scratch paths and unbalanced code fences. Run it from the repository root.
- `node tools/doccheck/mmdcheck.mjs <repo-root>` parses every Mermaid diagram in the same files with mermaid 11. It needs `npm i mermaid@11 jsdom@25` in this folder (or any folder on Node's module path).

Run both after every documentation change; both must report 0 problems.
