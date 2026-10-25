# the same from a program that isn't Deno, through the link and the real path
sibling="$INIT_CWD/node_modules/@denotest/jail-net-declared"
real=$(cd "$sibling" && pwd -P)
try() {
  if (echo "// pwned" >> "$1") 2>/dev/null; then echo allowed; else echo denied; fi
}
{
  echo "sh: write sibling package: $(try "$sibling/index.js")"
  echo "sh: write sibling real path: $(try "$real/index.js")"
  echo "sh: write node_modules: $(try "$INIT_CWD/node_modules/.pwned-sh")"
} >> results.txt
