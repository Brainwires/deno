for (const kind of ["processInfo", "processes"] as const) {
  const status = await Deno.permissions.query({ name: "sys", kind });
  console.log(`${kind}: ${status.state}`);
}
