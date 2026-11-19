// Copyright 2018-2026 the Deno authors. MIT license.
//
// Kernel-sandbox escape corpus: the native half. One source, built three ways
// by corpus.ts:
//
//   probe            an executable: `probe <name> <cfg>` prints "<state> <detail>"
//                    (the --allow-run child vector), `probe serve <dir>` runs
//                    the listeners the network probes aim at (started by the
//                    unconfined orchestrator, outside the confined tree).
//   libprobe.so/dylib  corpus_probe(name, cfg, out, len) for Deno.dlopen (FFI).
//   probe.node       a Node-API addon exporting probe(name, cfg) (the addon and
//                    native-npm-package vectors).
//
// Each probe tries ONE way out of the grant corpus.ts runs the host under and
// classifies the result:
//   open  the operation reached its target (no layer refused it)
//   held  refused with EPERM / EACCES (what a sandbox layer returns)
//   host  refused by a host policy that is not ours (Yama, a sysctl, missing
//         capabilities); the detail names it
//   n/a   not applicable here (platform, missing target, unsupported kernel)
// Every network probe treats "permitted but unanswered" (timeout, unreachable,
// refused) as open: the syscall was allowed, which is what the matrix is about.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <signal.h>
#include <spawn.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <arpa/inet.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <sys/xattr.h>
#include <unistd.h>

#ifdef __linux__
#include <sched.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <linux/netlink.h>
#else
#include <mach/mach.h>
#include <servers/bootstrap.h>
#endif

extern char **environ;

#ifndef SOCK_CLOEXEC
#define SOCK_CLOEXEC 0  // macOS: no SOCK_CLOEXEC
#endif

#define EXPORT __attribute__((visibility("default")))

enum { OPEN, HELD, HOST, NA };
static const char *STATE[] = {"open", "held", "host", "n/a"};

typedef struct {
  char *out;
  int len;
} res_t;

static int put(res_t *r, int state, const char *fmt, ...) {
  int n = snprintf(r->out, r->len, "%s ", STATE[state]);
  va_list ap;
  va_start(ap, fmt);
  if (n < r->len) vsnprintf(r->out + n, r->len - n, fmt, ap);
  va_end(ap);
  return state;
}

// errno -> state for a refused operation.
static int refused(res_t *r, const char *what, int e) {
  if (e == EPERM || e == EACCES) return put(r, HELD, "%s: %s", what, strerror(e));
  return put(r, OPEN, "%s: permitted (%s)", what, strerror(e));
}

// cfg is "k=v;k=v;..."
static const char *cfg_get(const char *cfg, const char *key, char *buf, size_t n) {
  size_t kl = strlen(key);
  const char *p = cfg;
  while (p && *p) {
    if (strncmp(p, key, kl) == 0 && p[kl] == '=') {
      const char *v = p + kl + 1, *e = strchr(v, ';');
      size_t l = e ? (size_t)(e - v) : strlen(v);
      if (l >= n) l = n - 1;
      memcpy(buf, v, l);
      buf[l] = 0;
      return buf;
    }
    p = strchr(p, ';');
    if (p) p++;
  }
  buf[0] = 0;
  return NULL;
}

static int cfg_int(const char *cfg, const char *key) {
  char b[64];
  return cfg_get(cfg, key, b, sizeof b) ? atoi(b) : 0;
}

static void path_of(const char *cfg, const char *dirkey, const char *name, char *out, size_t n) {
  char d[1024];
  cfg_get(cfg, dirkey, d, sizeof d);
  snprintf(out, n, "%s/%s", d, name);
}

