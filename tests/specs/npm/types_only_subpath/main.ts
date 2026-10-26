// `typesonly/types` exists only as `@types/typesonly/types.d.ts` (as
// `supertest/types` does). Every use of `App` is a type, so the import is
// elided, as tsc does.
import { App } from "typesonly/types";
import { value } from "typesonly";

const app: App = { name: value };
console.log(app.name);
