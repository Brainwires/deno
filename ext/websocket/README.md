# deno_websocket

This op crate implements the websocket functions of Deno.

Spec: https://html.spec.whatwg.org/multipage/web-sockets.html

## Embedder address deny list

With a `deno_fetch::deny_addresses::EmbedderDenyList` in the `OpState`, a
`WebSocket` or `WebSocketStream` connection is refused when an address it would
go to is denied, whichever HTTP client opens it: the name is resolved once,
refused when any of its addresses is denied, the connection goes to the checked
addresses only, and the connected peer is checked again; behind an HTTP(S) or
SOCKS proxy the destination is checked as for `fetch()`. The `error` event's
`error` (and the rejection of `WebSocketStream`'s `opened`) is a `NetworkError`
`DOMException` whose `cause.code` is `"ERR_ADDRESS_DENIED"`. Without the list
nothing changes.
