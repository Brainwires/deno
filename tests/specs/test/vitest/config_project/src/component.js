// JSX in a .js file: the config's `esbuild.loader` is "jsx" for it.
const React = {
  createElement: (type, props, ...children) => ({ type, props, children }),
};

export const element = <b title="x">bold</b>;
