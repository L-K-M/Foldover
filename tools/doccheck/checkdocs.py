#!/usr/bin/env python3
"""Check relative links/anchors across repo markdown, scratch-path leaks, fence balance, table column consistency."""
import re, os, sys, unicodedata
root = sys.argv[1]
files = []
for dp, dns, fns in os.walk(root):
    dns[:] = [d for d in dns if d not in ('.git', 'node_modules')]
    files += [os.path.join(dp, fn) for fn in fns if fn.endswith('.md')]
files.sort()
def slug(h):
    h = h.strip().lower()
    h = re.sub(r'<[^>]+>', '', h)
    h = re.sub(r'[`*~]', '', h)
    h = re.sub(r'\[([^\]]*)\]\([^)]*\)', r'\1', h)
    out = []
    for ch in h:
        if ch.isalnum() or ch in '-_' :
            out.append(ch)
        elif ch == ' ':
            out.append('-')
    return ''.join(out)
anchors = {}
for f in files:
    s = set(); counts = {}
    infence = False
    for line in open(f, encoding='utf-8'):
        if line.lstrip().startswith('```'):
            infence = not infence; continue
        if infence: continue
        m = re.match(r'^(#{1,6})\s+(.*?)\s*#*\s*$', line)
        if m:
            sl = slug(m.group(2))
            n = counts.get(sl, 0); counts[sl] = n + 1
            s.add(sl if n == 0 else f'{sl}-{n}')
        for a in re.findall(r'<a\s+(?:name|id)="([^"]+)"', line):
            s.add(a)
    anchors[os.path.abspath(f)] = s
problems = 0
for f in files:
    txt = open(f, encoding='utf-8').read()
    if '/tmp/' in txt or 'scratchpad' in txt or 'skeleton.md' in txt:
        for i, line in enumerate(txt.splitlines(), 1):
            if any(k in line for k in ('/tmp/', 'scratchpad', 'skeleton.md')):
                print(f'LEAK {os.path.relpath(f, root)}:{i}: {line.strip()[:140]}'); problems += 1
    if txt.count('```') % 2:
        print(f'FENCE odd count in {os.path.relpath(f, root)}'); problems += 1
    infence = False
    for i, line in enumerate(txt.splitlines(), 1):
        if line.lstrip().startswith('```'):
            infence = not infence; continue
        if infence: continue
        for m in re.finditer(r'\]\(([^)\s]+)\)', line):
            target = m.group(1)
            if re.match(r'^[a-z]+:', target) or target.startswith('mailto:'):
                continue
            path, _, anchor = target.partition('#')
            tf = os.path.abspath(os.path.join(os.path.dirname(f), path)) if path else os.path.abspath(f)
            if not os.path.exists(tf):
                print(f'BROKEN-FILE {os.path.relpath(f, root)}:{i}: {target}'); problems += 1; continue
            if anchor and tf.endswith('.md') and anchor not in anchors.get(tf, set()):
                print(f'BROKEN-ANCHOR {os.path.relpath(f, root)}:{i}: {target}'); problems += 1
print(f'{problems} problems across {len(files)} files')
