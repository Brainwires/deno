# Kernel sandbox coverage: sockets and file paths

What the kernel sandbox (`--kernel-sandbox`) holds for each way the runtime
opens a socket or reaches a file by path, and what stays held by Deno's checks
only. Two tables: the network under a broker policy (a host-scoped
`--allow-net`), and the file-path operations that don't go through the
race-safe opens. Every row that says "checked only" or "fails closed" has its
reason. Keep it in step with the code: a new socket or path operation gets a
row.

Terms:

- **routed**: the socket comes from the broker (`deno_net_route` →
  `deno_sandbox::net_authority`), which checks the grant, resolves names
  itself and connects to exactly the address it checked.
- **pre-bound**: the listener was bound before the process confined itself
  (Linux, Mode A) and is handed to the operation that listens there.
- **fails closed**: not routed. Under a broker policy the kernel refuses the
  socket, so the operation fails; it is never an escape.
- **kernel**: held by the kernel sandbox itself, for native code and children
  too.
- **checked only**: Deno's check decides and the kernel sandbox adds nothing
  (the kernel's rule is wider there, or there is no rule).

## Network, under a broker policy

The confined process tree has no IP socket that reaches an address by itself:
`socket(2)` passes TCP stream sockets only (the children's proxy needs them),
every `connect(2)` is answered by the supervisor (`deno_sandbox::net_gate`:
only the proxy's loopback address, never `AF_UNSPEC`), `listen`, `io_uring`
and `MSG_FASTOPEN` sends are refused, and on macOS the profile denies all
network but the proxy's `localhost:<port>`.

| Path | Where | State | Reason / notes |
| --- | --- | --- | --- |
| `Deno.connect` (TCP), with Happy Eyeballs | `ext/net/ops.rs`, `happy_eyeballs.rs` | routed | resolve and connect are both the broker's; each racing attempt is its own request |
| `Deno.connectTls`, `Deno.startTls` | `ext/net/ops_tls.rs` | routed | TLS runs in the confined process over the routed socket |
| `Deno.listen`, `Deno.listenTls`, `Deno.serve`, `deno serve` (also `--parallel`) | `ext/net/tcp.rs` | pre-bound, else routed | the broker binds and listens; `accept` needs no socket call |
| `Deno.listenDatagram`, `DatagramConn.send` | `ext/net/ops.rs` | routed only with an unscoped connect grant and no deny; else fails closed | a UDP socket can send to any address (`sendto` with a destination works on a connected UDP socket on Linux, and `connect` can be called again), so a host-scoped grant has none |
| `Deno.resolveDns`, `node:dns` `resolve*` | `ext/net/ops.rs`, `routed.rs` | routed | each UDP query is one exchange the broker carries out with a name server the grant names (the check Deno already makes); DNS over TCP is a routed connection |
| Name resolution for connect / listen (`getaddrinfo`) | `ext/net/resolve_addr.rs`, `ext/node/ops/dns.rs`, `ext/fetch/dns.rs` | routed | the broker resolves only names the grant holds; its answers are what later connect requests are checked against |
| Unix sockets: connect, listen, datagram | `ext/net/ops_unix.rs` | routed | the broker checks read and write on the path and the network grant for it; the path is checked, then connected by name (see the file table) |
| Abstract Unix sockets | `ext/net/ops_unix.rs` | fails closed | not routed; the confined process has no `AF_UNIX` socket |
| QUIC, WebTransport | `ext/net/quic.rs` | as UDP | one UDP socket talks to every peer |
| vsock | `ext/net`, `ext/fetch/proxy.rs` | not routed | held by the kernel policy's vsock rule, as in Mode A |
| `fetch`, `WebSocket`, `EventSource`, `Deno.createHttpClient`, Deno KV remote, the CLI's own downloads | `ext/fetch/dns.rs` | routed | one connector for all of them; with no permissions to check (the CLI's own client) the broker's check is the only one, against the network grant |
| `fetch` through an HTTP(S), SOCKS or Unix-socket proxy | `ext/fetch/proxy.rs` | routed | the connection to the proxy is the routed one |
| `node:net` connect / listen, and everything built on it (`node:http`, `https`, `http2`, `tls`) | `ext/node/ops/tcp_wrap.rs`, `libs/core/uv_compat/tcp.rs` | routed, pre-bound | `uv_tcp_bind` makes no socket: the bind is carried out by `uv_listen` / `uv_tcp_connect` |
| Native addons using the libuv TCP surface | `libs/core/uv_compat/tcp.rs` | routed | the same functions |
| `node:net` named pipes (Unix sockets), IPC channels | `libs/core/uv_compat/pipe.rs` | `socketpair` IPC works; named pipes fail closed | the pipe surface creates its own `AF_UNIX` sockets; not routed yet |
| `node:dgram` | `ext/node/ops/udp.rs` | as UDP | |
| `inspector.open(port, host)` with a host name | `ext/node/ops/inspector.rs` | fails closed for a name; an IP literal works | the name is resolved with `getaddrinfo` directly; the listener itself is pre-bound / routed |
| `--inspect`, `--inspect-brk`, `--inspect-wait` | `libs/inspector_server` | pre-bound, else routed | the port is in the policy |
| WebRTC: ICE over UDP, mDNS | `ext/webrtc/peer.rs` | as UDP | an ICE socket sends to every candidate; under a host-scoped grant no UDP candidate is gathered |
| WebRTC: ICE-TCP (dial and listen), TURN over TCP / TLS, STUN / TURN host names | `ext/webrtc/peer.rs`, `net.rs` | routed | |
| WebRTC: the local-address probe (a connected UDP socket that sends nothing) | `ext/webrtc/peer.rs` | fails closed | the probe finds no address; candidates fall back to the interface list |
| Postgres (`Deno.SQL`): TCP and Unix socket | `ext/sql/pg.rs` | routed | |
| The vendored `tokio-postgres` connector | `libs/vendor/tokio-postgres` | fails closed | unused: `ext/sql` opens the socket and hands it over |
| OpenTelemetry exporter | `ext/telemetry` | fails closed | its own connector, started by the runtime, not checked against `--allow-net`; needs a class of its own at the broker (as module imports do) |
| `Deno.cron` control socket, the deploy tunnel, crash-report upload | `ext/cron/socket.rs`, `cli/lib.rs`, `runtime/crash_report.rs` | fails closed | runtime services outside the grant; same reason |
| Desktop: devtools relay, scheme bridge, renderer inspect | `cli/tools/desktop_*`, `cli/rt_desktop` | not routed | the desktop runtime has no kernel sandbox yet |
| Dynamic `import()` of a remote module not in the cache, npm auto-install | the CLI's file fetcher | fails closed | the static graph is fetched before the process confines itself; a later fetch needs an import class at the broker |
| Native code: `socket(2)` / `connect(2)` directly, a re-pointed socket, Fast Open, `io_uring` | FFI, addons | kernel | no socket reaches an address without the supervisor's answer |
| Child processes | `--allow-run` | kernel: the broker's proxy only | `HTTP_PROXY` / `HTTPS_PROXY` point at the allow-listing proxy; a program that ignores them has no network |

## File paths outside the race-safe opens

`deno_permissions::checked_open` opens a path and checks it in one step. The
operations below can't, or don't yet. Under `--kernel-sandbox=enforce` a path
swapped between the check and the open only matters where Deno's rule is finer
than the kernel's: without a kernel layer (prompting, `off`, Windows), and for
the paths the implicit set makes readable without a grant.

| Operation | Where | State | Why it stays by name |
| --- | --- | --- | --- |
| `Deno.connectTls({ certFile })`, `Deno.SQL` file reads | `ext/net/ops_tls.rs`, `ext/sql/files.rs` | checked after the open (`check_opened`) | read-only opens: the file that opened is checked again as the object it is |
| SQLite databases (`node:sqlite`, `Deno.openKv`, `Deno.SQL` SQLite, backup) | `ext/node_sqlite`, `ext/kv/sqlite.rs`, `ext/sql/sqlite.rs` | checked only | SQLite opens the database by name and creates its `-wal`, `-shm` and journal siblings by name; it takes no descriptor |
| `Deno.watchFs`, `node:fs.watch` | `runtime/ops/fs_events.rs` | checked only | the OS watch APIs (inotify, FSEvents, kqueue) take paths; events are filtered by path again when delivered |
| `Deno.dlopen`, `process.dlopen`, native addons | `ext/ffi`, `ext/napi` | checked only; kernel: the read scope (and `file-map-executable` on macOS) | the dynamic loader opens the library and its dependencies itself; a scoped `--allow-ffi` can load any library in the read scope (documented gap) |
| `Deno.Command` / `node:child_process`: the program path, `cwd` | `ext/process` | checked only; kernel: the exec scope | `execve` and `chdir` take paths; the child's own opens are held by the kernel sandbox it inherits |
| `Deno.chdir`, `process.chdir` | `ext/fs`, `ext/node` | checked only | `chdir` takes a path; relative opens that follow are checked as the absolute path they name |
| `node:wasi` preopens and path operations | `ext/node/ops/wasi.rs` | checked only | resolved against the preopen's canonical root on every call; not converted to descriptors yet |
| `process.loadEnvFile`, `fetch("file:")`, dynamic local imports, `Deno.bundle` inputs, test snapshots | `ext/node/lib.rs`, `ext/fetch/fs_fetch_handler.rs`, `libs/resolver/file_fetcher.rs`, `cli/tools/bundle`, `cli/ops/testing.rs` | checked only until the file-system interface takes checked files | they read through `deno_fs::FileSystem` or the bundler; they follow `ext/fs` |
| Unix-socket paths (`Deno.connect({ transport: "unix" })`, listeners, the `fetch` proxy) | `ext/net/ops_unix.rs`, `ext/fetch/proxy.rs` | checked only | `connect` and `bind` on a Unix socket take a path |
| Desktop: `BrowserWindow.share()`, `startDrag()` | `runtime/ops/desktop.rs` | checked only | the paths are handed to the OS share / drag services |
| npm resolution (`package.json`, `node_modules` probes) | `cli/lib/npm/permission_checker.rs` | checked only | the node resolver's file-system interface has no descriptor form |
| Hard links to a denied file | everywhere | checked only (path semantics) | a hard link placed in an allowed folder is the same file under an allowed path; Landlock and Seatbelt see paths too |
| The cache overlay (`DENO_DIR/sandboxed/<policy hash>`) | `libs/resolver/cache` | kernel | opened by the runtime, never by a program's path; the overlay's write rule is the only cache write the policy has |
| Temp files (`Deno.makeTempFile` / `makeTempDir`, `node:fs.mkdtemp`) | `ext/fs` | follows `ext/fs` | created with `O_EXCL` / `mkdir` under a checked parent: a swapped link can't redirect a create that refuses an existing name |
| Rename, link and symlink edges | `ext/fs` | follows `ext/fs` | both parents are checked; a rename across a swapped parent is the `*at` conversion's to close |