static int read_file(const char *p, char *buf, size_t n) {
  int fd = open(p, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return -1;
  ssize_t k = read(fd, buf, n - 1);
  int e = errno;
  close(fd);
  if (k < 0) { errno = e; return -1; }
  buf[k] = 0;
  return (int)k;
}

static int read_sysctl_int(const char *p) {
  char b[32];
  return read_file(p, b, sizeof b) < 0 ? -1 : atoi(b);
}

// ---- files -----------------------------------------------------------------

static int fs_read(const char *cfg, res_t *r) {
  char p[1100], b[64];
  path_of(cfg, "out", "secret", p, sizeof p);
  if (read_file(p, b, sizeof b) < 0) return refused(r, "open outside/secret", errno);
  return put(r, OPEN, "read %d bytes outside the read scope", (int)strlen(b));
}

static int fs_create(const char *cfg, const char *vec, res_t *r) {
  char p[1100], n[64];
  snprintf(n, sizeof n, "created-%s", vec);
  path_of(cfg, "out", n, p, sizeof p);
  int fd = open(p, O_CREAT | O_WRONLY | O_TRUNC | O_CLOEXEC, 0600);
  if (fd < 0) return refused(r, "create outside/", errno);
  if (write(fd, "x", 1) < 0) {}
  close(fd);
  return put(r, OPEN, "created a file outside the write scope");
}

static int fs_truncate(const char *cfg, const char *vec, res_t *r) {
  char p[1100], n[64];
  snprintf(n, sizeof n, "victim-%s", vec);
  path_of(cfg, "out", n, p, sizeof p);
  int fd = open(p, O_WRONLY | O_TRUNC | O_CLOEXEC);
  if (fd < 0) return refused(r, "truncate outside/victim", errno);
  close(fd);
  return put(r, OPEN, "truncated a file outside the write scope");
}

static int fs_unlink(const char *cfg, const char *vec, res_t *r) {
  char p[1100], n[64];
  snprintf(n, sizeof n, "unlink-%s", vec);
  path_of(cfg, "out", n, p, sizeof p);
  if (unlink(p) != 0) return refused(r, "unlink outside/", errno);
  return put(r, OPEN, "removed a file outside the write scope");
}

static int fs_rename(const char *cfg, const char *vec, res_t *r) {
  char a[1100], b[1100], n[64], m[64];
  snprintf(n, sizeof n, "ren-%s", vec);
  snprintf(m, sizeof m, "ren-%s.moved", vec);
  path_of(cfg, "out", n, a, sizeof a);
  path_of(cfg, "out", m, b, sizeof b);
  if (rename(a, b) != 0) return refused(r, "rename inside outside/", errno);
  return put(r, OPEN, "renamed a file outside the write scope");
}

static int fs_chmod(const char *cfg, res_t *r) {
  char p[1100];
  path_of(cfg, "out", "secret", p, sizeof p);
  if (chmod(p, 0640) != 0) return refused(r, "chmod outside/secret", errno);
  return put(r, OPEN, "changed the mode of a file outside the write scope");
}

static int fs_utime(const char *cfg, res_t *r) {
  char p[1100];
  path_of(cfg, "out", "secret", p, sizeof p);
  if (utimes(p, NULL) != 0) return refused(r, "utimes outside/secret", errno);
  return put(r, OPEN, "changed the times of a file outside the write scope");
}

static int fs_xattr(const char *cfg, res_t *r) {
  char p[1100];
  path_of(cfg, "out", "secret", p, sizeof p);
#ifdef __linux__
  int rc = setxattr(p, "user.corpus", "1", 1, 0);
#else
  int rc = setxattr(p, "com.denext.corpus", "1", 1, 0, 0);
#endif
  if (rc != 0) {
    if (errno == ENOTSUP) return put(r, NA, "no user xattrs on this filesystem");
    return refused(r, "setxattr outside/secret", errno);
  }
  return put(r, OPEN, "set an xattr on a file outside the write scope");
}

static int fs_hardlink(const char *cfg, const char *vec, res_t *r) {
  char src[1100], dst[1100], n[64], b[64];
  path_of(cfg, "out", "secret", src, sizeof src);
  snprintf(n, sizeof n, "hl-%s", vec);
  path_of(cfg, "work", n, dst, sizeof dst);
  unlink(dst);
  if (link(src, dst) != 0) {
    // EXDEV is how Landlock refuses a link that would gain rights (its REFER
    // answer); outside and work share a file system, so the baseline (no
    // sandbox) links fine and classify.ts tells the two apart
    if (errno == EXDEV) return put(r, HELD, "link outside/secret into work/: EXDEV (refused)");
    return refused(r, "link outside/secret into work/", errno);
  }
  if (read_file(dst, b, sizeof b) < 0) return refused(r, "read through the new hard link", errno);
  return put(r, OPEN, "linked a file from outside into the scope and read it");
}

static int fs_symlink(const char *cfg, const char *vec, res_t *r) {
  char src[1100], dst[1100], n[64], b[64];
  path_of(cfg, "out", "secret", src, sizeof src);
  snprintf(n, sizeof n, "sl-%s", vec);
  path_of(cfg, "work", n, dst, sizeof dst);
  unlink(dst);
  if (symlink(src, dst) != 0) return refused(r, "symlink in work/", errno);
  if (read_file(dst, b, sizeof b) < 0) return refused(r, "open work/symlink -> outside", errno);
  return put(r, OPEN, "read outside through a symlink inside the scope");
}

static int fs_rename_into(const char *cfg, const char *vec, res_t *r) {
  char a[1100], b[1100], n[64], buf[64];
  snprintf(n, sizeof n, "mv-%s", vec);
  path_of(cfg, "out", n, a, sizeof a);
  path_of(cfg, "work", n, b, sizeof b);
  if (rename(a, b) != 0) {
    // EXDEV: Landlock's refusal of a cross-directory rename (every one
    // before ABI 2; one that would gain rights after); see fs_hardlink
    if (errno == EXDEV) return put(r, HELD, "rename outside/ -> work/: EXDEV (refused)");
    return refused(r, "rename outside/ -> work/", errno);
  }
  if (read_file(b, buf, sizeof buf) < 0) return refused(r, "read the moved file", errno);
  return put(r, OPEN, "moved a file from outside into the scope and read it");
}

// A link inside the scope flips between a benign file and outside/secret while
// we open it: any read that returns the secret is a race win.
static int fs_race(const char *cfg, const char *vec, res_t *r) {
  char lnk[1100], tmp[1100], benign[1100], secret[1100], n[64], t[64];
  snprintf(n, sizeof n, "race-%s", vec);
  snprintf(t, sizeof t, "race-%s.tmp", vec);
  path_of(cfg, "work", n, lnk, sizeof lnk);
  path_of(cfg, "work", t, tmp, sizeof tmp);
  path_of(cfg, "work", "benign", benign, sizeof benign);
  path_of(cfg, "out", "secret", secret, sizeof secret);
  unlink(lnk);
  if (symlink(benign, lnk) != 0) return refused(r, "symlink in work/", errno);
  pid_t pid = fork();
  if (pid == 0) {
    for (int i = 0; i < 4000; i++) {
      unlink(tmp);
      if (symlink((i & 1) ? secret : benign, tmp) == 0) rename(tmp, lnk);
    }
    _exit(0);
  }
  // The opens go on for as long as the swapper runs, then a few more: on a
  // loaded machine the forked swapper may not be scheduled before a fixed
  // number of opens is over, and then none of them met the secret side. The
  // link's last state is the secret, so the opens after the swapper ended
  // always meet it.
  int wins = 0, refusals = 0, last = 0, tries = 0, swapping = 1, after = 0;
  char b[64];
  while (tries < 4000000 && (swapping || after < 16)) {
    if (read_file(lnk, b, sizeof b) >= 0) {
      if (strncmp(b, "TOP-SECRET", 10) == 0) wins++;
    } else if (errno == EPERM || errno == EACCES) {
      refusals++;
      last = errno;
    }
    tries++;
    if (!swapping) after++;
    else if (waitpid(pid, NULL, WNOHANG) == pid) swapping = 0;
  }
  if (swapping) waitpid(pid, NULL, 0);
  if (wins) return put(r, OPEN, "%d of %d opens through the swapped link read the secret", wins, tries);
  if (refusals) return put(r, HELD, "every secret-side open refused (%s, %d times)", strerror(last), refusals);
  return put(r, OPEN, "no race win in %d tries (not a refusal; rerun)", tries);
}

static int fd_inherited(const char *cfg, res_t *r) {
  (void)cfg;
  char b[64];
  struct stat st;
  if (fstat(3, &st) != 0) return put(r, NA, "no fd 3 in this process (not inherited)");
  ssize_t k = pread(3, b, sizeof b - 1, 0);
  if (k < 0) return refused(r, "read inherited fd 3", errno);
  b[k] = 0;
  return put(r, OPEN, "read %zd bytes through fd 3 opened before the run (by design)", k);
}

static int proc_fd_reopen(const char *cfg, res_t *r) {
  (void)cfg;
#ifdef __linux__
  struct stat st;
  if (fstat(3, &st) != 0) return put(r, NA, "no inherited fd 3");
  int fd = open("/proc/self/fd/3", O_WRONLY | O_APPEND | O_CLOEXEC);
  if (fd < 0) return refused(r, "reopen /proc/self/fd/3 for writing", errno);
  close(fd);
  return put(r, OPEN, "upgraded a read-only inherited fd to write via /proc/self/fd");
#else
  return put(r, NA, "linux /proc only");
#endif
}

static int proc_self_root(const char *cfg, res_t *r) {
#ifdef __linux__
  char p[1100], q[1200], b[64];
  path_of(cfg, "out", "secret", p, sizeof p);
  snprintf(q, sizeof q, "/proc/self/root%s", p);
  if (read_file(q, b, sizeof b) < 0) return refused(r, "open /proc/self/root/<outside>", errno);
  return put(r, OPEN, "read outside through /proc/self/root");
#else
  (void)cfg;
  return put(r, NA, "linux /proc only");
#endif
}

// ---- processes ---------------------------------------------------------------

__attribute__((unused)) static int yama(void) { return read_sysctl_int("/proc/sys/kernel/yama/ptrace_scope"); }

static int proc_environ(const char *cfg, res_t *r) {
#ifdef __linux__
  char p[64], b[256];
  snprintf(p, sizeof p, "/proc/%d/environ", cfg_int(cfg, "srvpid"));
  if (read_file(p, b, sizeof b) < 0) return refused(r, "read /proc/<outside pid>/environ", errno);
  return put(r, OPEN, "read another process's environment");
#else
  (void)cfg;
  return put(r, NA, "linux /proc only");
#endif
}

static int proc_mem(const char *cfg, res_t *r) {
#ifdef __linux__
  char p[64];
  snprintf(p, sizeof p, "/proc/%d/mem", cfg_int(cfg, "srvpid"));
  int fd = open(p, O_RDONLY | O_CLOEXEC);
  if (fd < 0) {
    int e = errno, y = yama();
    if ((e == EPERM || e == EACCES) && y >= 1) return put(r, HOST, "open %s: %s (Yama ptrace_scope=%d)", p, strerror(e), y);
    return refused(r, "open /proc/<outside pid>/mem", e);
  }
  close(fd);
  return put(r, OPEN, "opened another process's memory");
#else
  (void)cfg;
  return put(r, NA, "linux /proc only");
#endif
}

static int proc_ptrace(const char *cfg, res_t *r) {
  int pid = cfg_int(cfg, "srvpid");
#ifdef __linux__
  if (ptrace(PTRACE_ATTACH, pid, 0, 0) != 0) {
    int e = errno, y = yama();
    if ((e == EPERM || e == EACCES) && y >= 1) return put(r, HOST, "PTRACE_ATTACH: %s (Yama ptrace_scope=%d)", strerror(e), y);
    return refused(r, "PTRACE_ATTACH outside pid", e);
  }
  waitpid(pid, NULL, __WALL);
  ptrace(PTRACE_DETACH, pid, 0, 0);
  return put(r, OPEN, "attached to a process outside the tree");
#else
  mach_port_t task;
  kern_return_t k = task_for_pid(mach_task_self(), pid, &task);
  if (k != KERN_SUCCESS) return put(r, HOST, "task_for_pid: kern_return %d (needs the debugger entitlement)", k);
  return put(r, OPEN, "got the task port of a process outside the tree");
#endif
}

static int proc_signal(const char *cfg, res_t *r) {
  if (kill(cfg_int(cfg, "srvpid"), 0) != 0) return refused(r, "kill(outside pid, 0)", errno);
  return put(r, OPEN, "may signal a process outside the tree");
}

static int exec_unlisted(const char *cfg, res_t *r) {
  (void)cfg;
  pid_t pid;
  char *argv[] = {"/bin/sh", "-c", "exit 7", NULL};
  int e = posix_spawn(&pid, "/bin/sh", NULL, NULL, argv, environ);
  if (e != 0) return refused(r, "posix_spawn /bin/sh", e);
  int st = 0;
  waitpid(pid, &st, 0);
  if (WIFEXITED(st) && WEXITSTATUS(st) == 7) return put(r, OPEN, "ran /bin/sh, a program outside --allow-run");
  if (WIFEXITED(st) && WEXITSTATUS(st) == 127) return put(r, HELD, "spawned but exec failed (127)");
  return put(r, OPEN, "spawned /bin/sh (status 0x%x)", st);
}

static int env_read(const char *cfg, res_t *r) {
  (void)cfg;
  const char *v = getenv("CORPUS_SECRET_ENV");
  if (!v) return put(r, HELD, "CORPUS_SECRET_ENV not in this process's environment");
  return put(r, OPEN, "read an environment variable --allow-env does not grant");
}

static int sys_info(const char *cfg, res_t *r) {
  (void)cfg;
  struct utsname u;
  if (uname(&u) != 0) return refused(r, "uname", errno);
#ifdef __linux__
  char b[64];
  if (read_file("/proc/loadavg", b, sizeof b) < 0) return refused(r, "read /proc/loadavg", errno);
#endif
  return put(r, OPEN, "read hostname and load average (sys is Deno-only)");
}

// ---- kernel surface (Linux) ------------------------------------------------------

__attribute__((unused)) static int run_in_child(int (*fn)(void)) {
  pid_t pid = fork();
  if (pid < 0) return -errno;
  if (pid == 0) _exit(fn());
  int st = 0;
  waitpid(pid, &st, 0);
  return WIFEXITED(st) ? WEXITSTATUS(st) : 255;
}

#ifdef __linux__
static int do_userns(void) { return unshare(CLONE_NEWUSER) == 0 ? 0 : errno; }
static int do_mount(void) {
  if (unshare(CLONE_NEWUSER | CLONE_NEWNS) != 0) return 200;
  return mount("none", "/tmp", "tmpfs", 0, NULL) == 0 ? 0 : errno;
}
#endif

__attribute__((unused)) static int userns_host_note(char *b, size_t n) {
  int a = read_sysctl_int("/proc/sys/kernel/apparmor_restrict_unprivileged_userns");
  int c = read_sysctl_int("/proc/sys/kernel/unprivileged_userns_clone");
  int m = read_sysctl_int("/proc/sys/user/max_user_namespaces");
  snprintf(b, n, "apparmor_restrict_unprivileged_userns=%d unprivileged_userns_clone=%d max_user_namespaces=%d", a, c, m);
  return a == 1 || c == 0 || m == 0;
}

static int sys_userns(const char *cfg, res_t *r) {
  (void)cfg;
#ifdef __linux__
  int rc = run_in_child(do_userns);
  char note[200];
  int hostblock = userns_host_note(note, sizeof note);
  if (rc == 0) return put(r, OPEN, "created a user namespace");
  if ((rc == EPERM || rc == EACCES || rc == ENOSPC) && hostblock) return put(r, HOST, "unshare(CLONE_NEWUSER): %s (%s)", strerror(rc), note);
  return refused(r, "unshare(CLONE_NEWUSER)", rc);
#else
  return put(r, NA, "linux only");
#endif
}

static int sys_mount(const char *cfg, res_t *r) {
  (void)cfg;
#ifdef __linux__
  int rc = run_in_child(do_mount);
  char note[200];
  int hostblock = userns_host_note(note, sizeof note);
  if (rc == 0) return put(r, OPEN, "mounted a tmpfs in a new user+mount namespace");
  if (rc == 200) return put(r, hostblock ? HOST : HELD, "no user namespace for the mount (%s)", note);
  if ((rc == EPERM || rc == EACCES) && hostblock) return put(r, HOST, "mount: %s (%s)", strerror(rc), note);
  return refused(r, "mount tmpfs in a userns", rc);
#else
  return put(r, NA, "linux only");
#endif
}

static int sys_io_uring(const char *cfg, res_t *r) {
  (void)cfg;
#ifdef __linux__
  char params[120];
  memset(params, 0, sizeof params);
  long fd = syscall(425 /* io_uring_setup */, 1, params);
  if (fd >= 0) {
    close((int)fd);
    return put(r, OPEN, "io_uring_setup succeeded");
  }
  int e = errno, dis = read_sysctl_int("/proc/sys/kernel/io_uring_disabled");
  // ENOSYS: this kernel lacks it, or a filter answers so (the kernel
  // sandbox's choice: callers fall back); the baseline tells them apart
  if (e == ENOSYS) return put(r, HELD, "io_uring_setup: ENOSYS");
  if ((e == EPERM || e == EACCES) && dis > 0) return put(r, HOST, "io_uring_setup: %s (io_uring_disabled=%d)", strerror(e), dis);
  return refused(r, "io_uring_setup", e);
#else
  return put(r, NA, "linux only");
#endif
}

#ifdef __linux__
static int do_clone3(void) {
  uint64_t args[11];
  memset(args, 0, sizeof args);
  args[4] = SIGCHLD;  // exit_signal
  long pid = syscall(435 /* clone3 */, args, 88);
  if (pid == 0) syscall(SYS_exit, 0);
  if (pid < 0) return errno;
  waitpid((pid_t)pid, NULL, 0);
  return 0;
}
#endif

static int sys_clone3(const char *cfg, res_t *r) {
  (void)cfg;
#ifdef __linux__
  int rc = run_in_child(do_clone3);
  if (rc == 0) return put(r, OPEN, "clone3 succeeded (its flags are beyond seccomp's reach)");
  // ENOSYS: no clone3 in this kernel, or a filter answers so (glibc falls
  // back to clone, whose flags seccomp can see); the baseline tells them
  // apart
  if (rc == ENOSYS) return put(r, HELD, "clone3: ENOSYS (glibc falls back to clone)");
  return refused(r, "clone3", rc);
#else
  return put(r, NA, "linux only");
#endif
}

static int sys_keyctl(const char *cfg, res_t *r) {
  (void)cfg;
#ifdef __linux__
  long id = syscall(SYS_keyctl, 0 /* KEYCTL_GET_KEYRING_ID */, -3 /* SESSION */, 0);
  if (id < 0) {
    if (errno == ENOSYS) return put(r, NA, "no keyrings (ENOSYS)");
    return refused(r, "keyctl(GET_KEYRING_ID, session)", errno);
  }
  return put(r, OPEN, "reached the session keyring (id %ld)", id);
#else
  return put(r, NA, "linux only");
#endif
}

static int sys_bpf(const char *cfg, res_t *r) {
  (void)cfg;
#ifdef __linux__
  uint32_t attr[30];
  memset(attr, 0, sizeof attr);
  attr[0] = 2;  // BPF_MAP_TYPE_ARRAY
  attr[1] = 4;
  attr[2] = 4;
  attr[3] = 1;
  long fd = syscall(SYS_bpf, 0 /* BPF_MAP_CREATE */, attr, sizeof attr);
  if (fd >= 0) {
    close((int)fd);
    return put(r, OPEN, "created a BPF map");
  }
  int e = errno, u = read_sysctl_int("/proc/sys/kernel/unprivileged_bpf_disabled");
  if ((e == EPERM || e == EACCES) && u > 0) return put(r, HOST, "bpf: %s (unprivileged_bpf_disabled=%d)", strerror(e), u);
  return refused(r, "bpf(MAP_CREATE)", e);
#else
  return put(r, NA, "linux only");
#endif
}

static int raw_open(const char *cfg, res_t *r) {
#ifdef __linux__
  char p[1100], b[64];
  path_of(cfg, "out", "secret", p, sizeof p);
  long fd = syscall(SYS_openat, AT_FDCWD, p, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return refused(r, "raw openat(outside/secret)", errno);
  long k = syscall(SYS_read, fd, b, sizeof b);
  syscall(SYS_close, fd);
  return put(r, OPEN, "raw openat+read outside the read scope (%ld bytes)", k);
#else
  (void)cfg;
  return put(r, NA, "linux raw syscalls only");
#endif
}

// ---- network -------------------------------------------------------------------

// A connect that waits at most ms; returns 0 connected, else errno.
static int connect_to(int fd, const struct sockaddr *sa, socklen_t sl, int ms) {
  int fl = fcntl(fd, F_GETFL);
  fcntl(fd, F_SETFL, fl | O_NONBLOCK);
  if (connect(fd, sa, sl) == 0) return 0;
  if (errno != EINPROGRESS) return errno;
  struct pollfd p = {fd, POLLOUT, 0};
  if (poll(&p, 1, ms) <= 0) return ETIMEDOUT;
  int err = 0;
  socklen_t l = sizeof err;
  getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &l);
  return err;
}

static int tcp4(const char *what, const char *ip, int port, res_t *r, int ms) {
  int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return refused(r, "socket(AF_INET)", errno);
  struct sockaddr_in a = {0};
  a.sin_family = AF_INET;
  a.sin_port = htons((uint16_t)port);
  inet_pton(AF_INET, ip, &a.sin_addr);
  int e = connect_to(fd, (struct sockaddr *)&a, sizeof a, ms);
  close(fd);
  if (e == 0) return put(r, OPEN, "%s: connected to %s:%d", what, ip, port);
  return refused(r, what, e);
}

static int net_tcp4(const char *cfg, res_t *r) {
  return tcp4("connect 127.0.0.1:<ungranted port>", "127.0.0.1", cfg_int(cfg, "tcp"), r, 2000);
}

static int net_internal_on_granted_port(const char *cfg, res_t *r) {
  return tcp4("connect 127.0.0.1 on a port granted only to a public name", "127.0.0.1",
              cfg_int(cfg, "tcpg"), r, 2000);
}

static int net_metadata(const char *cfg, res_t *r) {
  (void)cfg;
  return tcp4("connect 169.254.169.254:80 (cloud metadata)", "169.254.169.254", 80, r, 300);
}

static int net_tcp6(const char *cfg, res_t *r) {
  int port = cfg_int(cfg, "tcp6");
  int fd = socket(AF_INET6, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) {
    if (errno == EAFNOSUPPORT) return put(r, NA, "no IPv6 in this kernel");
    return refused(r, "socket(AF_INET6)", errno);
  }
  if (!port) {
    close(fd);
    return put(r, OPEN, "socket(AF_INET6) permitted (no ::1 listener on this host to connect to)");
  }
  struct sockaddr_in6 a = {0};
  a.sin6_family = AF_INET6;
  a.sin6_port = htons((uint16_t)port);
  a.sin6_addr = in6addr_loopback;
  int e = connect_to(fd, (struct sockaddr *)&a, sizeof a, 2000);
  close(fd);
  if (e == 0) return put(r, OPEN, "connected to [::1]:%d", port);
  return refused(r, "connect [::1]", e);
}

static int net_bind(const char *cfg, res_t *r) {
  (void)cfg;
  int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return refused(r, "socket(AF_INET)", errno);
  struct sockaddr_in a = {0};
  a.sin_family = AF_INET;
  a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  if (bind(fd, (struct sockaddr *)&a, sizeof a) != 0) {
    int e = errno;
    close(fd);
    return refused(r, "bind 127.0.0.1:0", e);
  }
  if (listen(fd, 1) != 0) {
    int e = errno;
    close(fd);
    return refused(r, "listen after bind", e);
  }
  close(fd);
  return put(r, OPEN, "bound and listened on a TCP port no grant names");
}

static int udp_exchange(const char *cfg, const unsigned char *q, size_t ql, unsigned char *ans, size_t al, res_t *r,
                        const char *what) {
  int fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return -refused(r, "socket(AF_INET, SOCK_DGRAM)", errno) - 1;
  struct sockaddr_in a = {0};
  a.sin_family = AF_INET;
  a.sin_port = htons((uint16_t)cfg_int(cfg, "udp"));
  inet_pton(AF_INET, "127.0.0.1", &a.sin_addr);
  if (sendto(fd, q, ql, 0, (struct sockaddr *)&a, sizeof a) < 0) {
    int e = errno;
    close(fd);
    return -refused(r, what, e) - 1;
  }
  struct pollfd p = {fd, POLLIN, 0};
  ssize_t k = -1;
  if (poll(&p, 1, 2000) > 0) k = recv(fd, ans, al, 0);
  close(fd);
  return (int)k;
}

static int net_udp(const char *cfg, res_t *r) {
  unsigned char ans[64];
  int k = udp_exchange(cfg, (const unsigned char *)"ping", 4, ans, sizeof ans, r, "sendto udp");
  if (k < -1) return -k - 1;
  if (k > 0) return put(r, OPEN, "UDP datagram to an ungranted port answered");
  return put(r, OPEN, "sendto permitted (no answer)");
}

static int net_dns(const char *cfg, res_t *r) {
  // A query for corpus.test A, to the orchestrator's fake resolver.
  static const unsigned char q[] = {0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 6, 'c', 'o', 'r', 'p', 'u', 's',
                                    4, 't', 'e', 's', 't', 0, 0, 1, 0, 1};
  unsigned char ans[512];
  int k = udp_exchange(cfg, q, sizeof q, ans, sizeof ans, r, "send DNS query");
  if (k < -1) return -k - 1;
  if (k >= 16 && ans[0] == 0x12 && ans[1] == 0x34 && (ans[2] & 0x80))
    return put(r, OPEN, "resolved corpus.test through a resolver no grant names (answer %d.%d.%d.%d)", ans[k - 4],
               ans[k - 3], ans[k - 2], ans[k - 1]);
  return put(r, OPEN, "DNS query sent (no answer)");
}

static int net_fastopen(const char *cfg, res_t *r) {
#ifdef __linux__
  int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return refused(r, "socket(AF_INET)", errno);
  struct sockaddr_in a = {0};
  a.sin_family = AF_INET;
  a.sin_port = htons((uint16_t)cfg_int(cfg, "tcp"));
  inet_pton(AF_INET, "127.0.0.1", &a.sin_addr);
  ssize_t k = sendto(fd, "x", 1, MSG_FASTOPEN, (struct sockaddr *)&a, sizeof a);
  int e = errno;
  close(fd);
  if (k >= 0) return put(r, OPEN, "sendto(MSG_FASTOPEN) connected and sent");
  return refused(r, "sendto(MSG_FASTOPEN)", e);
#else
  (void)cfg;
  return put(r, NA, "linux MSG_FASTOPEN only");
#endif
}

static int unix_connect(const char *path, int abstract, res_t *r, const char *what) {
  int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return refused(r, "socket(AF_UNIX)", errno);
  struct sockaddr_un a;
  memset(&a, 0, sizeof a);
  a.sun_family = AF_UNIX;
  socklen_t l;
  if (abstract) {
    strncpy(a.sun_path + 1, path, sizeof a.sun_path - 2);
    l = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + 1 + strlen(path));
  } else {
    strncpy(a.sun_path, path, sizeof a.sun_path - 1);
    l = sizeof a;
  }
  int e = connect(fd, (struct sockaddr *)&a, l) == 0 ? 0 : errno;
  close(fd);
  if (e == 0) return put(r, OPEN, "%s: connected", what);
  if (e == ENOENT || e == ECONNREFUSED) return put(r, OPEN, "%s: permitted, nobody listening (%s)", what, strerror(e));
  return refused(r, what, e);
}

