import { JSDOM } from 'jsdom';
import fs from 'fs';
import path from 'path';
import { execFileSync } from 'child_process';
const dom = new JSDOM('<!doctype html><html><body></body></html>', { pretendToBeVisual: true });
globalThis.window = dom.window; globalThis.document = dom.window.document;
try { globalThis.navigator = dom.window.navigator; } catch {}
globalThis.DOMParser = dom.window.DOMParser; globalThis.Element = dom.window.Element;
const { default: mermaid } = await import('mermaid');
mermaid.initialize({ startOnLoad: false });
const root = process.argv[2];
// Tracked and new Markdown files; ignored build output is not documentation.
function markdownFiles() {
  try {
    const listed = execFileSync('git', ['-C', root, 'ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', '*.md'],
      { stdio: ['ignore', 'pipe', 'ignore'] }).toString('utf8').split('\0');
    // A tracked file deleted in the work tree is still listed.
    return listed.filter(Boolean).map((name) => path.join(root, name)).filter((f) => fs.existsSync(f));
  } catch {
    // Not a git checkout (an exported archive): walk it.
  }
  const found = [];
  (function walk(d) {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      if (e.name === '.git' || e.name === 'node_modules') continue;
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name.endsWith('.md')) found.push(p);
    }
  })(root);
  return found;
}
const files = markdownFiles().sort();
let bad = 0, total = 0;
for (const f of files) {
  const lines = fs.readFileSync(f, 'utf8').split('\n');
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].trim() === '```mermaid') {
      const start = i + 1; const buf = [];
      i++;
      while (i < lines.length && lines[i].trim() !== '```') { buf.push(lines[i]); i++; }
      total++;
      try { await mermaid.parse(buf.join('\n')); }
      catch (e) { bad++; console.log(`BAD ${f}:${start}: ${String(e.message || e).split('\n').slice(0, 4).join(' | ')}`); }
    }
  }
}
console.log(`${bad} invalid of ${total} diagrams`);
// Not process.exit(): it can end the process before the lines above
// reach a pipe.
process.exitCode = bad ? 1 : 0;
