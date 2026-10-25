# A listed NAME that resolves to this machine (DENEXT.md item 111): the jail's
# proxy refuses it (403), plainly and as a CONNECT tunnel, while the literal
# address listed as itself still goes through. One line per check in
# results.txt.
code() {
  format=$1
  shift
  curl -sS -m 10 -o /dev/null -w "$format" "$@" 2>/dev/null
}
reach() {
  c=$(code "%{http_code}" "$1")
  [ "$c" = 403 ] && echo refused || echo "reached ($c)"
}
# the proxy's own refusals (403, 502) and curl's failure (000) are not the
# server answering
served() {
  c=$(code '%{http_code}' "$1")
  case "$c" in
    000 | 403 | 502) echo "not reached ($c)" ;;
    *) echo reached ;;
  esac
}
{
  echo "literal 127.0.0.1:4546: $(served http://127.0.0.1:4546/)"
  echo "name to loopback: $(reach http://ip6-localhost:4545/)"
  c=$(code '%{http_connect}' --proxytunnel http://ip6-localhost:4545/)
  [ "$c" = 403 ] && t=refused || t="reached ($c)"
  echo "name to loopback, tunnel: $t"
} > results.txt
