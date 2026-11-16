// Copyright 2018-2026 the Deno authors. MIT license.
// The regular files this process holds open for writing (descriptors 3 and
// up), one path per line in `out`; returns how many.
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int writable_files(char *out, int size) {
  int count = 0;
  size_t used = 0;
  out[0] = 0;
  for (int fd = 3; fd < 4096; fd++) {
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0) continue;
    int mode = flags & O_ACCMODE;
    if (mode != O_WRONLY && mode != O_RDWR) continue;
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode)) continue;
    char path[PATH_MAX] = "?";
#ifdef __APPLE__
    if (fcntl(fd, F_GETPATH, path) != 0) strcpy(path, "?");
#else
    char link[64];
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t len = readlink(link, path, sizeof path - 1);
    if (len >= 0) path[len] = 0;
    // anonymous memory files hold no one's data
    if (strncmp(path, "/memfd:", 7) == 0) continue;
#endif
    count++;
    int n = snprintf(out + used, size - used, "%d %s\n", fd, path);
    if (n > 0 && used + n < (size_t)size) used += n;
  }
  return count;
}
