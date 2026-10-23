export async function load(url, context, nextLoad) {
  const result = await nextLoad(url, context);
  if (url.includes("/original.")) {
    const source = typeof result.source === "string"
      ? result.source
      : new TextDecoder().decode(result.source);
    return { ...result, source: source.replace("ORIGINAL", "TRANSFORMED") };
  }
  return result;
}
