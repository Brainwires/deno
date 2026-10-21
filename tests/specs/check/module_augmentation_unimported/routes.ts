export type Routes = "/";

// Augments a module no file of the checked graph imports.
declare module "mylib" {
  interface Registry {
    routes: Routes;
  }
}
