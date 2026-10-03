#!/usr/bin/env bash
# Run against the existing Windows Chrome CDP session, one GPU page only.
# The caller must reserve that page and restart the 0.9 backend first.
set -euo pipefail
cd "$(dirname "$0")/.."
renderer_cli=${WB_RENDERER_CLI:-/tmp/wb-win-cli.sh}
renderer_windows_dir=${WB_RENDERER_WINDOWS_DIR:-/mnt/d/Workspace/.world-builder-perf}
renderer_output=output/playwright/renderer-09
mkdir -p "$renderer_output" "$renderer_windows_dir/output/playwright"
cp scripts/qa-renderer-browser.js "$renderer_windows_dir/qa-renderer-browser-09.js"
# --filename is interpreted by the Windows Node process in the CLI working dir.
bash "$renderer_cli" run-code 'async (page) => {
  const errors = new Set();
  const consoleError = (entry) => { if (entry.type() === "error") errors.add(entry.text()); };
  const pageError = (error) => errors.add(error.message);
  page.on("console", consoleError); page.on("pageerror", pageError);
  try {
    await page.goto("http://localhost:8001/");
    await page.waitForFunction(() => typeof window.worldBuilderPerf?.().view?.active_level === "number"
      && document.getElementById("stats")?.textContent.includes("d8d1261ca22a1815"), null, { timeout: 60000 });
    return { loaded: true, errors: [...errors] };
  } finally {
    page.removeListener("console", consoleError); page.removeListener("pageerror", pageError);
  }
}' > "$renderer_output/navigation.log"
python3 - "$renderer_output/navigation.log" <<'PY'
import json
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text()
marker = '### Result\n'
if marker not in text:
    raise RuntimeError(f'QA navigation failed: {text[:1000]}')
data = json.JSONDecoder().raw_decode(text.split(marker, 1)[1].lstrip())[0]
assert data.get('loaded') is True and not data['errors'], data
PY
bash "$renderer_cli" snapshot > "$renderer_output/initial-snapshot.log"
bash "$renderer_cli" run-code --filename qa-renderer-browser-09.js > "$renderer_output/setup.log"
python3 - "$renderer_output/setup.log" <<'PY'
import json
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text()
marker = '### Result\n'
if marker not in text:
    raise RuntimeError(f'QA setup failed: {text[:1000]}')
data = json.JSONDecoder().raw_decode(text.split(marker, 1)[1].lstrip())[0]
assert data.get('setup') is True, data
PY
for renderer_case in $(seq 0 13); do
  bash "$renderer_cli" run-code --filename qa-renderer-browser-09.js > "$renderer_output/case-$renderer_case.log"
  cp "$renderer_windows_dir/output/playwright/renderer-09-"*.png "$renderer_output/"
  printf 'Renderer QA case %s finished\n' "$renderer_case"
done
bash "$renderer_cli" run-code --filename qa-renderer-browser-09.js > "$renderer_output/final.log"
bash "$renderer_cli" console error > "$renderer_output/console-errors.log"
python3 - "$renderer_output" <<'PY'
import json
import pathlib
import sys

directory = pathlib.Path(sys.argv[1])

def result(name):
    text = (directory / name).read_text()
    marker = '### Result\n'
    if marker not in text:
        raise RuntimeError(f'CLI returned no result: {name}: {text[:300]}')
    return json.JSONDecoder().raw_decode(text.split(marker, 1)[1].lstrip())[0]

setup = result('setup.log')
navigation = result('navigation.log')
cases = [result(f'case-{i}.log') for i in range(14)]
report = {'setup': setup, 'navigation': navigation, 'results': cases, 'errors': navigation['errors'] + setup['errors'] + [e for c in cases for e in c['errors']],
          'measurement': 'rAF cadence and main-thread longtasks; CPU submission is not GPU timing',
          'cache_state': 'page reloaded before setup: client RAM cold; server and persistent tile caches may already be warm'}
(directory / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
print(directory / 'report.json')
PY
