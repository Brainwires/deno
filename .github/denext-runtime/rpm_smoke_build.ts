// Copyright 2018-2026 the Deno authors. MIT license.
// Build the `.rpm` rpm_smoke.sh installs, with denext's own packager, so its
// dependency list is the one denext apps ship with.
//
// Usage: rpm_smoke_build.ts <bundle dir> <x86_64|arm64> <webview|cef> <out.rpm>
//        <the app's origin>
// Env: DENEXT_PACKAGING_VERSION
const [bundleDir, arch, backend, out, origin] = Deno.args;
const version = Deno.env.get("DENEXT_PACKAGING_VERSION");
if (!version || !origin || (arch !== "x86_64" && arch !== "arm64")) {
  throw new Error("usage: see the top of rpm_smoke_build.ts");
}
const scheme = new URL(origin).protocol.slice(0, -1);
const { buildDesktopRpm } = await import(
  `jsr:@denext/denext@${version}/desktop`
);

// What denext writes beside the executable; launch.sh sets the same as env.
await Deno.writeTextFile(
  `${bundleDir}/laufey-launch.json`,
  JSON.stringify({
    appId: "dev.denext.smoke",
    ...(backend === "cef" ? { customSchemes: [scheme] } : {}),
  }),
);
// DesktopPackageMeta; `secureStore` adds the secure store's libsecret.
const meta = {
  name: "smoke",
  version: "1.0.0",
  publisher: "denext",
  description: "denext runtime smoke app",
  identifier: "dev.denext.smoke",
  deepLinks: [],
  singleInstance: false,
  backend,
  secureStore: true,
};
await buildDesktopRpm({ meta, bundleDir, exe: "smoke", arch, out });
console.log(`built ${out}`);
