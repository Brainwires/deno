// JSX in a .js test and source file: Jest's default transform (babel-jest)
// compiles it.
import { Greeting, React } from "./greeting.js";

test("parses JSX in .js files", () => {
  const element = <Greeting name="jest" />;
  expect(element.type(element.props)).toEqual({
    type: "p",
    props: { className: "greeting" },
    children: ["Hello, ", "jest"],
  });
  expect(React).toBeDefined();
});
