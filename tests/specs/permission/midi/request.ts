try {
  await navigator.requestMIDIAccess();
  console.log("granted");
} catch (error) {
  console.log(`${(error as Error).name}: ${(error as Error).message}`);
}
