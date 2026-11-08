#!/usr/bin/env bash
# Prints the next release version from the commit subjects since the last tag:
# 💥 → major, else ✨ → minor, else patch.
set -euo pipefail

last=$(git describe --tags --abbrev=0 --match 'v*.*.*' 2>/dev/null || true)
if [[ -z $last ]]; then
  echo v0.1.0
  exit
fi

subjects=$(git log --format=%s "$last..HEAD")
if [[ -z $subjects ]]; then
  echo "nothing to release: no commits since $last" >&2
  exit 1
fi

IFS=. read -r major minor patch <<<"${last#v}"
if grep -qE '^(AB#[0-9]+ )?💥' <<<"$subjects"; then
  echo "v$((major + 1)).0.0"
elif grep -qE '^(AB#[0-9]+ )?✨' <<<"$subjects"; then
  echo "v$major.$((minor + 1)).0"
else
  echo "v$major.$minor.$((patch + 1))"
fi
