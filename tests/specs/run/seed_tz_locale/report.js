export function report() {
  const resolved = new Intl.DateTimeFormat().resolvedOptions();
  const epoch = new Date(0);
  return [
    `timeZone: ${resolved.timeZone}`,
    `locale: ${resolved.locale}`,
    `navigator.language: ${navigator.language}`,
    `TZ: ${Deno.env.get("TZ")}`,
    `toString: ${epoch.toString()}`,
    `toLocaleString: ${epoch.toLocaleString()}`,
    `number: ${(1234567.891).toLocaleString()}`,
  ].join("\n");
}
