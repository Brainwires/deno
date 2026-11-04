// deno-lint-ignore-file
// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Node.js contributors. All rights reserved. MIT License.
// Ported from Node.js v26.5.1 (lib/vfs.js).

(function () {
const { core, primordials } = __bootstrap;

const {
  FunctionPrototypeSymbolHasInstance,
} = primordials;

const { VirtualFileSystem } = core.loadExtScript(
  "ext:deno_node/internal/vfs/file_system.js",
);
const { VirtualProvider } = core.loadExtScript(
  "ext:deno_node/internal/vfs/provider.js",
);
const { MemoryProvider } = core.loadExtScript(
  "ext:deno_node/internal/vfs/providers/memory.js",
);
const { RealFSProvider } = core.loadExtScript(
  "ext:deno_node/internal/vfs/providers/real.js",
);

/**
 * Creates a new VirtualFileSystem instance.
 * @param {VirtualProvider} [provider] The provider to use (defaults to MemoryProvider)
 * @param {object} [options] Configuration options
 * @param {boolean} [options.moduleHooks] Whether to enable require/import hooks (default: true)
 * @param {boolean} [options.virtualCwd] Whether to enable virtual working directory
 * @returns {VirtualFileSystem}
 */
function create(provider, options) {
  // Handle case where first arg is options (no provider)
  if (
    provider != null &&
    !FunctionPrototypeSymbolHasInstance(VirtualProvider, provider) &&
    typeof provider === "object"
  ) {
    options = provider;
    provider = undefined;
  }
  return new VirtualFileSystem(provider, options);
}

return {
  create,
  VirtualFileSystem,
  VirtualProvider,
  MemoryProvider,
  RealFSProvider,
};
})();
