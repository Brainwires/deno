// Under the direction-specific net flags a prompt names the direction.
const connect = await Deno.permissions.request({
  name: "net-connect",
  host: "example.com",
});
console.log(`net-connect: ${connect.state}`);
const listen = await Deno.permissions.request({
  name: "net-listen",
  host: "127.0.0.1:8080",
});
console.log(`net-listen: ${listen.state}`);