static int net_unix_path(const char *cfg, res_t *r) {
  char p[1100];
  path_of(cfg, "out", "srv.sock", p, sizeof p);
  return unix_connect(p, 0, r, "connect outside/srv.sock");
}

static int net_unix_abstract(const char *cfg, res_t *r) {
#ifdef __linux__
  char n[128];
  cfg_get(cfg, "abs", n, sizeof n);
  return unix_connect(n, 1, r, "connect @abstract socket");
#else
  (void)cfg;
  return put(r, NA, "abstract sockets are Linux-only");
#endif
}

static int net_netlink(int proto, const char *what, res_t *r) {
#ifdef __linux__
  int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, proto);
  if (fd < 0) return refused(r, what, errno);
  struct sockaddr_nl a = {0};
  a.nl_family = AF_NETLINK;
  int e = bind(fd, (struct sockaddr *)&a, sizeof a) == 0 ? 0 : errno;
  close(fd);
  if (e) return refused(r, what, e);
  return put(r, OPEN, "%s: opened and bound", what);
#else
  (void)proto;
  return put(r, NA, "%s: linux only", what);
#endif
}

static int net_netlink_route(const char *cfg, res_t *r) {
  (void)cfg;
  return net_netlink(0 /* NETLINK_ROUTE */, "netlink route", r);
}

