// The variables the dynamic loader and libc read at startup, set by an env
// file: the program has them, with the kernel sandbox as without it. The
// image the runtime re-executes itself as is started without them and sets
// them again where the env files are loaded.
for (
  const name of [
    "GREETING",
    "LD_PRELOAD",
    "LD_AUDIT",
    "DYLD_INSERT_LIBRARIES",
    "GCONV_PATH",
    "LOCPATH",
    "MALLOC_PERTURB_",
    "DENO_INTERNAL_ENV_FILE_LOADER_VARIABLES",
  ]
) {
  console.log(`${name}=${Deno.env.get(name)}`);
}
