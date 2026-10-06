#!/usr/bin/env python3
"""Guard the sole ambient OS module, including proof/measurement code.

Catches qualified std paths, grouped imports, and std aliases. This is a
repository architecture guard, not a Rust parser or a hostile-source sandbox.
"""
from pathlib import Path
import re
import sys
root = Path(__file__).resolve().parents[1] / 'crates/moonseed/src'
violations = []
for path in sorted(root.rglob('*.rs')):
    if path == root / 'hostcaps/native.rs':
        continue
    source = path.read_text()
    source = re.sub(r'/\*.*?\*/|//[^\n]*', '', source, flags=re.S)
    patterns = [r'\bstd\s*::\s*(?:fs|env|process|time)\b',
                r'\buse\s+std\s*::\s*\{[^;]*(?:\bfs\b|\benv\b|\bprocess\b|\btime\b)',
                r'\b(?:use|extern\s+crate)\s+std\s+as\b']
    for pattern in patterns:
        if re.search(pattern, source):
            violations.append(str(path.relative_to(root)))
            break
if violations:
    sys.exit('ambient std authority outside hostcaps/native.rs: ' + ', '.join(violations))
print('host boundary: all std fs/env/process/time calls isolated in hostcaps/native.rs')