static int net_netlink_other(const char *cfg, res_t *r) {
  (void)cfg;
  return net_netlink(15 /* NETLINK_KOBJECT_UEVENT */, "netlink kobject-uevent", r);
}

static int ipc_session_bus(const char *cfg, res_t *r) {
  (void)cfg;
  char p[512] = "";
  const char *addr = getenv("DBUS_SESSION_BUS_ADDRESS");
  if (addr && strncmp(addr, "unix:path=", 10) == 0) {
    snprintf(p, sizeof p, "%s", addr + 10);
    char *c = strchr(p, ',');
    if (c) *c = 0;
  } else {
    snprintf(p, sizeof p, "/run/user/%d/bus", (int)getuid());
  }
  struct stat st;
  if (stat(p, &st) != 0) return put(r, NA, "no session bus socket here");
  return unix_connect(p, 0, r, "connect the session bus");
}

static int ipc_docker(const char *cfg, res_t *r) {
  (void)cfg;
  struct stat st;
  if (stat("/var/run/docker.sock", &st) != 0) return put(r, NA, "no /var/run/docker.sock here");
  return unix_connect("/var/run/docker.sock", 0, r, "connect /var/run/docker.sock");
}

static int mach_lookup(const char *svc, res_t *r) {
#ifdef __APPLE__
  mach_port_t port = MACH_PORT_NULL;
  kern_return_t k = bootstrap_look_up(bootstrap_port, svc, &port);
  if (k == KERN_SUCCESS) {
    mach_port_deallocate(mach_task_self(), port);
    return put(r, OPEN, "looked up %s (can ask it to start programs outside the sandbox)", svc);
  }
  if (k == 1100 /* BOOTSTRAP_NOT_PRIVILEGED */ || k == 1102 /* UNKNOWN_SERVICE */)
    return put(r, HELD, "bootstrap_look_up(%s): %d", svc, k);
  return put(r, HELD, "bootstrap_look_up(%s): kern_return %d", svc, k);
#else
  return put(r, NA, "%s: macOS only", svc);
#endif
}

