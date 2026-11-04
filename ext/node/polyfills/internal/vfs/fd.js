// deno-lint-ignore-file
// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Node.js contributors. All rights reserved. MIT License.
// Ported from Node.js v26.5.1 (lib/internal/vfs/fd.js).

(function () {
const { core, primordials } = __bootstrap;

const {
  SafeMap,
  Symbol,
} = primordials;

// Private symbols
const kFd = Symbol("kFd");
const kEntry = Symbol("kEntry");

// VFS FDs use bit 30 set to avoid conflicts with real OS fds.
// Real fds are small non-negative integers; VFS fds start at 0x40000000.
const VFS_FD_MASK = 0x40000000;
let nextFd = 0;

// Global registry of open virtual file descriptors
const openFDs = new SafeMap();

/**
 * Represents an open virtual file descriptor.
 * Wraps a VirtualFileHandle from the provider.
 */
class VirtualFD {
  /**
   * @param {number} fd The file descriptor number
   * @param {VirtualFileHandle} entry The virtual file handle
   */
  constructor(fd, entry) {
    this[kFd] = fd;
    this[kEntry] = entry;
  }

  /**
   * Gets the file descriptor number.
   * @returns {number}
   */
  get fd() {
    return this[kFd];
  }

  /**
   * Gets the file handle.
   * @returns {VirtualFileHandle}
   */
  get entry() {
    return this[kEntry];
  }
}

/**
 * Opens a virtual file and returns its file descriptor.
 * @param {VirtualFileHandle} entry The virtual file handle
 * @returns {number} The file descriptor
 */
function openVirtualFd(entry) {
  const fd = VFS_FD_MASK | nextFd++;
  const vfd = new VirtualFD(fd, entry);
  openFDs.set(fd, vfd);
  return fd;
}

/**
 * Gets a VirtualFD by its file descriptor number.
 * @param {number} fd The file descriptor number
 * @returns {VirtualFD|undefined}
 */
function getVirtualFd(fd) {
  return openFDs.get(fd);
}

/**
 * Closes a virtual file descriptor.
 * @param {number} fd The file descriptor number
 * @returns {boolean} True if the fd was found and closed
 */
function closeVirtualFd(fd) {
  return openFDs.delete(fd);
}

return {
  VFS_FD_MASK,
  VirtualFD,
  openVirtualFd,
  getVirtualFd,
  closeVirtualFd,
};
})();
