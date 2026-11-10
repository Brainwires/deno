// Copyright 2018-2026 the Deno authors. MIT license.

(function () {
const { core, primordials } = __bootstrap;
const { op_signal_bind, op_signal_poll, op_signal_unbind } = core.ops;
const {
  ArrayFrom,
  SafeSet,
  SafeSetIterator,
  SetPrototypeAdd,
  SetPrototypeDelete,
  SetPrototypeHas,
  TypeError,
} = primordials;

function bindSignal(signo) {
  return op_signal_bind(signo);
}

function pollSignal(rid) {
  const promise = op_signal_poll(rid);
  core.unrefOpPromise(promise);
  return promise;
}

function unbindSignal(rid) {
  op_signal_unbind(rid);
}

// Stores signal listeners and resource data. This has type of
// `Record<string, { rid: number | undefined, listeners: Set<() => void> }`
const signalData = { __proto__: null };

/** Gets the signal handlers and resource data of the given signal */
function getSignalData(signo) {
  return signalData[signo] ??
    (signalData[signo] = { rid: undefined, listeners: new SafeSet() });
}

function checkSignalListenerType(listener) {
  if (typeof listener !== "function") {
    throw new TypeError(
      `Signal listener must be a function. "${typeof listener}" is given.`,
    );
  }
}

function addSignalListener(signo, listener) {
  checkSignalListenerType(listener);

  const sigData = getSignalData(signo);
  SetPrototypeAdd(sigData.listeners, listener);

  if (!sigData.rid) {
    // If signal resource doesn't exist, create it.
    // The program starts listening to the signal
    sigData.rid = bindSignal(signo);
    loop(sigData);
  }
}

function removeSignalListener(signo, listener) {
  checkSignalListenerType(listener);

  const sigData = getSignalData(signo);
  SetPrototypeDelete(sigData.listeners, listener);

  if (sigData.listeners.size === 0 && sigData.rid) {
    unbindSignal(sigData.rid);
    sigData.rid = undefined;
  }
}

async function loop(sigData) {
  // Each loop serves the resource it was started for: removing the last
  // listener and adding one again from inside a listener binds a new resource
  // with its own loop, and this one must stop rather than poll that resource
  // too (which would deliver every later signal twice).
  const rid = sigData.rid;
  while (sigData.rid === rid) {
    if (await pollSignal(rid)) {
      return;
    }
    // The listeners at delivery: one added by a listener waits for the next
    // signal, as with EventEmitter.
    const listeners = ArrayFrom(new SafeSetIterator(sigData.listeners));
    for (let i = 0; i < listeners.length; i++) {
      if (SetPrototypeHas(sigData.listeners, listeners[i])) listeners[i]();
    }
  }
}

return { addSignalListener, removeSignalListener };
})();
