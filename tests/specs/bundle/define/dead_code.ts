declare const FEATURE_NEW: boolean;

function newCheckout() {
  return "NEW_CHECKOUT_MARKER";
}

function legacyCheckout() {
  return "legacy";
}

console.log(FEATURE_NEW ? newCheckout() : legacyCheckout());
