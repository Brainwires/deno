// deno-lint-ignore-file
// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Node.js contributors. All rights reserved. MIT License.
// Ported from Node.js v26.5.1 (lib/internal/vfs/router.js).

(function () {
const { core, primordials } = __bootstrap;

const {
  ArrayPrototypeJoin,
  StringPrototypeSplit,
  StringPrototypeStartsWith,
} = primordials;

const { isAbsolute, relative, sep } = core.loadExtScript(
  "ext:deno_node/path/mod.ts",
);

// `path.sep` is required here because on Windows `path.resolve('/virtual')`
// produces 'C:\virtual' and all resolved paths use backslashes - a hardcoded
// '/' check would never match. The trailing-separator guard handles root
// mount points like 'C:\' so we don't end up with 'C:\\'.
function isUnderMountPoint(normalizedPath, mountPoint) {
  if (normalizedPath === mountPoint) {
    return true;
  }
  if (mountPoint === "/") {
    return StringPrototypeStartsWith(normalizedPath, "/");
  }
  const prefix = mountPoint[mountPoint.length - 1] === sep
    ? mountPoint
    : mountPoint + sep;
  return StringPrototypeStartsWith(normalizedPath, prefix);
}

// Returns a POSIX-style relative path the provider can consume. Uses
// `path.relative()` so Windows backslash paths are handled correctly, then
// re-joins with forward slashes for the provider's internal POSIX format.
function getRelativePath(normalizedPath, mountPoint) {
  if (normalizedPath === mountPoint) {
    return "/";
  }
  if (mountPoint === "/") {
    return normalizedPath;
  }
  const rel = relative(mountPoint, normalizedPath);
  const segments = StringPrototypeSplit(rel, sep);
  return "/" + ArrayPrototypeJoin(segments, "/");
}

return {
  isUnderMountPoint,
  getRelativePath,
  isAbsolutePath: isAbsolute,
};
})();
