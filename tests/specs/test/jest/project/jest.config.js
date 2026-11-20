/** A Jest config: deno test maps it onto its Vitest layer. */
export default {
  displayName: "spec",
  roots: ["<rootDir>/src"],
  testMatch: ["**/__tests__/**/*.(spec|test).ts"],
  testPathIgnorePatterns: ["/node_modules/", "/ignored/"],
  moduleNameMapper: {
    "^@lib/(.*)$": "<rootDir>/lib/$1",
    "\\.css$": "<rootDir>/style_mock.js",
    "^mapped-target$": "<rootDir>/lib/mapped_target.cjs",
  },
  moduleDirectories: ["node_modules", "shared"],
  setupFiles: ["<rootDir>/setup.ts"],
  setupFilesAfterEnv: ["<rootDir>/setup_after_env.ts"],
  testEnvironment: "node",
  globals: { __APP__: { name: "jest" } },
  restoreMocks: true,
  // Not the 5000 default, so the mapping shows; generous, since it bounds
  // every test here and a loaded CI machine can take seconds over one.
  testTimeout: 61234,
  transform: {
    "^.+\\.tsx?$": "ts-jest",
    "^.+\\.svg$": "<rootDir>/svg_transform.js",
  },
  snapshotSerializers: ["<rootDir>/serializer.js"],
};
