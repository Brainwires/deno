export default {
  name: "noop",
  rules: {
    "noop": {
      create() {
        return {};
      },
    },
  },
} satisfies Deno.lint.Plugin;
