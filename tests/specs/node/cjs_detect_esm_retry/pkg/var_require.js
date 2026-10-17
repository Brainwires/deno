// var may redeclare a wrapper parameter.
var exports = module.exports;
exports.a = 1;
console.log("cjs", module.exports.a);
