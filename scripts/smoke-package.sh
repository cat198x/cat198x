#!/usr/bin/env bash
set -euo pipefail
[ "$1" = cat198x ]
package_bin=$2
fixture_dir=$3/cat198x-smoke
mkdir -p "$fixture_dir/source"
printf 'A small catalogue fixture\n' > "$fixture_dir/source/sample.bin"
"$package_bin/cat198x" --data-dir "$fixture_dir/catalogue" init
"$package_bin/cat198x" --data-dir "$fixture_dir/catalogue" source add "$fixture_dir/source"
"$package_bin/cat198x" --data-dir "$fixture_dir/catalogue" scan
python3 - "$fixture_dir" <<'PY'
import hashlib
from pathlib import Path
import sqlite3
import sys
root = Path(sys.argv[1])
with sqlite3.connect(f'file:{root}/catalogue/db.sqlite?mode=ro', uri=True) as db:
    rows = db.execute('SELECT l.path, f.size, f.sha1 FROM files f JOIN file_locations l USING (sha1)').fetchall()
    data = (root / 'source/sample.bin').read_bytes()
    assert rows == [('sample.bin', len(data), hashlib.sha1(data).hexdigest().upper())], rows
PY
