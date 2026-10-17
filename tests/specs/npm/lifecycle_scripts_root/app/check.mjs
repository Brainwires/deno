const read = (f) => {
  try {
    return Deno.readTextFileSync(f);
  } catch {
    return undefined;
  }
};
const pre = read("pre.txt");
const post = read("post.txt");
const prepare = read("prepare.txt");
console.log(
  pre || post || prepare ? `${pre} post=${post} ${prepare}` : "none",
);
