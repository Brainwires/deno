console.log(typeof globalThis.HTMLRewriter);
if (typeof HTMLRewriter === "function") {
  console.log(
    new HTMLRewriter()
      .on("b", { element: (e) => e.setInnerContent("on") })
      .transform("<b>off</b>"),
  );
}
