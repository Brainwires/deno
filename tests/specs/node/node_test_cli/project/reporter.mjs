export default async function* reporter(source) {
  for await (const event of source) {
    if (event.type === "test:pass") yield `passed ${event.data.name}\n`;
    if (event.type === "test:summary" && event.data.file === undefined) {
      yield `tests ${event.data.counts.tests}\n`;
    }
  }
}
