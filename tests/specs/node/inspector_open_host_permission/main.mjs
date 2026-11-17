import inspector from "node:inspector";

try {
  inspector.open(0, "inspector-host.invalid");
  console.log("opened");
  inspector.close();
} catch (err) {
  console.log(`${err.name}: ${err.message}`);
}
