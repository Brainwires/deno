# What programs other than Deno reach from a jailed script (DENEXT.md item
# 111): curl through the proxy the jail names in HTTP_PROXY, through a CONNECT
# tunnel, and around the proxy; a raw socket from `node` (Deno). One line per
# check in results.txt.
reach() {
  code=$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$@" 2>/dev/null) &&
    [ "$code" != 403 ] && echo allowed || echo denied
}
{
  echo "proxy 4545: $(reach http://localhost:4545/)"
  echo "proxy 4546: $(reach http://localhost:4546/)"
  echo "tunnel 4545: $(reach --proxytunnel http://localhost:4545/)"
  echo "direct 4545: $(reach --noproxy '*' http://localhost:4545/)"
  echo "direct 4546: $(reach --noproxy '*' http://localhost:4546/)"
  echo "direct 127.0.0.1:4546: $(reach --noproxy '*' http://127.0.0.1:4546/)"
} > results.txt
node socket.js >> results.txt
