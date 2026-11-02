export default async function* reporter() {
  yield "Going to throw an error\n";
  throw new Error("Reporting error");
}
