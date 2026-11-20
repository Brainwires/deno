// Each step starts from the event before it, so the order holds on a busy machine: fixed
// timers (a catch at 10 ms, "Success" at 1 s) could both come due in one turn and print
// "Success" before rejectionhandled.
globalThis.addEventListener("unhandledrejection", (event) => {
  console.log("unhandledrejection", event.reason, event.promise);
  event.preventDefault();
  setTimeout(() => {
    a.catch(() => console.log("Added catch handler to the promise"));
  }, 10);
});

globalThis.addEventListener("rejectionhandled", (event) => {
  console.log("rejectionhandled", event.reason, event.promise);
  setTimeout(() => console.log("Success"), 0);
});

const a = Promise.reject(1);
