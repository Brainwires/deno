# Security Policy

This repository is Brainwires' fork of Deno: the runtime behind denext's Deno
toolchain and its Deno Desktop runtime (`3.0.0-denext.N` releases). Thank you
for taking the time to investigate its security.

## Reporting a vulnerability

Please open a new
[Security advisory](https://github.com/Brainwires/deno/security/advisories/new)
on this repository with your findings (**Security → Report a vulnerability**).
This covers anything in the fork's releases, including code it shares with
upstream Deno.

**Please do not open security issues in the public issue tracker.**

When a report also affects upstream Deno, we report it privately to the Deno
team through
[their process](https://github.com/denoland/deno/security/advisories/new) and
coordinate the fix and disclosure with them. If you found an issue in stock Deno
only, report it to Deno directly.

Try to include as much information as possible: the release (`deno --version`),
the platform, and the steps to reproduce.

### Please do the following

- Do not take advantage of the vulnerability or problem you have discovered.
- Do not publish or reveal the problem until it has been resolved.
- Do not use attacks on physical security or applications of third parties.
- Do provide sufficient information to reproduce the problem, so we will be able
  to resolve it as quickly as possible. Usually, a list of steps to follow, and
  the vulnerable version is enough. More complex vulnerabilities may require
  further explanation.

### Our commitment to you

- We acknowledge a report within 3 business days and triage it within 7.
- We publish a fix and a GitHub security advisory for every confirmed issue in a
  released version.
- If you act in accordance with this policy, we will not take legal action
  against you in regard to your report.
- We will handle your report with strict confidentiality, and not pass on your
  personal details to third parties without your permission.

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
