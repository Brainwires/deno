# Security Policy

This repository is Brainwires' fork of the Deno runtime, maintained for the
[denext](https://github.com/Brainwires/denext) runtime line (the
`2.9.7-denext.N` releases: the Deno CLI fork, `denort`, and the Deno Desktop
runtime built with [Brainwires/laufey](https://github.com/Brainwires/laufey)).
It is not affiliated with Deno Land Inc. or Cloudflare. Deno is a trademark of
its respective owner.

## Reporting a vulnerability

Please report vulnerabilities **privately** through GitHub's private
vulnerability reporting:
[open a security advisory](https://github.com/Brainwires/deno/security/advisories/new).

**Please do not open security issues in the public issue tracker, and do not
open public pull requests that disclose an unfixed vulnerability.**

Include as much as you can: the affected release (for example
`2.9.7-denext.13`), the target (OS and architecture), steps to reproduce, and
the impact you observed. We will acknowledge a report within 3 business days and
keep you informed while we work on a fix. We are happy to credit you in the
advisory once the issue is resolved.

If the issue also affects upstream Deno (it reproduces on a stock Deno release
without this fork's patches), please report it to the Deno team as well, through
the
[Deno security advisory form](https://github.com/denoland/deno/security/advisories/new).
We will coordinate disclosure with them where possible.

### Please do the following

- Do not take advantage of the vulnerability or problem you have discovered.
- Do not publish or reveal the problem until it has been resolved.
- Do not use attacks on physical security or applications of third parties.
- Do provide sufficient information to reproduce the problem, so we will be able
  to resolve it as quickly as possible.

### Our commitment to you

- If you act in accordance with this policy, we will not take legal action
  against you in regard to your report.
- We will handle your report with strict confidentiality, and not pass on your
  personal details to third parties without your permission.

## Supported versions

Only the current denext runtime line receives security fixes. Fixes ship as a
new `denext-runtime-v<deno>-denext.<N>` release on this repository, and denext
pins the new release.

| Version                                        | Supported            |
| ---------------------------------------------- | -------------------- |
| Latest `2.9.7-denext.N` release                | Yes                  |
| Earlier `2.9.7-denext.N` releases              | No                   |
| Stock Deno releases (not built from this fork) | No (report upstream) |

## How we track upstream

- We follow upstream Deno security releases and advisories and merge the fixes
  into this fork's runtime line, then cut a new `denext.N` release.
- A scheduled `cargo audit` workflow (`.github/workflows/cargo_audit.yml`)
  checks `Cargo.lock` against the RustSec advisory database every week.
- V8 comes from the published `rusty_v8` crates; Chromium/V8 security releases
  are tracked through them. Prebuilt V8 libraries are mirrored on this
  repository's `rusty_v8-mirror` releases.
- The Desktop runtime's CEF backend follows CEF / Chromium security releases
  through [Brainwires/laufey](https://github.com/Brainwires/laufey).

## Security Model

The following paragraphs outline the rough security model for Deno. The model
may change slightly over time, but in general the model is as follows:

- All JavaScript run in Deno is considered untrusted, so permissions are thus
  never enforced in JavaScript.
- All JavaScript run in a single Deno process is considered to be part of the
  same program and is not isolated from itself. This means that Deno does not
  guarantee that values set by one JS module will be inaccessible to another, or
  that a value set in one web worker can not be accessed by another.
- All runtime I/O is considered to be privileged and must always be guarded by a
  runtime permission. This includes filesystem access, network access, etc.
  - The only exception to this is runtime storage explosion attacks that are
    isolated to a part of the file system, caused by evaluated code (for
    example, caching big dependencies or no limits on runtime caches such as the
    [Web Cache](https://developer.mozilla.org/en-US/docs/Web/API/Cache) API).
- Users should not be able to self-escalate their permissions without explicit
  consent.
- I/O required to build an initial static module graph should always follow the
  permissions of its parent. If there is no parent, all permissions are granted.
  As an example, this means that the initial static module graph that is
  constructed when doing `deno run`, does not have any permission restrictions.
  However, the module graph constructed as the result of loading a web worker or
  dynamic import will be restricted to the permissions of the caller (the main
  worker most likely).
- We consider the V8 VM to be a secure sandbox.
