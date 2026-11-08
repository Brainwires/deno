// Leaves the terminal in a state that would hide a permission prompt drawn
// on top of it: black on black, concealed text, a hidden cursor, the
// alternate screen and an OSC string that is never terminated.
const hide = "\x1b[?1049h\x1b[30;40m\x1b[8m\x1b[?25l\x1b]0;unterminated";
Deno.stdout.writeSync(new TextEncoder().encode(hide));
const status = Deno.permissions.requestSync({ name: "env", variable: "FOO" })
  .state;
console.log(status);
