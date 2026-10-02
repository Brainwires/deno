// Every call is typed only: this file is checked, never run.
const d = Deno.desktop;

async function authSession(): Promise<string | null> {
  const caps: Deno.desktop.AuthSessionCapabilities = d.authSession
    .capabilities();
  if (!caps.supported) return null;
  setTimeout(() => {
    const cancelled: boolean = d.authSession.cancel();
    console.log(cancelled);
  }, 60_000);
  try {
    const { url } = await d.authSession.start({
      url: "https://idp.example/authorize",
      callbackScheme: "myapp",
      ephemeral: true,
    });
    return url;
  } catch (e) {
    const code: Deno.desktop.AuthSessionErrorCode =
      (e as Deno.desktop.AuthSessionError).code;
    return code;
  }
}

async function updater() {
  const u = d.updater;
  if (u.status().trial) {
    const confirmed: boolean = u.confirm();
    console.log(confirmed);
  }
  const found = await u.check("https://updates.example/app.json");
  if (found.available) {
    const { version, size } = await u.download({
      onProgress: (p) => console.log(p.transferred / p.total),
    });
    console.log(version, size);
    const staged = await u.stage({ allowUnsignedDev: false });
    console.log(staged.signature.identity);
    const { quitting }: { quitting: boolean } = u.applyAndRelaunch();
    console.log(quitting);
  }
  const code: Deno.desktop.AppUpdateErrorCode = "not_staged";
  console.log(code);
}

async function runOnMainThread(fn: Deno.UnsafeFnPointer<any>) {
  const value: bigint = await d.runOnMainThread(fn, null);
  console.log(value);
}

async function system() {
  const passkeyCaps = await d.passkeys.capabilities();
  console.log(passkeyCaps);
  const credential: string = await d.passkeys.get("{}");
  console.log(credential);
  await d.shortcuts.register("CommandOrControl+Shift+Space");
  const tag: string = await d.notifications.schedule({
    title: "Reminder",
    tag: "r1",
    at: new Date(Date.now() + 60_000),
  });
  d.notifications.cancel(tag);
  const screens = d.screens();
  const primary: typeof screens[number] | null = d.getPrimaryScreen();
  console.log(screens, primary, d.windowCapabilities(), d.menuCapabilities());
  const quitting: boolean = d.quit();
  console.log(quitting, d.launchUrls, d.launchFiles);
  const owner = await d.getSchemeOwner("myapp");
  console.log(owner);
}

export { authSession, runOnMainThread, system, updater };
