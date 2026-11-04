// Copyright 2018-2026 the Deno authors. MIT license.

#[inline(always)]
pub fn construct_v8_flags(
  default_v8_flags: &[String],
  v8_flags: &[String],
  env_v8_flags: Vec<String>,
) -> Vec<String> {
  std::iter::once("UNUSED_BUT_NECESSARY_ARG0".to_owned())
    .chain(default_v8_flags.iter().cloned())
    .chain(env_v8_flags)
    .chain(v8_flags.iter().cloned())
    .collect::<Vec<_>>()
}

/// The time zone `--seed` pins the process to.
pub const DETERMINISTIC_TIME_ZONE: &str = "UTC";
/// The default locale (`Intl`, `Date.prototype.toString`'s zone name,
/// `navigator.language`) `--seed` pins the process to.
pub const DETERMINISTIC_LOCALE: &str = "en-US";

/// Under `--seed` (the deterministic mode, upstream #35718), pins the time
/// zone to UTC and ICU's default locale to en-US, so output that formats a
/// date or a number does not depend on the host. Without a seed it does
/// nothing. Call it once, after the V8 platform (and so ICU's data) is
/// initialized and before the first isolate is created. `TZ` is set in the
/// process environment (libc, children and `Deno.env.get("TZ")` see it) and
/// ICU's default zone is set directly too, since ICU on Windows reads the
/// zone from the OS and ignores `TZ`. ICU's default locale is process-wide.
pub fn apply_deterministic_environment(seed: Option<u64>) {
  use deno_runtime::deno_core::v8::icu;
  if seed.is_none() {
    return;
  }
  deno_runtime::deno_os::ProcessEnvGuard::lock()
    .set_var("TZ", DETERMINISTIC_TIME_ZONE);
  let pinned = icu::set_default_time_zone(DETERMINISTIC_TIME_ZONE);
  debug_assert!(pinned, "ICU knows {DETERMINISTIC_TIME_ZONE}");
  icu::set_default_locale(DETERMINISTIC_LOCALE);
}
