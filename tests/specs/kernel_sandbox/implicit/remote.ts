// a remote module and an npm package, fetched by the first image before it
// confines itself: the confined run needs no network grant for them
import { printHello } from "http://localhost:4545/subdir/print_hello.ts";
import { getValue, setValue } from "npm:@denotest/esm-basic";
printHello();
setValue(5);
console.log(getValue());