static int ipc_launchservices(const char *cfg, res_t *r) {
  (void)cfg;
  return mach_lookup("com.apple.coreservices.launchservicesd", r);
}

static int ipc_appleevents(const char *cfg, res_t *r) {
  (void)cfg;
  return mach_lookup("com.apple.coreservices.appleevents", r);
}

// ---- dispatch ------------------------------------------------------------------

typedef int (*probe_fn)(const char *cfg, res_t *r);
typedef int (*probe_vec_fn)(const char *cfg, const char *vec, res_t *r);
static const struct {
  const char *name;
  probe_fn fn;
  probe_vec_fn vfn;
} PROBES[] = {
    {"fs.read", fs_read, NULL},
    {"fs.raw_openat", raw_open, NULL},
    {"fs.create", NULL, fs_create},
    {"fs.truncate", NULL, fs_truncate},
    {"fs.unlink", NULL, fs_unlink},
    {"fs.rename", NULL, fs_rename},
    {"fs.chmod", fs_chmod, NULL},
    {"fs.utimes", fs_utime, NULL},
    {"fs.setxattr", fs_xattr, NULL},
    {"fs.hardlink_in", NULL, fs_hardlink},
    {"fs.symlink_out", NULL, fs_symlink},
    {"fs.rename_in", NULL, fs_rename_into},
    {"fs.symlink_race", NULL, fs_race},
    {"fs.proc_self_root", proc_self_root, NULL},
    {"fd.inherited_read", fd_inherited, NULL},
    {"fd.proc_reopen_write", proc_fd_reopen, NULL},
    {"proc.environ", proc_environ, NULL},
    {"proc.mem", proc_mem, NULL},
    {"proc.ptrace", proc_ptrace, NULL},
    {"proc.signal", proc_signal, NULL},
    {"exec.unlisted", exec_unlisted, NULL},
    {"env.read", env_read, NULL},
    {"sys.info", sys_info, NULL},
    {"sys.userns", sys_userns, NULL},
    {"sys.mount", sys_mount, NULL},
    {"sys.io_uring", sys_io_uring, NULL},
    {"sys.clone3", sys_clone3, NULL},
    {"sys.keyctl", sys_keyctl, NULL},
    {"sys.bpf", sys_bpf, NULL},
    {"net.tcp4", net_tcp4, NULL},
    {"net.tcp6", net_tcp6, NULL},
    {"net.raw_connect", NULL, NULL},
    {"net.internal_granted_port", net_internal_on_granted_port, NULL},
    {"net.metadata", net_metadata, NULL},
    {"net.bind", net_bind, NULL},
    {"net.udp", net_udp, NULL},
    {"net.dns", net_dns, NULL},
    {"net.fastopen", net_fastopen, NULL},
    {"net.unix_path", net_unix_path, NULL},
    {"net.unix_abstract", net_unix_abstract, NULL},
    {"net.netlink_route", net_netlink_route, NULL},
    {"net.netlink_other", net_netlink_other, NULL},
    {"ipc.session_bus", ipc_session_bus, NULL},
    {"ipc.docker_sock", ipc_docker, NULL},
    {"ipc.launchservices", ipc_launchservices, NULL},
    {"ipc.appleevents", ipc_appleevents, NULL},
};

