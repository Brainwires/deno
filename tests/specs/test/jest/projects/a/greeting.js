export const React = {
  createElement: (type, props, ...children) => ({ type, props, children }),
};

export function Greeting({ name }) {
  return <p className="greeting">Hello, {name}</p>;
}
