const devices = Deno.permissions.querySync({ name: "midi" });
const sysex = Deno.permissions.querySync({ name: "midi", sysex: true });
console.log(`devices: ${devices.state}`);
console.log(`sysex: ${sysex.state}`);
