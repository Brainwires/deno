// Copyright 2018-2026 the Deno authors. MIT license.

// Fallback types for `vitest/config` under `deno test`, used only when the
// vitest package itself is not installed.

// deno-lint-ignore-file no-explicit-any

export declare function defineConfig<T>(config: T): T;
export declare function defineProject<T>(config: T): T;
export declare function defineWorkspace<T>(config: T): T;
export declare function mergeConfig(defaults: any, overrides: any): any;
export declare function loadEnv(
  mode: string,
  envDir: string,
  prefixes?: string | string[],
): Record<string, string>;
export declare function normalizePath(id: string): string;
export declare const configDefaults: any;
export declare const coverageConfigDefaults: any;
export declare const defaultInclude: string[];
export declare const defaultExclude: string[];
export declare const defaultBrowserPort: number;
export type UserConfig = any;
export type ViteUserConfig = any;
export type TestProjectInlineConfiguration = any;
