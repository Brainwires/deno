// deno-lint-ignore-file
// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Node.js contributors. All rights reserved. MIT License.
// Ported from Node.js v26.5.1 (lib/internal/vfs/errors.js).

(function () {
const { core, primordials } = __bootstrap;

const {
  ErrorCaptureStackTrace,
} = primordials;

const { uvException } = core.loadExtScript(
  "ext:deno_node/internal/errors.ts",
);
// Node's `new UVException(ctx)`: the same error `uvException(ctx)` builds.
function UVException(ctx) {
  return uvException(ctx);
}

const {
  UV_ENOENT,
  UV_ENOTDIR,
  UV_ENOTEMPTY,
  UV_EISDIR,
  UV_EBADF,
  UV_EEXIST,
  UV_EROFS,
  UV_EINVAL,
  UV_ELOOP,
  UV_EACCES,
  UV_EXDEV,
} = core.loadExtScript("ext:deno_node/internal_binding/uv.ts");

/**
 * Creates an ENOENT error for virtual file system operations.
 * @param {string} syscall The system call name
 * @param {string} path The path that was not found
 * @returns {Error}
 */
function createENOENT(syscall, path) {
  const err = new UVException({
    errno: UV_ENOENT,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createENOENT);
  return err;
}

/**
 * Creates an ENOTDIR error.
 * @param {string} syscall The system call name
 * @param {string} path The path that is not a directory
 * @returns {Error}
 */
function createENOTDIR(syscall, path) {
  const err = new UVException({
    errno: UV_ENOTDIR,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createENOTDIR);
  return err;
}

/**
 * Creates an ENOTEMPTY error for non-empty directory.
 * @param {string} syscall The system call name
 * @param {string} path The path of the non-empty directory
 * @returns {Error}
 */
function createENOTEMPTY(syscall, path) {
  const err = new UVException({
    errno: UV_ENOTEMPTY,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createENOTEMPTY);
  return err;
}

/**
 * Creates an EISDIR error.
 * @param {string} syscall The system call name
 * @param {string} path The path that is a directory
 * @returns {Error}
 */
function createEISDIR(syscall, path) {
  const err = new UVException({
    errno: UV_EISDIR,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createEISDIR);
  return err;
}

/**
 * Creates an EBADF error for invalid file descriptor operations.
 * @param {string} syscall The system call name
 * @returns {Error}
 */
function createEBADF(syscall) {
  const err = new UVException({
    errno: UV_EBADF,
    syscall,
  });
  ErrorCaptureStackTrace(err, createEBADF);
  return err;
}

/**
 * Creates an EEXIST error.
 * @param {string} syscall The system call name
 * @param {string} path The path that already exists
 * @returns {Error}
 */
function createEEXIST(syscall, path) {
  const err = new UVException({
    errno: UV_EEXIST,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createEEXIST);
  return err;
}

/**
 * Creates an EROFS error for read-only file system.
 * @param {string} syscall The system call name
 * @param {string} path The path
 * @returns {Error}
 */
function createEROFS(syscall, path) {
  const err = new UVException({
    errno: UV_EROFS,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createEROFS);
  return err;
}

/**
 * Creates an EINVAL error for invalid argument.
 * @param {string} syscall The system call name
 * @param {string} path The path
 * @returns {Error}
 */
function createEINVAL(syscall, path) {
  const err = new UVException({
    errno: UV_EINVAL,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createEINVAL);
  return err;
}

/**
 * Creates an ELOOP error for too many symbolic links.
 * @param {string} syscall The system call name
 * @param {string} path The path
 * @returns {Error}
 */
function createELOOP(syscall, path) {
  const err = new UVException({
    errno: UV_ELOOP,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createELOOP);
  return err;
}

/**
 * Creates an EACCES error for permission denied.
 * @param {string} syscall The system call name
 * @param {string} path The path
 * @returns {Error}
 */
function createEACCES(syscall, path) {
  const err = new UVException({
    errno: UV_EACCES,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createEACCES);
  return err;
}

function createEXDEV(syscall, path) {
  const err = new UVException({
    errno: UV_EXDEV,
    syscall,
    path,
  });
  ErrorCaptureStackTrace(err, createEXDEV);
  return err;
}

return {
  createENOENT,
  createENOTDIR,
  createENOTEMPTY,
  createEISDIR,
  createEBADF,
  createEEXIST,
  createEROFS,
  createEINVAL,
  createELOOP,
  createEACCES,
  createEXDEV,
};
})();
