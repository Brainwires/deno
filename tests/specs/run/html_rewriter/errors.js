function show(label, fn) {
  try {
    fn();
    console.log(label, "no error");
  } catch (error) {
    console.log(label, error.constructor.name, error.message);
  }
}
async function showAsync(label, fn) {
  try {
    await fn();
    console.log(label, "no error");
  } catch (error) {
    console.log(label, error.constructor.name, error.message);
  }
}

// A sync handler's exception is rethrown by a string transform, unchanged.
class MyError extends Error {}
show("sync throw", () =>
  new HTMLRewriter()
    .on("p", {
      element() {
        throw new MyError("from element");
      },
    })
    .transform("<p>x</p>"));

// An invalid selector throws from on(), not from transform().
show("bad selector", () => new HTMLRewriter().on("p[", {}));
show("unsupported selector", () => new HTMLRewriter().on("p:hover", {}));

// A token can't be used once its handler has returned.
let saved;
new HTMLRewriter()
  .on("p", { element: (e) => void (saved = e) })
  .transform("<p>x</p>");
show("stale token", () => saved.setAttribute("a", "b"));

// A string transform is synchronous: an async handler is refused.
show("async on string", () =>
  new HTMLRewriter()
    .on("p", { async element() {} })
    .transform("<p>x</p>"));

// onEndTag on a void element has no end tag to wait for.
show("void onEndTag", () =>
  new HTMLRewriter()
    .on("br", { element: (e) => e.onEndTag(() => {}) })
    .transform("<br>"));

// An async handler's rejection rejects the body read with the same error.
const expected = new MyError("from async element");
await showAsync("async reject", async () => {
  const response = new HTMLRewriter()
    .on("p", {
      async element() {
        await new Promise((resolve) => setTimeout(resolve, 1));
        throw expected;
      },
    })
    .transform(new Response("<div><p>x</p></div>"));
  try {
    await response.text();
  } catch (error) {
    console.log("same error", error === expected);
    throw error;
  }
});

// A source stream error reaches the reader too.
await showAsync("source error", async () => {
  const response = new HTMLRewriter()
    .on("p", { element() {} })
    .transform(
      new Response(
        new ReadableStream({
          pull(controller) {
            controller.error(new RangeError("source broke"));
          },
        }),
      ),
    );
  await response.text();
});

// The rewriter is still usable after a failed transform.
console.log(
  new HTMLRewriter()
    .on("p", { element: (e) => e.setInnerContent("ok") })
    .transform("<p>x</p>"),
);
