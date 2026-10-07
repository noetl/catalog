#!/usr/bin/env bash
# Stamp a release version into the workspace Cargo.toml.
#
# ⚠ WHY: `CARGO_PKG_VERSION` is compiled into the binary and reported by
# `/api/catalog/health` and by `catalog_build_info{version}`. semantic-release does not
# push its version bump back to `main` in this fleet, so a tag's checked-out tree carries
# a STALE floor — without this stamp the image would build, publish and deploy while
# reporting the PREVIOUS version, and `build_info` is the one series used to tell "this
# pod predates that metric" from "that metric never fired".
set -euo pipefail
V="${1:?usage: stamp-version.sh <version-without-v>}"
case "$V" in
  v*) echo "stamp-version.sh: pass the version WITHOUT the leading v (got $V)" >&2; exit 2 ;;
esac
# Only the [workspace.package] version, and only its first occurrence — a blanket
# s/version/…/ would rewrite every dependency pin in the file.
python3 - "$V" <<'PY'
import re, sys
v = sys.argv[1]
p = "Cargo.toml"
t = open(p).read()
new, n = re.subn(r'(?m)^(version\s*=\s*)"[^"]+"', r'\1"%s"' % v, t, count=1)
if n != 1:
    raise SystemExit("stamp-version.sh: expected exactly one version line to rewrite, rewrote %d" % n)
open(p, "w").write(new)
print("stamped version = %s" % v)
PY
# Assert it took. A stamp that silently did nothing is the failure this guards.
grep -qE "^version = \"$V\"" Cargo.toml || {
  echo "stamp-version.sh: Cargo.toml does not carry $V after stamping" >&2
  exit 1
}
