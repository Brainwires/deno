// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Joyent and Node contributors. All rights reserved. MIT license.

(function () {
const { core, primordials } = __bootstrap;
// deno-lint-ignore camelcase
const async_wrap = core.loadExtScript(
  "ext:deno_node/internal_binding/async_wrap.ts",
);
const { ERR_ASYNC_CALLBACK, ERR_INVALID_ARG_TYPE, ERR_INVALID_ARG_VALUE } = core
  .loadExtScript(
    "ext:deno_node/internal/errors.ts",
  );
const { asyncIdSymbol, ownerSymbol } = core.loadExtScript(
  "ext:deno_node/internal_binding/symbols.ts",
);
const {
  ArrayPrototypeIncludes,
  ArrayPrototypeIndexOf,
  ArrayPrototypePush,
  ArrayPrototypePop,
  ArrayPrototypeSlice,
  ArrayPrototypeSplice,
  Error,
  ErrorCaptureStackTrace,
  FunctionPrototypeApply,
  ObjectKeys,
  ObjectPrototypeHasOwnProperty,
  Symbol,
} = primordials;
const {
  AsyncVariable,
  getAsyncContext,
  kNoAsyncContextRestore,
  setAsyncContext,
} = core;

interface ActiveHooks {
  array: AsyncHook[];
  // deno-lint-ignore camelcase
  call_depth: number;
  // deno-lint-ignore camelcase
  tmp_array: AsyncHook[] | null;
  // deno-lint-ignore camelcase
  tmp_fields: number[] | null;
}

// Properties in active_hooks are used to keep track of the set of hooks being
// executed in case another hook is enabled/disabled. The new set of hooks is
// then restored once the active set of hooks is finished executing.
// deno-lint-ignore camelcase
const active_hooks: ActiveHooks = {
  // Array of all AsyncHooks that will be iterated whenever an async event
  // fires. Using var instead of (preferably const) in order to assign
  // active_hooks.tmp_array if a hook is enabled/disabled during hook
  // execution.
  array: [],
  // Use a counter to track nested calls of async hook callbacks and make sure
  // the active_hooks.array isn't altered mid execution.
  // deno-lint-ignore camelcase
  call_depth: 0,
  // Use to temporarily store and updated active_hooks.array if the user
  // enables or disables a hook while hooks are being processed. If a hook is
  // enabled() or disabled() during hook execution then the current set of
  // active hooks is duplicated and set equal to active_hooks.tmp_array. Any
  // subsequent changes are on the duplicated array. When all hooks have
  // completed executing active_hooks.tmp_array is assigned to
  // active_hooks.array.
  // deno-lint-ignore camelcase
  tmp_array: null,
  // Keep track of the field counts held in active_hooks.tmp_array. Because the
  // async_hook_fields can't be reassigned, store each uint32 in an array that
  // is written back to async_hook_fields when active_hooks.array is restored.
  // deno-lint-ignore camelcase
  tmp_fields: null,
};

const registerDestroyHook = async_wrap.registerDestroyHook;
const {
  async_hook_fields,
  // deno-lint-ignore camelcase
  asyncIdFields: async_id_fields,
  newAsyncId,
  constants,
} = async_wrap;

// The current execution context, as Node keeps it: the ids live in
// async_id_fields, and entering a context saves the previous pair on a stack.
// The main script runs as async id 1 (Node's bootstrap context).
const { UidFields } = async_wrap;
async_id_fields[UidFields.kExecutionAsyncId] = 1;
const asyncIdsStack: number[] = [];

function executionAsyncId(): number {
  return async_id_fields[UidFields.kExecutionAsyncId];
}

function triggerAsyncId(): number {
  return async_id_fields[UidFields.kTriggerAsyncId];
}

function pushAsyncContext(asyncId: number, triggerAsyncId: number) {
  ArrayPrototypePush(
    asyncIdsStack,
    async_id_fields[UidFields.kExecutionAsyncId],
    async_id_fields[UidFields.kTriggerAsyncId],
  );
  async_hook_fields[kStackLength] = asyncIdsStack.length / 2;
  async_id_fields[UidFields.kExecutionAsyncId] = asyncId;
  async_id_fields[UidFields.kTriggerAsyncId] = triggerAsyncId;
}

function popAsyncContext(): boolean {
  if (asyncIdsStack.length === 0) return false;
  async_id_fields[UidFields.kTriggerAsyncId] = ArrayPrototypePop(
    asyncIdsStack,
  )!;
  async_id_fields[UidFields.kExecutionAsyncId] = ArrayPrototypePop(
    asyncIdsStack,
  )!;
  async_hook_fields[kStackLength] = asyncIdsStack.length / 2;
  return asyncIdsStack.length > 0;
}

// Per-async-context "current resource" tracked via the AsyncVariable
// machinery (V8 ContinuationPreservedEmbedderData). This propagates across
// promises and await transitions automatically. The top-level resource is a
// shared singleton used before any specific resource has been entered.
// deno-lint-ignore no-explicit-any
const topLevelResource: any = { __proto__: null };
// deno-lint-ignore no-explicit-any
const executionResourceVariable: any = new AsyncVariable();

// deno-lint-ignore no-explicit-any
function executionAsyncResource(): any {
  const r = executionResourceVariable.get();
  return r === undefined ? topLevelResource : r;
}

// Enter a new "current resource" scope. The returned value is the previous
// async context snapshot that must be restored by exitAsyncResource.
// deno-lint-ignore no-explicit-any
function enterAsyncResource(resource: any): any {
  return executionResourceVariable.enter(resource);
}

// deno-lint-ignore no-explicit-any
function exitAsyncResource(previousContext: any): void {
  setAsyncContext(previousContext);
}

// deno-lint-ignore no-explicit-any
function enterAsyncResourceIfActive(resource: any): any {
  if (active_hooks.array.length > 0) {
    return executionResourceVariable.enter(resource);
  }
  return executionResourceVariable.enterIfActive(resource);
}

// deno-lint-ignore no-explicit-any
function exitAsyncResourceIfActive(previousContext: any): void {
  if (previousContext !== kNoAsyncContextRestore) {
    setAsyncContext(previousContext);
    return;
  }

  const currentContext = getAsyncContext();
  if (
    currentContext !== null &&
    currentContext !== undefined &&
    ObjectKeys(currentContext).length > 0
  ) {
    setAsyncContext(undefined);
  }
}

// Calls `symbol`'s callback of every active hook (a hook created with
// `trackPromises: false` sits out promise events), keeping the set of hooks
// fixed while they run.
function emitHook(
  symbol: symbol,
  asyncId: number,
  isPromiseHook = false,
): void {
  active_hooks.call_depth += 1;
  try {
    const hooks = active_hooks.array;
    for (let i = 0; i < hooks.length; i++) {
      const hook = hooks[i];
      if (isPromiseHook && hook[kNoPromiseHook]) continue;
      if (typeof hook[symbol] === "function") hook[symbol](asyncId);
    }
  } finally {
    active_hooks.call_depth -= 1;
  }
  if (active_hooks.call_depth === 0 && active_hooks.tmp_array !== null) {
    restoreActiveHooks();
  }
}

// Emit functions that work with the internal hook system
function emitBefore(
  asyncId: number,
  triggerAsyncId = 0,
  _resource?: unknown,
  isPromiseHook = false,
): void {
  pushAsyncContext(asyncId, triggerAsyncId);
  if (!hasHooks(kBefore)) return;
  try {
    emitHook(before_symbol, asyncId, isPromiseHook);
  } catch (e) {
    // Clean up stack corruption on hook errors (Node.js pattern)
    popAsyncContext();
    throw e;
  }
}

function emitAfter(asyncId: number): void {
  try {
    if (hasHooks(kAfter)) emitHook(after_symbol, asyncId);
  } finally {
    // Always pop stack even if hooks throw (Node.js pattern)
    popAsyncContext();
  }
}

function emitDestroy(asyncId: number): void {
  if (hasHooks(kDestroy)) emitHook(destroy_symbol, asyncId);
}
const {
  kInit,
  kBefore,
  kAfter,
  kDestroy,
  kPromiseResolve,
  kTotals,
  kCheck,
  kStackLength,
} = constants;

// deno-lint-ignore camelcase
const resource_symbol = Symbol("resource");
// Alias to the same symbol used by `internal_binding/symbols.ts` so that
// `socket[asyncIdSymbol]` (set in net.ts/dgram.ts) and
// `socket[require('internal/async_hooks').symbols.async_id_symbol]`
// (read by Node test fixtures) refer to the same slot on objects.
// deno-lint-ignore camelcase
const async_id_symbol = asyncIdSymbol;
// deno-lint-ignore camelcase
const trigger_async_id_symbol = Symbol("trigger_async_id");
// deno-lint-ignore camelcase
const init_symbol = Symbol("init");
// deno-lint-ignore camelcase
const before_symbol = Symbol("before");
// deno-lint-ignore camelcase
const after_symbol = Symbol("after");
// deno-lint-ignore camelcase
const destroy_symbol = Symbol("destroy");
// deno-lint-ignore camelcase
const promise_resolve_symbol = Symbol("promiseResolve");
// Set on a hook created with `trackPromises: false`.
const kNoPromiseHook = Symbol("kNoPromiseHook");

const symbols = {
  // deno-lint-ignore camelcase
  async_id_symbol,
  // deno-lint-ignore camelcase
  trigger_async_id_symbol,
  // deno-lint-ignore camelcase
  init_symbol,
  // deno-lint-ignore camelcase
  before_symbol,
  // deno-lint-ignore camelcase
  after_symbol,
  // deno-lint-ignore camelcase
  destroy_symbol,
  // deno-lint-ignore camelcase
  promise_resolve_symbol,
};

// deno-lint-ignore no-explicit-any
function lookupPublicResource(resource: any) {
  if (typeof resource !== "object" || resource === null) return resource;
  // TODO(addaleax): Merge this with owner_symbol and use it across all
  // AsyncWrap instances.
  const publicResource = resource[resource_symbol];
  if (publicResource !== undefined) {
    return publicResource;
  }
  return resource;
}

// Used by C++ to call all init() callbacks. Because some state can be setup
// from C++ there's no need to perform all the same operations as in
// emitInitScript.
function emitInitNative(
  asyncId: number,
  // deno-lint-ignore no-explicit-any
  type: any,
  triggerAsyncId: number,
  // deno-lint-ignore no-explicit-any
  resource: any,
  isPromiseHook = false,
) {
  active_hooks.call_depth += 1;
  resource = lookupPublicResource(resource);
  // Use a single try/catch for all hooks to avoid setting up one per iteration.
  try {
    for (let i = 0; i < active_hooks.array.length; i++) {
      if (isPromiseHook && active_hooks.array[i][kNoPromiseHook]) continue;
      if (typeof active_hooks.array[i][init_symbol] === "function") {
        active_hooks.array[i][init_symbol](
          asyncId,
          type,
          triggerAsyncId,
          resource,
        );
      }
    }
  } catch (e) {
    throw e;
  } finally {
    active_hooks.call_depth -= 1;
  }

  // Hooks can only be restored if there have been no recursive hook calls.
  // Also the active hooks do not need to be restored if enable()/disable()
  // weren't called during hook execution, in which case active_hooks.tmp_array
  // will be null.
  if (active_hooks.call_depth === 0 && active_hooks.tmp_array !== null) {
    restoreActiveHooks();
  }
}

function getHookArrays(): [AsyncHook[], number[] | Uint32Array] {
  if (active_hooks.call_depth === 0) {
    return [active_hooks.array, async_hook_fields];
  }
  // If this hook is being enabled while in the middle of processing the array
  // of currently active hooks then duplicate the current set of active hooks
  // and store this there. This shouldn't fire until the next time hooks are
  // processed.
  if (active_hooks.tmp_array === null) {
    storeActiveHooks();
  }
  return [active_hooks.tmp_array!, active_hooks.tmp_fields!];
}

function storeActiveHooks() {
  active_hooks.tmp_array = ArrayPrototypeSlice(active_hooks.array);
  // Don't want to make the assumption that kInit to kDestroy are indexes 0 to
  // 4. So do this the long way.
  active_hooks.tmp_fields = [];
  copyHooks(active_hooks.tmp_fields, async_hook_fields);
}

function copyHooks(
  destination: number[] | Uint32Array,
  source: number[] | Uint32Array,
) {
  destination[kInit] = source[kInit];
  destination[kBefore] = source[kBefore];
  destination[kAfter] = source[kAfter];
  destination[kDestroy] = source[kDestroy];
  destination[kPromiseResolve] = source[kPromiseResolve];
}

// Then restore the correct hooks array in case any hooks were added/removed
// during hook callback execution.
function restoreActiveHooks() {
  active_hooks.array = active_hooks.tmp_array!;
  copyHooks(async_hook_fields, active_hooks.tmp_fields!);

  active_hooks.tmp_array = null;
  active_hooks.tmp_fields = null;
}

// Promises as async resources (type PROMISE), through V8's promise hooks:
// Node's lib/internal/async_hooks.js.
function getOrSetAsyncId(object: object): number {
  if (ObjectPrototypeHasOwnProperty(object, async_id_symbol)) {
    // deno-lint-ignore no-explicit-any
    return (object as any)[async_id_symbol];
  }
  // deno-lint-ignore no-explicit-any
  return (object as any)[async_id_symbol] = newAsyncId();
}

// deno-lint-ignore no-explicit-any
function trackPromise(promise: any, parent?: any) {
  if (promise[async_id_symbol]) return;
  // Get trigger id from parent async id before making the async id of the
  // child so if a new one must be made it will be lower than the child.
  const triggerAsyncId = parent
    ? getOrSetAsyncId(parent)
    : getDefaultTriggerAsyncId();
  promise[async_id_symbol] = newAsyncId();
  promise[trigger_async_id_symbol] = triggerAsyncId;
}

// A promise the runtime creates for itself (an op's, a lazy module load's)
// is no async resource: Node's equivalents create none.
const kInternalPromise = Symbol("kInternalPromise");

// deno-lint-ignore no-explicit-any
function promiseInitHook(promise: any, parent?: any) {
  if (core.isCreatingInternalPromise() || promiseFromRuntime()) {
    promise[kInternalPromise] = true;
    return;
  }
  trackPromise(promise, parent);
  if (!hasHooks(kInit)) return;
  emitInitNative(
    promise[async_id_symbol],
    "PROMISE",
    promise[trigger_async_id_symbol],
    promise,
    true,
  );
}

// deno-lint-ignore no-explicit-any
function promiseTrackingInitHook(promise: any, parent?: any) {
  if (core.isCreatingInternalPromise()) {
    promise[kInternalPromise] = true;
    return;
  }
  trackPromise(promise, parent);
}

// deno-lint-ignore no-explicit-any
function promiseBeforeHook(promise: any) {
  if (promise[kInternalPromise]) return;
  trackPromise(promise);
  emitBefore(
    promise[async_id_symbol],
    promise[trigger_async_id_symbol],
    promise,
    true,
  );
}

// deno-lint-ignore no-explicit-any
function promiseAfterHook(promise: any) {
  if (promise[kInternalPromise]) return;
  trackPromise(promise);
  const asyncId = promise[async_id_symbol];
  if (hasHooks(kAfter)) emitHook(after_symbol, asyncId, true);
  if (asyncId === executionAsyncId()) {
    // Not true if async_hooks was enabled during the promise callback: then
    // its `before` was never seen.
    popAsyncContext();
  }
}

// Whether the runtime itself, not JS, created or settled a promise: nothing
// but the promise hook machinery is on the stack. A module's evaluation
// promise is one (V8 makes it, and Deno runs a CommonJS main and builtins
// through modules, where Node's CommonJS main has none). JS that creates or
// settles a promise is on the stack, even when it does so through a native
// builtin.
function promiseFromRuntime(): boolean {
  // deno-lint-ignore deno-internal/prefer-primordials -- the stack trace API's own settings
  const prepare = Error.prepareStackTrace;
  // deno-lint-ignore deno-internal/prefer-primordials -- as above
  const limit = Error.stackTraceLimit;
  // Count the call sites instead of formatting them: cheap, and it can't fail
  // the way formatting does near a stack overflow.
  // deno-lint-ignore deno-internal/prefer-primordials -- as above
  Error.prepareStackTrace = countCallSites;
  // this function, the hook, the dispatcher: a fourth frame is the JS that
  // made or settled the promise
  // deno-lint-ignore deno-internal/prefer-primordials -- as above
  Error.stackTraceLimit = 4;
  try {
    const holder: { stack?: unknown } = {};
    ErrorCaptureStackTrace(holder);
    return (holder.stack as number) < 4;
  } catch {
    return false;
  } finally {
    // deno-lint-ignore deno-internal/prefer-primordials -- as above
    Error.prepareStackTrace = prepare;
    // deno-lint-ignore deno-internal/prefer-primordials -- as above
    Error.stackTraceLimit = limit;
  }
}

function countCallSites(_error: unknown, callSites: unknown[]): number {
  return callSites.length;
}

// deno-lint-ignore no-explicit-any
function promiseResolveHook(promise: any) {
  if (promise[kInternalPromise]) return;
  if (promise[async_id_symbol] === undefined && promiseFromRuntime()) {
    promise[kInternalPromise] = true;
    return;
  }
  trackPromise(promise);
  emitHook(promise_resolve_symbol, promise[async_id_symbol], true);
}

let wantPromiseHook = false;
let stopPromiseHook: (() => void) | undefined;
function updatePromiseHookMode() {
  wantPromiseHook = true;
  if (stopPromiseHook) stopPromiseHook();
  const { promiseHooks } = core.loadExtScript("ext:deno_node/v8.ts");
  stopPromiseHook = promiseHooks.createHook({
    init: hasHooks(kInit) ? promiseInitHook : promiseTrackingInitHook,
    before: promiseBeforeHook,
    after: promiseAfterHook,
    settled: hasHooks(kPromiseResolve) ? promiseResolveHook : undefined,
  });
}

function disablePromiseHookIfNecessary() {
  if (!wantPromiseHook && stopPromiseHook) {
    stopPromiseHook();
    stopPromiseHook = undefined;
  }
}

function enableHooks() {
  async_hook_fields[kCheck] += 1;
}

function disableHooks() {
  async_hook_fields[kCheck] -= 1;

  wantPromiseHook = false;

  // Delay the call to `disablePromiseHook()` because we might currently be
  // between the `before` and `after` calls of a Promise.
  globalThis.queueMicrotask(disablePromiseHookIfNecessary);
}

// Return the triggerAsyncId meant for the constructor calling it. It's up to
// the user to safeguard this call and make sure it's zero'd out when the
// constructor is complete.
function getDefaultTriggerAsyncId() {
  const defaultTriggerAsyncId =
    async_id_fields[UidFields.kDefaultTriggerAsyncId];
  // If defaultTriggerAsyncId isn't set, use the executionAsyncId
  if (defaultTriggerAsyncId < 0) {
    return async_id_fields[UidFields.kExecutionAsyncId];
  }
  return defaultTriggerAsyncId;
}

function defaultTriggerAsyncIdScope(
  triggerAsyncId: number | undefined,
  // deno-lint-ignore no-explicit-any
  block: (...arg: any[]) => void,
  ...args: unknown[]
) {
  if (triggerAsyncId === undefined) {
    return FunctionPrototypeApply(block, null, args);
  }
  // CHECK(NumberIsSafeInteger(triggerAsyncId))
  // CHECK(triggerAsyncId > 0)
  const oldDefaultTriggerAsyncId =
    async_id_fields[UidFields.kDefaultTriggerAsyncId];
  async_id_fields[UidFields.kDefaultTriggerAsyncId] = triggerAsyncId;

  try {
    return FunctionPrototypeApply(block, null, args);
  } finally {
    async_id_fields[UidFields.kDefaultTriggerAsyncId] =
      oldDefaultTriggerAsyncId;
  }
}

function hasHooks(key: number) {
  return async_hook_fields[key] > 0;
}

function enabledHooksExist() {
  return active_hooks.array.length > 0;
}

function hasAsyncIdStack() {
  return hasHooks(kStackLength);
}

type Fn = (...args: unknown[]) => unknown;

class AsyncHook {
  [init_symbol]: Fn;
  [before_symbol]: Fn;
  [after_symbol]: Fn;
  [destroy_symbol]: Fn;
  [promise_resolve_symbol]: Fn;
  [kNoPromiseHook]: boolean;

  constructor({
    init,
    before,
    after,
    destroy,
    promiseResolve,
    trackPromises,
  }: {
    init: Fn;
    before: Fn;
    after: Fn;
    destroy: Fn;
    promiseResolve: Fn;
    trackPromises?: boolean;
  }) {
    if (init !== undefined && typeof init !== "function") {
      throw new ERR_ASYNC_CALLBACK("hook.init");
    }
    if (before !== undefined && typeof before !== "function") {
      throw new ERR_ASYNC_CALLBACK("hook.before");
    }
    if (after !== undefined && typeof after !== "function") {
      throw new ERR_ASYNC_CALLBACK("hook.after");
    }
    if (destroy !== undefined && typeof destroy !== "function") {
      throw new ERR_ASYNC_CALLBACK("hook.destroy");
    }
    if (promiseResolve !== undefined && typeof promiseResolve !== "function") {
      throw new ERR_ASYNC_CALLBACK("hook.promiseResolve");
    }
    if (trackPromises !== undefined && typeof trackPromises !== "boolean") {
      throw new ERR_INVALID_ARG_TYPE("trackPromises", "boolean", trackPromises);
    }

    this[init_symbol] = init;
    this[before_symbol] = before;
    this[after_symbol] = after;
    this[destroy_symbol] = destroy;
    this[promise_resolve_symbol] = promiseResolve;
    if (trackPromises === false) {
      if (promiseResolve) {
        throw new ERR_INVALID_ARG_VALUE(
          "trackPromises",
          trackPromises,
          "must not be false when promiseResolve is enabled",
        );
      }
      this[kNoPromiseHook] = true;
    } else {
      // Default to tracking promises for now.
      this[kNoPromiseHook] = false;
    }
  }

  enable() {
    // The set of callbacks for a hook should be the same regardless of whether
    // enable()/disable() are run during their execution. The following
    // references are reassigned to the tmp arrays if a hook is currently being
    // processed.
    // deno-lint-ignore camelcase
    const { 0: hooks_array, 1: hook_fields } = getHookArrays();

    // Each hook is only allowed to be added once.
    if (ArrayPrototypeIncludes(hooks_array, this)) {
      return this;
    }

    // deno-lint-ignore camelcase
    const prev_kTotals = hook_fields[kTotals];

    // createHook() has already enforced that the callbacks are all functions,
    // so here simply increment the count of whether each callbacks exists or
    // not.
    hook_fields[kTotals] = hook_fields[kInit] += +!!this[init_symbol];
    hook_fields[kTotals] += hook_fields[kBefore] += +!!this[before_symbol];
    hook_fields[kTotals] += hook_fields[kAfter] += +!!this[after_symbol];
    hook_fields[kTotals] += hook_fields[kDestroy] += +!!this[destroy_symbol];
    hook_fields[kTotals] += hook_fields[kPromiseResolve] +=
      +!!this[promise_resolve_symbol];
    ArrayPrototypePush(hooks_array, this);

    if (prev_kTotals === 0 && hook_fields[kTotals] > 0) {
      enableHooks();
    }

    if (!this[kNoPromiseHook]) {
      updatePromiseHookMode();
    }

    return this;
  }

  disable() {
    // deno-lint-ignore camelcase
    const { 0: hooks_array, 1: hook_fields } = getHookArrays();

    const index = ArrayPrototypeIndexOf(hooks_array, this);
    if (index === -1) {
      return this;
    }

    // deno-lint-ignore camelcase
    const prev_kTotals = hook_fields[kTotals];

    hook_fields[kTotals] = hook_fields[kInit] -= +!!this[init_symbol];
    hook_fields[kTotals] += hook_fields[kBefore] -= +!!this[before_symbol];
    hook_fields[kTotals] += hook_fields[kAfter] -= +!!this[after_symbol];
    hook_fields[kTotals] += hook_fields[kDestroy] -= +!!this[destroy_symbol];
    hook_fields[kTotals] += hook_fields[kPromiseResolve] -=
      +!!this[promise_resolve_symbol];
    ArrayPrototypeSplice(hooks_array, index, 1);

    if (prev_kTotals > 0 && hook_fields[kTotals] === 0) {
      disableHooks();
    }

    return this;
  }
}

return {
  asyncIdSymbol,
  ownerSymbol,
  newAsyncId,
  emitInit: emitInitNative,
  constants,
  executionAsyncId,
  triggerAsyncId,
  executionAsyncResource,
  enterAsyncResource,
  exitAsyncResource,
  enterAsyncResourceIfActive,
  exitAsyncResourceIfActive,
  emitBefore,
  emitAfter,
  emitDestroy,
  getDefaultTriggerAsyncId,
  defaultTriggerAsyncIdScope,
  enabledHooksExist,
  hasHooks,
  hasAsyncIdStack,
  AsyncHook,
  registerDestroyHook,
  async_id_symbol,
  trigger_async_id_symbol,
  init_symbol,
  before_symbol,
  after_symbol,
  destroy_symbol,
  promise_resolve_symbol,
  symbols,
};
})();
