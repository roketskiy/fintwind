import json
import re
from pathlib import Path

root = Path(__file__).resolve().parent.parent
source = (root / 'crates/fintwind-protocol/src/protocol.rs').read_text(encoding='utf-8')
body = source.split('pub enum Command {', 1)[1].split('\n}', 1)[0]
commands = re.findall(r'^    ([A-Z][A-Za-z0-9]*)(?:\s*\{|,)', body, re.MULTILINE)
go_root = root / 'fintwind-go/internal'
sources = list(go_root.glob('backend*/*.go'))
entries = []
for command in commands:
    wire = command[0].lower() + command[1:]
    locations = []
    for path in sources:
        for number, line in enumerate(path.read_text(encoding='utf-8').splitlines(), 1):
            if re.search(r'"' + re.escape(wire) + r'"', line):
                locations.append(f'{path.relative_to(root).as_posix()}:{number}')
    entries.append({'command': wire, 'references': locations, 'verified': False})
output = root / 'docs/dev-notes/go-backend/local-command-inventory.json'
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(json.dumps({'note': 'Lexical references are not proof of implementation or E2E coverage.', 'commands': entries}, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
print(f'[OK] Inventoried {len(entries)} commands; verification remains explicit')
for entry in entries:
    if not entry['references']:
        print(f'[MISSING_REFERENCE] {entry["command"]}')
