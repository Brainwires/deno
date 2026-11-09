self.onmessage = () => {
  const states = {};
  for (const name of Deno.permissions.names) {
    states[name] = Deno.permissions.querySync({ name }).state;
  }
  self.postMessage(states);
  self.close();
};