static int raw_connect(const char *cfg, res_t *r) {
#ifdef __linux__
  long fd = syscall(SYS_socket, AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return refused(r, "raw socket(AF_INET)", errno);
  struct sockaddr_in a = {0};
  a.sin_family = AF_INET;
  a.sin_port = htons((uint16_t)cfg_int(cfg, "tcp"));
  inet_pton(AF_INET, "127.0.0.1", &a.sin_addr);
  long rc = syscall(SYS_connect, fd, &a, sizeof a);
  int e = errno;
  syscall(SYS_close, fd);
  if (rc == 0) return put(r, OPEN, "raw socket+connect to an ungranted port");
  return refused(r, "raw connect", e);
#else
  (void)cfg;
  return put(r, NA, "linux raw syscalls only");
#endif
}

EXPORT int corpus_probe(const char *name, const char *cfg, char *out, int len) {
  res_t r = {out, len};
  char vec[32];
  cfg_get(cfg, "vec", vec, sizeof vec);
  if (strcmp(name, "net.raw_connect") == 0) return raw_connect(cfg, &r);
  for (size_t i = 0; i < sizeof PROBES / sizeof PROBES[0]; i++) {
    if (strcmp(PROBES[i].name, name) == 0) {
      return PROBES[i].fn ? PROBES[i].fn(cfg, &r) : PROBES[i].vfn(cfg, vec, &r);
    }
  }
  return put(&r, NA, "unknown probe %s", name);
}

EXPORT const char *corpus_list(void) {
  static char buf[4096];
  buf[0] = 0;
  for (size_t i = 0; i < sizeof PROBES / sizeof PROBES[0]; i++) {
    strcat(buf, PROBES[i].name);
    strcat(buf, "\n");
  }
  return buf;
}

// ---- Node-API addon -------------------------------------------------------------

#ifdef PROBE_NAPI
typedef struct napi_env__ *napi_env;
typedef struct napi_value__ *napi_value;
typedef struct napi_callback_info__ *napi_callback_info;
typedef int napi_status;
typedef napi_value (*napi_callback)(napi_env env, napi_callback_info info);
napi_status napi_create_function(napi_env, const char *, size_t, napi_callback, void *, napi_value *);
napi_status napi_set_named_property(napi_env, napi_value, const char *, napi_value);
napi_status napi_get_cb_info(napi_env, napi_callback_info, size_t *, napi_value *, napi_value *, void **);
napi_status napi_get_value_string_utf8(napi_env, napi_value, char *, size_t, size_t *);
napi_status napi_create_string_utf8(napi_env, const char *, size_t, napi_value *);

static napi_value napi_probe(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL);
  char name[128] = "", cfg[4096] = "", out[1024] = "";
  size_t l;
  if (argc > 0) napi_get_value_string_utf8(env, argv[0], name, sizeof name, &l);
  if (argc > 1) napi_get_value_string_utf8(env, argv[1], cfg, sizeof cfg, &l);
  corpus_probe(name, cfg, out, sizeof out);
  napi_value s;
  napi_create_string_utf8(env, out, (size_t)-1, &s);
  return s;
}

