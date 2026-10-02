#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:-}"

if [ -z "$VERSION" ]; then
  echo "Usage: $0 <version>"
  echo "  e.g. $0 1.3.0"
  echo "  e.g. $0 1.3.0-beta.0"
  exit 1
fi

VERSION="${VERSION#v}"
TAG="v${VERSION}"
if ! echo "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
  echo "Error: '${VERSION}' is not a valid version (expected X.Y.Z or X.Y.Z-pre.N)."
  exit 1
fi

# Ensure we're on a clean working tree
if [ -n "$(git status --porcelain)" ]; then
  echo "Error: Working tree is not clean. Commit or stash changes first."
  exit 1
fi

# Ensure we're up to date with remote
git fetch origin
BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [ "$(git rev-parse HEAD)" != "$(git rev-parse "origin/${BRANCH}")" ]; then
  echo "Error: Local branch '${BRANCH}' is not up to date with origin. Pull or push first."
  exit 1
fi

# Check tag doesn't already exist
if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "Error: Tag '${TAG}' already exists."
  exit 1
fi

echo "Updating version to ${VERSION} (tag ${TAG}) on branch ${BRANCH}"
echo ""

echo "Updating Cargo.toml..."
perl -0pi -e 's/(\[package\][^\[]*?^version = ")[^"]+(")/${1}'"${VERSION}"'${2}/ms' Cargo.toml

echo "Updating Cargo.lock..."
cargo update --workspace --quiet

NEW_VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
if [ "$NEW_VERSION" != "$VERSION" ]; then
  echo "Error: Cargo.toml version is '${NEW_VERSION}' after the edit, expected '${VERSION}'."
  exit 1
fi

echo ""
echo "Done! Version ${VERSION} updated in Cargo.toml and Cargo.lock."
echo "Next steps:"
echo "  Commit and merge a Pull Request with the version bump (git add Cargo.toml Cargo.lock)"
echo "  Push the (signed) tag from the merged commit: git tag ${TAG} && git push origin ${TAG}"
echo "  Trigger the Release workflow via GitHub UI with version: ${VERSION}"
