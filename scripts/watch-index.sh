#!/usr/bin/env bash
set -e
taskkill //F //IM prism-core.exe 2>/dev/null || true
sleep 0.3
: > /tmp/prism-core2.log
src/prism-core/target/debug/prism-core.exe >> /tmp/prism-core2.log 2>&1 &
BPID=$!
echo "started pid=$BPID"
for i in $(seq 1 25); do
  sleep 1
  echo "--- t=${i}s ---"
  cat /tmp/prism-core2.log
  python - <<'PY'
import json, time
from pathlib import Path
try:
    # use win32 named pipe via powershell is easier; try open
    pass
except Exception as e:
    print(e)
PY
  powershell -NoProfile -ExecutionPolicy Bypass -File /tmp/one-search.ps1 || true
  if grep -qE '索引|缓存|MFT|遍历' /tmp/prism-core2.log 2>/dev/null; then
    echo 'INDEX LOG SEEN'
    sleep 2
    powershell -NoProfile -ExecutionPolicy Bypass -File /tmp/one-search.ps1 || true
    break
  fi
done
taskkill //F //IM prism-core.exe 2>/dev/null || true
echo '==== final log ===='
cat /tmp/prism-core2.log
