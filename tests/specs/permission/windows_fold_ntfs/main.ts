// The Windows permission fold must merge exactly the names NTFS merges: a
// fold wider than the volume's $UpCase table lets a grant for `settings`
// cover a sibling `ſettings` that NTFS keeps apart, and a narrower one lets
// a deny scope for `Ärger` miss `ärger`, which NTFS treats as the same
// folder. For each pair of spellings below, NTFS is asked whether they name
// one folder (the second spelling finds the folder made with the first), and
// the permission layer whether a scope written with the first covers the
// second (an allow scope, and a deny scope). Any disagreement is a
// violation; the per-pair lines record what this volume does (DENEXT.md
// 225).
const pairs: [string, string][] = [
  ["a", "A"],
  ["ä", "Ä"], // ä Ä
  ["я", "Я"], // я Я
  ["σ", "Σ"], // σ Σ
  ["ς", "Σ"], // ς Σ (final sigma)
  ["ı", "I"], // ı (dotless i)
  ["ſ", "S"], // ſ (long s)
  ["k", "K"], // k, Kelvin sign
  ["ß", "ẞ"], // ß ẞ
  ["ǆ", "Ǆ"], // ǆ Ǆ
  ["ǅ", "Ǆ"], // ǅ Ǆ (titlecase)
  ["ͅ", "Ι"], // combining ypogegrammeni, Ι
  ["ẛ", "Ṡ"], // ẛ Ṡ
  ["ϐ", "Β"], // ϐ Β
  ["ÿ", "Ÿ"], // ÿ Ÿ
  ["ɥ", "Ɥ"], // ɥ Ɥ (Unicode 6.0)
  ["ᲀ", "В"], // ᲀ В (Unicode 9)
  ["ᏸ", "Ᏸ"], // ᏸ Ᏸ (Cherokee, Unicode 8)
  ["ꭰ", "Ꭰ"], // ꭰ Ꭰ (Cherokee, Unicode 8)
  ["ა", "Ა"], // ა Ა (Georgian Mtavruli, Unicode 11)
  ["ꞔ", "Ꞔ"], // ꞔ Ꞔ (Unicode 12)
];

function child(flags: string[], path: string): string {
  const { stdout, stderr } = new Deno.Command(Deno.execPath(), {
    args: ["run", ...flags, "child.ts", path],
  }).outputSync();
  const decoder = new TextDecoder();
  const out = (decoder.decode(stdout) + decoder.decode(stderr)).trim();
  return out === "done" || out === "refused" ? out : `error ${out}`;
}

let violations = 0;
pairs.forEach(([first, second], i) => {
  const dir = `base/p${i}_${first}`;
  const other = `base/p${i}_${second}`;
  Deno.mkdirSync(dir, { recursive: true });
  let same: boolean;
  try {
    Deno.statSync(other);
    same = true;
  } catch {
    same = false;
  }
  const allowCovers = child(
    ["--allow-read", `--allow-write=${dir}`],
    `${other}/f.txt`,
  ) === "done";
  const denyCovers = child(
    ["--allow-read", "--allow-write=base", `--deny-write=${dir}`],
    `${other}/g.txt`,
  ) === "refused";
  const ok = allowCovers === same && denyCovers === same;
  if (!ok) violations++;
  const cp = (s: string) =>
    [...s].map((c) => "U+" + c.codePointAt(0)!.toString(16).padStart(4, "0"))
      .join(" ");
  console.log(
    `${cp(first)} / ${cp(second)}: one name on NTFS: ${same}, allow scope ` +
      `covers: ${allowCovers}, deny scope covers: ${denyCovers}` +
      (ok ? "" : " (VIOLATION)"),
  );
});
console.log(`violations: ${violations}`);