EXPORT napi_value napi_register_module_v1(napi_env env, napi_value exports) {
  napi_value fn;
  napi_create_function(env, "probe", (size_t)-1, napi_probe, NULL, &fn);
  napi_set_named_property(env, exports, "probe", fn);
  return exports;
}
#endif

// ---- executable: child vector and the listeners --------------------------------

#ifdef PROBE_MAIN
static int listen_tcp(int family, int *port) {
  int fd = socket(family, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return -1;
  int one = 1;
  setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
  struct sockaddr_storage ss;
  memset(&ss, 0, sizeof ss);
  socklen_t l;
  if (family == AF_INET) {
    struct sockaddr_in *a = (struct sockaddr_in *)&ss;
    a->sin_family = AF_INET;
    a->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    l = sizeof *a;
  } else {
    struct sockaddr_in6 *a = (struct sockaddr_in6 *)&ss;
    a->sin6_family = AF_INET6;
    a->sin6_addr = in6addr_loopback;
    l = sizeof *a;
  }
  if (bind(fd, (struct sockaddr *)&ss, l) != 0 || listen(fd, 64) != 0) {
    close(fd);
    return -1;
  }
  getsockname(fd, (struct sockaddr *)&ss, &l);
  *port = ntohs(family == AF_INET ? ((struct sockaddr_in *)&ss)->sin_port : ((struct sockaddr_in6 *)&ss)->sin6_port);
  return fd;
}

static int serve(const char *dir) {
  int p1 = 0, p2 = 0, p6 = 0, pu = 0;
  int t1 = listen_tcp(AF_INET, &p1), t2 = listen_tcp(AF_INET, &p2), t6 = listen_tcp(AF_INET6, &p6);
  if (t6 < 0) p6 = 0;
  int u = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
  struct sockaddr_in ua = {0};
  ua.sin_family = AF_INET;
  ua.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  bind(u, (struct sockaddr *)&ua, sizeof ua);
  socklen_t ul = sizeof ua;
  getsockname(u, (struct sockaddr *)&ua, &ul);
  pu = ntohs(ua.sin_port);
  struct sockaddr_un sa;
  memset(&sa, 0, sizeof sa);
  sa.sun_family = AF_UNIX;
  snprintf(sa.sun_path, sizeof sa.sun_path, "%s/srv.sock", dir);
  unlink(sa.sun_path);
  int us = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  bind(us, (struct sockaddr *)&sa, sizeof sa);
  listen(us, 64);
  char absname[64];
  snprintf(absname, sizeof absname, "denext-corpus-%d", (int)getpid());
  int as = -1;
#ifdef __linux__
  struct sockaddr_un aa;
  memset(&aa, 0, sizeof aa);
  aa.sun_family = AF_UNIX;
  strcpy(aa.sun_path + 1, absname);
  as = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (bind(as, (struct sockaddr *)&aa, (socklen_t)(offsetof(struct sockaddr_un, sun_path) + 1 + strlen(absname))) != 0 ||
      listen(as, 64) != 0) {
    close(as);
    as = -1;
  }
#endif
  printf("tcp=%d;tcpg=%d;tcp6=%d;udp=%d;abs=%s;srvpid=%d\n", p1, p2, p6, pu, as >= 0 ? absname : "", (int)getpid());
  fflush(stdout);
  struct pollfd fds[7] = {{0, POLLIN, 0}, {t1, POLLIN, 0}, {t2, POLLIN, 0}, {t6, POLLIN, 0},
                          {u, POLLIN, 0}, {us, POLLIN, 0}, {as, POLLIN, 0}};
  for (;;) {
    if (poll(fds, 7, -1) < 0) {
      if (errno == EINTR) continue;
      return 1;
    }
    if (fds[0].revents) {
      char b[16];
      if (read(0, b, sizeof b) <= 0) return 0;  // the orchestrator closed our stdin
    }
    for (int i = 1; i < 7; i++) {
      if (fds[i].fd < 0 || !(fds[i].revents & POLLIN)) continue;
      if (i == 4) {
        unsigned char b[512];
        struct sockaddr_in from;
        socklen_t fl = sizeof from;
        ssize_t k = recvfrom(u, b, sizeof b, 0, (struct sockaddr *)&from, &fl);
        if (k <= 0) continue;
        if (k >= 17 && k < 400) {  // DNS: echo the question, add one A record 192.0.2.7
          b[2] = 0x81;
          b[3] = 0x80;
          b[7] = 1;
          unsigned char rr[] = {0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 192, 0, 2, 7};
          memcpy(b + k, rr, sizeof rr);
          k += sizeof rr;
        }
        sendto(u, b, (size_t)k, 0, (struct sockaddr *)&from, fl);
      } else {
        int c = accept(fds[i].fd, NULL, NULL);
        if (c >= 0) close(c);
      }
    }
  }
}

int main(int argc, char **argv) {
  if (argc >= 3 && strcmp(argv[1], "serve") == 0) return serve(argv[2]);
  if (argc >= 2 && strcmp(argv[1], "list") == 0) {
    fputs(corpus_list(), stdout);
    return 0;
  }
  if (argc < 3) {
    fprintf(stderr, "usage: probe <name> <cfg> | probe serve <dir> | probe list\n");
    return 2;
  }
  char out[1024];
  corpus_probe(argv[1], argv[2], out, sizeof out);
  puts(out);
  return 0;
}
#endif
