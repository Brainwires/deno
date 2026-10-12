export default {
  test: (value) =>
    value !== null && typeof value === "object" && value.$$typeof === "money",
  serialize: (value) => `Money<${value.amount}>`,
};
