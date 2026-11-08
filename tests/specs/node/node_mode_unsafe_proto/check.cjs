// Reads `({}).__proto__` and assigns `obj.__proto__`, as old CommonJS
// libraries do (pm2's dependencies call `this.set` after such an assignment).
module.exports = function check() {
  const read = ({}).__proto__ === Object.prototype;
  const proto = {
    set(key) {
      return `set ${key}`;
    },
  };
  const obj = {};
  obj.__proto__ = proto;
  const set = Object.getPrototypeOf(obj) === proto &&
    typeof obj.set === "function";
  return `read=${read} set=${set}`;
};
