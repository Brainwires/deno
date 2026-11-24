// Copyright 2018-2026 the Deno authors. MIT license.
// A program on the run list that makes one exec call, in the form its first
// argument names, and says how the call ended when it returns:
//
//   helper ok                 prints "ok"
//   helper interp <file>      prints <file>'s ELF interpreter (its loader)
//   helper path <file>        execve(<file>)
//   helper loader <file>      execve(<file>'s loader, <file>)
//   helper memory <file>      a memory file with <file>'s bytes, fexecve
//   helper memory-name <file> the same, by the descriptor's /proc name
//   helper descriptor <file>  execveat(fd of <file>, "", AT_EMPTY_PATH)
//   helper again              execve("/proc/self/exe")
//
// The program it starts gets "ok" as its argument (this helper prints it;
// other.c prints "ran other").
#define _GNU_SOURCE
#include <elf.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

extern char **environ;

static int report(void) {
  if (errno == EACCES) {
    puts("refused");
  } else {
    printf("failed: %s\n", strerror(errno));
  }
  return 0;
}

// The interpreter a dynamically linked ELF64 file names; 0 when it has one.
static int interp_of(const char *file, char *out, size_t size) {
  int fd = open(file, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return -1;
  Elf64_Ehdr eh;
  if (pread(fd, &eh, sizeof eh, 0) != (ssize_t)sizeof eh) return -1;
  for (int i = 0; i < eh.e_phnum; i++) {
    Elf64_Phdr ph;
    off_t at = (off_t)eh.e_phoff + (off_t)i * eh.e_phentsize;
    if (pread(fd, &ph, sizeof ph, at) != (ssize_t)sizeof ph) return -1;
    if (ph.p_type != PT_INTERP) continue;
    size_t n = ph.p_filesz < size - 1 ? ph.p_filesz : size - 1;
    if (pread(fd, out, n, (off_t)ph.p_offset) != (ssize_t)n) return -1;
    out[n] = 0;
    close(fd);
    return 0;
  }
  close(fd);
  return -1;
}

// A memory file holding a copy of `file`; -1 when it can't be made.
static int memory_copy(const char *file) {
  int in = open(file, O_RDONLY | O_CLOEXEC);
  if (in < 0) return -1;
  int fd = memfd_create("copy", 0);
  if (fd < 0) return -1;
  char buf[65536];
  ssize_t n;
  while ((n = read(in, buf, sizeof buf)) > 0) {
    if (write(fd, buf, (size_t)n) != n) return -1;
  }
  close(in);
  return n < 0 ? -1 : fd;
}

int main(int argc, char **argv) {
  const char *form = argc > 1 ? argv[1] : "";
  const char *file = argc > 2 ? argv[2] : "";
  char *args[] = {(char *)file, "ok", NULL};
  if (strcmp(form, "ok") == 0) {
    puts("ok");
    return 0;
  }
  if (strcmp(form, "interp") == 0) {
    char loader[4096];
    if (interp_of(file, loader, sizeof loader) != 0) return 1;
    puts(loader);
    return 0;
  }
  if (strcmp(form, "path") == 0) {
    execve(file, args, environ);
    return report();
  }
  if (strcmp(form, "loader") == 0) {
    char loader[4096];
    if (interp_of(file, loader, sizeof loader) != 0) {
      // a statically linked program has no loader to start it through
      puts("refused");
      return 0;
    }
    char *through[] = {loader, (char *)file, "ok", NULL};
    execve(loader, through, environ);
    return report();
  }
  if (strcmp(form, "memory") == 0 || strcmp(form, "memory-name") == 0) {
    int fd = memory_copy(file);
    if (fd < 0) {
      printf("no memory file: %s\n", strerror(errno));
      return 0;
    }
    if (strcmp(form, "memory") == 0) {
      fexecve(fd, args, environ);
    } else {
      char name[64];
      snprintf(name, sizeof name, "/proc/self/fd/%d", fd);
      execve(name, args, environ);
    }
    return report();
  }
  if (strcmp(form, "descriptor") == 0) {
    int fd = open(file, O_RDONLY);
    if (fd < 0) {
      printf("failed: %s\n", strerror(errno));
      return 0;
    }
    syscall(SYS_execveat, fd, "", args, environ, AT_EMPTY_PATH);
    return report();
  }
  if (strcmp(form, "again") == 0) {
    execve("/proc/self/exe", args, environ);
    return report();
  }
  return 2;
}
