# The ways out of the jail that DENEXT.md item 124 closes, one line per
# check in results.txt: typing into the terminal, a hard link from outside
# the writable roots, a process left behind, and (macOS) the system
# services a script could leave the sandbox through: LaunchServices
# (`open`), launchd, the keychain, AppleEvents.
rc() { "$@" >/dev/null 2>&1 && echo allowed || echo denied; }
{
  echo "tty open: $(rc sh -c ': < /dev/tty')"
  if command -v python3 >/dev/null 2>&1; then
    python3 tiocsti.py "$PPID" 2>/dev/null \
      || echo "tiocsti: denied"
  else
    echo "tiocsti: denied"
    echo "own session: unknown"
  fi
  echo "hard link from outside: $(rc ln "$INIT_CWD/package.json" linked-package.json)"
  echo "hard link inside: $(rc sh -c 'echo x > a && ln a b')"
  nohup sleep 300 >/dev/null 2>&1 &
  echo $! > sleeper.pid
  echo "sleeper: started"
  if [ "$(uname)" = Darwin ]; then
    printf '#!/bin/sh\ntouch "%s/escaped-open"\n' "$INIT_CWD" > x.command
    chmod +x x.command
    echo "open .command: $(rc open x.command)"
    echo "open -a: $(rc open -a /System/Applications/Calculator.app)"
    echo "launchctl list: $([ "$(launchctl list 2>/dev/null | wc -l)" -gt 1 ] && echo allowed || echo denied)"
    echo "keychain: $(rc security list-keychains)"
    echo "appleevents: $(rc osascript -e 'tell application "Finder" to get name of home')"
    echo "pasteboard write: $(rc sh -c 'echo x | pbcopy')"
  fi
} > results.txt
