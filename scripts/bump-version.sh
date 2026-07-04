#!/bin/bash
# bump-version.sh — bump tt-toplike's version everywhere in one shot
#
# Cargo.toml is the source of truth for the crate version, but three other files
# hard-code it and must stay in lockstep (CI's version-consistency job enforces
# this). This script rewrites all four so they never drift:
#   1. Cargo.toml            — [package] version = "X"
#   2. QUICK_START.md        — **Version**: X
#   3. site/index.html       — hero eyebrow "vX · Rust"
#   4. debian/changelog      — the version token in the first line
#
# Usage:
#   ./scripts/bump-version.sh <new-version>     # e.g. 0.7.20 or 1.0.0-rc.1
#
# It does NOT write a changelog body or commit — after running, edit the
# debian/changelog stanza to describe the release and commit the result.

set -euo pipefail

# Self-locating: resolve the repo root as the parent of this script's dir so the
# seds hit the right files no matter where the script is invoked from.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Parse + validate argument ─────────────────────────────────────────────────
if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <new-version>   (e.g. 0.7.20 or 1.0.0-rc.1)" >&2
    exit 1
fi
NEW_VERSION="$1"

# Semver-ish: MAJOR.MINOR.PATCH with an optional pre-release suffix.
if ! echo "$NEW_VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    echo "ERROR: '$NEW_VERSION' is not a valid version (expected MAJOR.MINOR.PATCH[-pre])" >&2
    exit 1
fi

# ── Sanity-check the files exist before touching anything ─────────────────────
for f in Cargo.toml QUICK_START.md site/index.html debian/changelog; do
    if [ ! -f "$f" ]; then
        echo "ERROR: expected file '$f' not found — run from the repo checkout." >&2
        exit 1
    fi
done

echo "╔══════════════════════════════════════════╗"
echo "║  tt-toplike version bump → $NEW_VERSION"
echo "╚══════════════════════════════════════════╝"
echo ""

# ── Rewrite each file with its exact current pattern ──────────────────────────
# Cargo.toml: the package version is the only `version = "..."` anchored at
# column 0 (dependency versions are indented or inline in { ... } tables).
sed -i -E "s/^version = \"[^\"]*\"/version = \"$NEW_VERSION\"/" Cargo.toml

# QUICK_START.md: the "**Version**: X" line near the top.
sed -i -E "s/^\*\*Version\*\*: .*/**Version**: $NEW_VERSION/" QUICK_START.md

# site/index.html: the hero eyebrow "... · vX · Rust". Match from "hero-eyebrow"
# up to the vX.Y.Z token (the run in between has no 'v'), leaving " · Rust" alone.
sed -i -E "s/(hero-eyebrow[^v]*)v[0-9]+\.[0-9]+\.[0-9]+/\1v$NEW_VERSION/" site/index.html

# debian/changelog: only the version token in the first stanza's header line.
sed -i -E "1s/^tt-toplike \([^)]*\)/tt-toplike ($NEW_VERSION)/" debian/changelog

# ── Report ────────────────────────────────────────────────────────────────────
echo "Updated files:"
git diff --stat -- Cargo.toml QUICK_START.md site/index.html debian/changelog || true
echo ""
echo "Next steps:"
echo "  1. Edit debian/changelog — replace the stanza body with the real"
echo "     $NEW_VERSION changes (this script only bumped the version token)."
echo "  2. Review: git diff"
echo "  3. Commit: git commit -am \"release: bump to v$NEW_VERSION\""
