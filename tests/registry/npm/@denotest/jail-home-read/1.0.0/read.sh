# Tries to read files in the user's real home directory (JAIL_REAL_HOME, the
# fixture's HOME, passed through the package's `env`) and records which it
# could: a credential file, a sibling project's .env and notes (also through
# a link in the project), a config file in the home directory, the project's
# own deno.json, and this package's own files; and how a refused read fails.
r() {
  if cat "$2" >/dev/null 2>&1; then echo "$1: read"; else echo "$1: denied"; fi
}
{
  r "fly config" "$JAIL_REAL_HOME/.fly/config.yml"
  r "shell history" "$JAIL_REAL_HOME/.zsh_history"
  r "sibling .env" "$JAIL_REAL_HOME/sibling/.env"
  r "sibling notes" "$JAIL_REAL_HOME/sibling/notes.txt"
  r "sibling notes through a link" "$JAIL_REAL_HOME/$JAIL_PROJECT/to_sibling/notes.txt"
  err=$(cat "$JAIL_REAL_HOME/sibling/notes.txt" 2>&1 >/dev/null)
  case "$err" in
    "") echo "sibling notes error: none" ;;
    *"No such file"*) echo "sibling notes error: not found" ;;
    *) echo "sibling notes error: refused" ;;
  esac
  if ls "$JAIL_REAL_HOME" >/dev/null 2>&1; then
    echo "home listing: read"
  else
    echo "home listing: denied"
  fi
  r "home .nvmrc" "$JAIL_REAL_HOME/.nvmrc"
  r "project deno.json" "$JAIL_REAL_HOME/$JAIL_PROJECT/deno.json"
  r "own package.json" "./package.json"
} > results.txt
